//! Multi-window read cache (byte-budget LRU + network-style sequential fill).
//!
//! mediabunny `UrlSource` defaults: `maxCacheSize` **64 MiB**,
//! `prefetchProfile: network` (grow sequential regions up to +8 MiB/step;
//! trigger when a read crosses `thresholdPoint` near the region end).
//! mediabunny `BlobSource`: `ReadOrchestrator` with `maxCacheSize ?? 8 MiB`
//! (`src/source.ts` BlobSource) — JS already holds the Blob; orch is a small
//! **JS** window; packets borrow `Uint8Array` (no second full-file mirror).
//!
//! **Budgets (via [`CachedSource::with_fetch_map`]):**
//! - **Url** (`network_min_load`): [`DEFAULT_CACHE_BUDGET`] **512 MiB** (we keep
//!   more than mediabunny’s 64 MiB — high-RTT `/proxy` thrash).
//! - **Blob / Memory**: [`LOCAL_CACHE_BUDGET`] **8 MiB** — match mediabunny
//!   BlobSource size; **no** bg quantum / margin / explicit multi-MiB
//!   `prefetch` into WASM (sample `want` only + `FetchMap`). Copying 8 MiB
//!   windows on top of the browser Blob skip-stormed ChG full VO Play.
//!
//! Fill quantum 8 MiB (URL); sequential misses extend like
//! `PREFETCH_PROFILES.network`; **hit-path** margin prefetch loads the next
//! extent without blocking the consumer (**URL only**).
//! **Sample-sized misses** on the critical path:
//! - **UrlSource** (`network_min_load`): floor at mediabunny
//!   `URL_SOURCE_MIN_LOAD_AMOUNT` (0.5 MiB), rest of quantum in bg — bare `want`
//!   caused a Range-per-sample RTT storm on cold gray.
//! - **Blob / Memory**: blocking `want` only; **no** bg quantum finish.
//!
//! Awaiting a full 8 MiB Range before returning AAC/NAL underran first-play A/V
//! (replay was fine = cache hit). `mediabunny: ReadOrchestrator` resolves pending
//! slices when their bytes arrive, not when the outer prefetch hole completes.
//! Do **not** chain fills to budget. `FetchMap` mirrors resident windows.

use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::error::MediaError;

use super::fetch_map::FetchMap;
use super::{MediaSource, ReadFuture};

/// Fill quantum / max extension step / right-margin (mediabunny network
/// `maxExtensionAmount`).
pub const DEFAULT_CACHE_SIZE: usize = 8 * 1024 * 1024;

/// Total resident bytes before LRU eviction for **network** sources.
///
/// mediabunny `UrlSource` default is 64 MiB; we keep more — this is WASM heap
/// RAM, not GPU memory, and GitHub-Release RTT makes under-buffering painful.
/// Local Blob/Memory use [`LOCAL_CACHE_BUDGET`] instead.
pub const DEFAULT_CACHE_BUDGET: usize = 512 * 1024 * 1024;

/// Resident budget for local Blob/Memory ([`MediaSource::network_min_load`] false).
///
/// `mediabunny: src/source.ts BlobSource` — `maxCacheSize ?? (8 * 2**20)`.
pub const LOCAL_CACHE_BUDGET: usize = DEFAULT_CACHE_SIZE;

/// mediabunny `URL_SOURCE_MIN_LOAD_AMOUNT` (0.5 MiB).
const MIN_LOAD: usize = 512 * 1024;

/// mediabunny network start padding (`2**16`).
const SEQ_START_PAD: u64 = 64 * 1024;

/// How far past a window end still counts as sequential continuation.
/// Keep small (mdat padding / A‑V interleave), not FetchMap merge gaps.
const SEQ_SLACK: u64 = 4 * 1024;

/// ISOBMFF headers are ≤16 bytes — never prefetch on those, or an `mdat` header
/// touch pulls multi‑MiB of media into WASM.
const NO_PREFETCH_MAX: usize = 16;

#[derive(Debug, Default)]
struct Window {
    /// File-absolute start of cached bytes.
    offset: u64,
    bytes: Bytes,
}

impl Window {
    fn end(&self) -> u64 {
        self.offset.saturating_add(self.bytes.len() as u64)
    }

    fn contains(&self, offset: u64, len: usize) -> bool {
        if self.bytes.is_empty() {
            return false;
        }
        let end = offset.saturating_add(len as u64);
        offset >= self.offset && end <= self.end()
    }

    fn slice(&self, offset: u64, len: usize) -> Bytes {
        let start = (offset - self.offset) as usize;
        self.bytes.slice(start..start + len)
    }
}

#[derive(Debug, Default)]
struct CacheState {
    /// LRU order: index 0 = oldest, last = most recently used.
    windows: Vec<Window>,
    /// Background margin prefetch in flight (one at a time).
    prefetch_inflight: bool,
}

impl CacheState {
    fn total_bytes(&self) -> usize {
        self.windows.iter().map(|w| w.bytes.len()).sum()
    }

    /// Hit → `(bytes, window_start, window_end)`.
    fn hit(&mut self, offset: u64, len: usize) -> Option<(Bytes, u64, u64)> {
        let idx = self.windows.iter().position(|w| w.contains(offset, len))?;
        let out = self.windows[idx].slice(offset, len);
        let start = self.windows[idx].offset;
        let end = self.windows[idx].end();
        let win = self.windows.remove(idx);
        self.windows.push(win);
        Some((out, start, end))
    }

    /// Find a sequential anchor for a miss at `offset` (most-recent first).
    fn sequential_anchor(&self, offset: u64) -> Option<(u64, u64)> {
        for w in self.windows.iter().rev() {
            let end = w.end();
            if offset >= w.offset && offset < end {
                return Some((w.offset, end));
            }
            if offset >= end && offset.saturating_sub(end) <= SEQ_SLACK {
                return Some((w.offset, end));
            }
        }
        None
    }

    fn covers_byte(&self, offset: u64) -> bool {
        self.windows.iter().any(|w| w.contains(offset, 1))
    }

    fn insert(&mut self, mut win: Window, max_bytes: usize) {
        if win.bytes.is_empty() {
            return;
        }
        // Merge overlapping / abutting windows into one contiguous buffer so
        // sequential growth sees the full region (mediabunny worker span).
        let mut i = 0;
        while i < self.windows.len() {
            let w = &self.windows[i];
            if w.end() < win.offset || win.end() < w.offset {
                i += 1;
                continue;
            }
            let start = w.offset.min(win.offset);
            let end = w.end().max(win.end());
            let mut merged = vec![0u8; (end - start) as usize];
            let w0 = (w.offset - start) as usize;
            merged[w0..w0 + w.bytes.len()].copy_from_slice(&w.bytes);
            let w1 = (win.offset - start) as usize;
            merged[w1..w1 + win.bytes.len()].copy_from_slice(&win.bytes);
            win = Window {
                offset: start,
                bytes: Bytes::from(merged),
            };
            self.windows.remove(i);
        }
        self.windows.push(win);
        let cap = max_bytes.max(1);
        while self.total_bytes() > cap && self.windows.len() > 1 {
            self.windows.remove(0);
        }
    }
}

/// Plan outer fill length for a media miss (mediabunny `PREFETCH_PROFILES.network`).
fn plan_fill_len(
    state: &CacheState,
    offset: u64,
    want: usize,
    available: usize,
    quantum: usize,
) -> usize {
    if want <= NO_PREFETCH_MAX {
        return want.min(available);
    }
    let quantum = quantum.max(1);
    let mut base = want.max(quantum).min(available);
    // UrlSource min load only when using a real (MiB-scale) quantum.
    if quantum >= MIN_LOAD {
        base = base.max(MIN_LOAD.min(available));
    }

    let Some((region_start, _region_end)) = state.sequential_anchor(offset) else {
        return base;
    };

    let aligned_start = region_start.saturating_sub(SEQ_START_PAD) & !(SEQ_START_PAD - 1);
    // mediabunny uses `size + 1` so a full quantum grows to the next step/pow2.
    let size = offset.saturating_sub(aligned_start).max(1);
    let max_ext = quantum as u64;
    let grow = size.saturating_add(1);
    let a = grow.div_ceil(max_ext).saturating_mul(max_ext).max(max_ext);
    let b = grow
        .checked_next_power_of_two()
        .unwrap_or(u64::MAX)
        .max(max_ext);
    let extent = a.min(b);
    let target_end = aligned_start.saturating_add(extent);
    let need = target_end.saturating_sub(offset) as usize;
    need.max(base).min(available)
}

/// mediabunny network `thresholdPoint`: past midpoint, or within `quantum` of end.
fn past_right_margin(win_start: u64, win_end: u64, read_end: u64, quantum: usize) -> bool {
    let half = win_start / 2 + win_end / 2;
    let threshold = half.max(win_end.saturating_sub(quantum as u64));
    read_end >= threshold
}

/// Next extent past `win_end` when a hit entered the right margin.
fn margin_prefetch_job(
    state: &CacheState,
    win_start: u64,
    win_end: u64,
    offset: u64,
    want: usize,
    file_size: u64,
    quantum: usize,
) -> Option<(u64, usize)> {
    if state.prefetch_inflight {
        return None;
    }
    let read_end = offset.saturating_add(want as u64);
    if !past_right_margin(win_start, win_end, read_end, quantum) {
        return None;
    }
    let fill_start = win_end;
    if fill_start >= file_size || state.covers_byte(fill_start) {
        return None;
    }
    let available = (file_size - fill_start) as usize;
    if available == 0 {
        return None;
    }
    // Pretend a sequential miss at the window edge so network grow applies.
    let probe = want.max(quantum.min(available));
    let fill_len = plan_fill_len(state, fill_start, probe, available, quantum);
    (fill_len > 0).then_some((fill_start, fill_len))
}

fn sync_fetch_map(fetch_map: &Option<Arc<FetchMap>>, state: &CacheState) {
    let Some(map) = fetch_map else {
        return;
    };
    let windows: Vec<(u64, u64)> = state
        .windows
        .iter()
        .filter(|w| w.bytes.len() > NO_PREFETCH_MAX)
        .map(|w| (w.offset, w.end()))
        .collect();
    map.set_windows(&windows);
}

struct Shared {
    quantum: usize,
    budget: usize,
    state: Mutex<CacheState>,
    fetch_map: Option<Arc<FetchMap>>,
}

/// Wraps an inner [`MediaSource`] with a byte-budget LRU of cache windows.
///
/// Requests larger than `quantum` bypass the cache. Smaller misses insert a
/// window (network-style sequential extend), then evict oldest until under budget.
/// Hits that enter the right margin kick a non-blocking ahead fill.
pub struct CachedSource<S> {
    inner: Arc<S>,
    shared: Arc<Shared>,
}

impl<S> CachedSource<S> {
    pub fn new(inner: S) -> Self {
        Self::with_capacity(inner, DEFAULT_CACHE_SIZE)
    }

    pub fn with_capacity(inner: S, quantum: usize) -> Self {
        Self::with_budget(inner, quantum, DEFAULT_CACHE_BUDGET, None)
    }

    pub fn with_fetch_map(inner: S, fetch_map: Arc<FetchMap>) -> Self
    where
        S: MediaSource,
    {
        let budget = if inner.network_min_load() {
            DEFAULT_CACHE_BUDGET
        } else {
            LOCAL_CACHE_BUDGET
        };
        Self::with_budget(inner, DEFAULT_CACHE_SIZE, budget, Some(fetch_map))
    }

    pub fn with_capacity_and_fetch_map(inner: S, quantum: usize, fetch_map: Arc<FetchMap>) -> Self
    where
        S: MediaSource,
    {
        let budget = if inner.network_min_load() {
            DEFAULT_CACHE_BUDGET
        } else {
            LOCAL_CACHE_BUDGET
        };
        Self::with_budget(inner, quantum, budget, Some(fetch_map))
    }

    pub fn with_budget(
        inner: S,
        quantum: usize,
        budget: usize,
        fetch_map: Option<Arc<FetchMap>>,
    ) -> Self {
        Self {
            inner: Arc::new(inner),
            shared: Arc::new(Shared {
                quantum: quantum.max(1),
                budget: budget.max(1),
                state: Mutex::new(CacheState::default()),
                fetch_map,
            }),
        }
    }

    pub fn into_inner(self) -> S {
        Arc::try_unwrap(self.inner)
            .unwrap_or_else(|_| panic!("CachedSource::into_inner while prefetch still holds inner"))
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Fill quantum / right-margin (not total budget).
    pub fn max_cache_size(&self) -> usize {
        self.shared.quantum
    }

    pub fn budget(&self) -> usize {
        self.shared.budget
    }

    /// Insert ranges already fetched outside this cache (progressive open demux).
    ///
    /// Skips tiny ISOBMFF header peeks (≤[`NO_PREFETCH_MAX`]) — same rule as miss fill —
    /// then syncs [`FetchMap`] so the gray scrubber strip covers `ftyp`/`moov` at open.
    pub fn seed_windows(&self, windows: impl IntoIterator<Item = (u64, Vec<u8>)>) {
        let Ok(mut state) = self.shared.state.lock() else {
            return;
        };
        for (offset, bytes) in windows {
            if bytes.len() <= NO_PREFETCH_MAX {
                continue;
            }
            state.insert(
                Window {
                    offset,
                    bytes: Bytes::from(bytes),
                },
                self.shared.budget,
            );
        }
        sync_fetch_map(&self.shared.fetch_map, &state);
    }

    #[cfg(test)]
    fn debug_windows(&self) -> Vec<(u64, u64)> {
        self.shared
            .state
            .lock()
            .map(|s| s.windows.iter().map(|w| (w.offset, w.end())).collect())
            .unwrap_or_default()
    }
}

impl<S: MediaSource + Send + Sync + 'static> CachedSource<S> {
    /// Arm + run ahead fill. On wasm, `spawn_local` so the hit path stays cold;
    /// on native, poll once (MemorySource completes inline — unit tests).
    fn kick_prefetch(&self, start: u64, len: usize) {
        let inner = Arc::clone(&self.inner);
        let shared = Arc::clone(&self.shared);
        let run = async move {
            let result = inner.read(start, len).await;
            if let Ok(mut state) = shared.state.lock() {
                state.prefetch_inflight = false;
                match result {
                    Ok(bytes) if !bytes.is_empty() => {
                        state.insert(
                            Window {
                                offset: start,
                                bytes,
                            },
                            shared.budget,
                        );
                        sync_fetch_map(&shared.fetch_map, &state);
                    }
                    _ => {}
                }
            }
        };

        #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
        {
            wasm_bindgen_futures::spawn_local(run);
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "webcodecs")))]
        {
            // MemorySource / test doubles complete on poll; loop in case of nested
            // Pending→Ready (CountingSource wraps another future).
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            let mut fut = Box::pin(run);
            for _ in 0..16 {
                if fut.as_mut().poll(&mut cx).is_ready() {
                    break;
                }
            }
        }
    }

    /// Explicit range warm (fMP4 next-fragment `mdat`) — does not take the
    /// single margin `prefetch_inflight` slot.
    fn kick_prefetch_explicit(&self, start: u64, len: usize) {
        if len == 0 {
            return;
        }
        let inner = Arc::clone(&self.inner);
        let shared = Arc::clone(&self.shared);
        let run = async move {
            let result = inner.read(start, len).await;
            if let Ok(mut state) = shared.state.lock()
                && let Ok(bytes) = result
                && !bytes.is_empty()
            {
                state.insert(
                    Window {
                        offset: start,
                        bytes,
                    },
                    shared.budget,
                );
                sync_fetch_map(&shared.fetch_map, &state);
            }
        };

        #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
        {
            wasm_bindgen_futures::spawn_local(run);
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "webcodecs")))]
        {
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            let mut fut = Box::pin(run);
            for _ in 0..16 {
                if fut.as_mut().poll(&mut cx).is_ready() {
                    break;
                }
            }
        }
    }
}

impl<S: MediaSource + Send + Sync + 'static> MediaSource for CachedSource<S> {
    fn size(&self) -> u64 {
        self.inner.size()
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        Box::pin(async move {
            let size = self.inner.size();
            if offset > size {
                return Err(MediaError::ReadOutOfRange { offset, len, size });
            }

            let available = (size - offset) as usize;
            let want = len.min(available);
            if want == 0 {
                return Ok(Bytes::new());
            }

            // Bypass for oversized requests — do not thrash the budget.
            if want > self.shared.quantum {
                return self.inner.read(offset, want).await;
            }

            let network = self.inner.network_min_load();
            {
                let mut state = self
                    .shared
                    .state
                    .lock()
                    .map_err(|_| MediaError::Source("cache lock poisoned".into()))?;
                if let Some((hit, win_start, win_end)) = state.hit(offset, want) {
                    // Margin ahead-fill is a UrlSource concern (RTT). Local Blob
                    // already lives in JS — do not mirror multi-MiB into WASM.
                    // mediabunny: BlobSource orch keeps Uint8Array in JS.
                    if network
                        && let Some((p_off, p_len)) = margin_prefetch_job(
                            &state,
                            win_start,
                            win_end,
                            offset,
                            want,
                            size,
                            self.shared.quantum,
                        )
                    {
                        state.prefetch_inflight = true;
                        drop(state);
                        self.kick_prefetch(p_off, p_len);
                    }
                    return Ok(hit);
                }
            }

            let fill_len = if network {
                let state = self
                    .shared
                    .state
                    .lock()
                    .map_err(|_| MediaError::Source("cache lock poisoned".into()))?;
                plan_fill_len(&state, offset, want, available, self.shared.quantum)
            } else {
                // Local: cache the sample only — no planned 8 MiB WASM window.
                want.min(available)
            };
            // Sample / small media reads: do **not** await the full 8 MiB quantum.
            // URL: floor at mediabunny `URL_SOURCE_MIN_LOAD_AMOUNT` (0.5 MiB) so
            // cold gray is not a Range-per-NAL storm; rest is bg kick.
            // Blob/Memory: blocking `want` only — no bg quantum finish.
            let blocking = if network && want > NO_PREFETCH_MAX && want < self.shared.quantum {
                let floor = if self.shared.quantum >= MIN_LOAD {
                    MIN_LOAD
                } else {
                    want
                };
                want.max(floor).min(fill_len).min(available)
            } else {
                fill_len
            };
            let filled = self.inner.read(offset, blocking).await?;
            // Share the window allocation with the consumer (no extra to_vec of want).
            let out = if want <= filled.len() {
                filled.slice(..want)
            } else {
                filled.clone()
            };

            // Tiny ISOBMFF header reads must not occupy budget.
            if filled.len() > NO_PREFETCH_MAX {
                let mut state = self
                    .shared
                    .state
                    .lock()
                    .map_err(|_| MediaError::Source("cache lock poisoned".into()))?;
                state.insert(
                    Window {
                        offset,
                        bytes: filled,
                    },
                    self.shared.budget,
                );
                sync_fetch_map(&self.shared.fetch_map, &state);
                // Finish the planned quantum behind the consumer (URL only).
                if network
                    && fill_len > blocking
                    && !state.prefetch_inflight
                    && !state.covers_byte(offset.saturating_add(blocking as u64))
                {
                    let rest = fill_len - blocking;
                    state.prefetch_inflight = true;
                    drop(state);
                    self.kick_prefetch(offset.saturating_add(blocking as u64), rest);
                }
            }

            Ok(out)
        })
    }

    fn prefetch(&self, offset: u64, len: usize) {
        // Local Blob/Memory: browser already holds the file; multi-MiB WASM
        // mirrors (play `kick_prefetch_samples`, margin fills) copy-tax Play.
        // Sample reads pull `want` on demand. `mediabunny: BlobSource` orch is JS.
        if !self.inner.network_min_load() {
            return;
        }
        if len <= NO_PREFETCH_MAX {
            return;
        }
        let size = self.inner.size();
        if offset >= size {
            return;
        }
        let available = (size - offset) as usize;
        let len = len.min(available).min(self.shared.quantum);
        if len == 0 {
            return;
        }
        if let Ok(state) = self.shared.state.lock()
            && state.covers_byte(offset)
            && state.covers_byte(offset.saturating_add(len as u64).saturating_sub(1))
        {
            return;
        }
        self.kick_prefetch_explicit(offset, len);
    }

    fn dispose(&self) {
        self.inner.dispose();
    }

    fn network_min_load(&self) -> bool {
        self.inner.network_min_load()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::source::MemorySource;

    #[derive(Clone)]
    struct CountingSource {
        inner: MemorySource,
        reads: Arc<AtomicUsize>,
        bytes_fetched: Arc<AtomicUsize>,
        first_read_len: Arc<AtomicUsize>,
    }

    impl CountingSource {
        fn new(data: Vec<u8>) -> Self {
            Self {
                inner: MemorySource::from_vec(data),
                reads: Arc::new(AtomicUsize::new(0)),
                bytes_fetched: Arc::new(AtomicUsize::new(0)),
                first_read_len: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl MediaSource for CountingSource {
        fn size(&self) -> u64 {
            self.inner.size()
        }

        fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
            let n = self.reads.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                self.first_read_len.store(len, Ordering::SeqCst);
            }
            let inner = self.inner.read(offset, len);
            let bytes_fetched = Arc::clone(&self.bytes_fetched);
            Box::pin(async move {
                let data = inner.await?;
                bytes_fetched.fetch_add(data.len(), Ordering::SeqCst);
                Ok(data)
            })
        }
    }

    #[derive(Clone)]
    struct NetworkCounting(CountingSource);

    impl MediaSource for NetworkCounting {
        fn size(&self) -> u64 {
            self.0.size()
        }

        fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
            self.0.read(offset, len)
        }

        fn network_min_load(&self) -> bool {
            true
        }
    }

    fn poll_ready<S: MediaSource>(src: &S, offset: u64, len: usize) -> Result<Bytes, MediaError> {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut fut = src.read(offset, len);
        for _ in 0..8 {
            match std::future::Future::poll(std::pin::Pin::new(&mut fut), &mut cx) {
                std::task::Poll::Ready(r) => return r,
                std::task::Poll::Pending => continue,
            }
        }
        panic!("source read stayed pending");
    }

    #[test]
    fn cache_hit_avoids_inner_read() {
        let data: Vec<u8> = (0u8..64).collect();
        let counting = CountingSource::new(data.clone());
        let cached = CachedSource::with_budget(counting.clone(), 32, 256, None);

        let a = poll_ready(&cached, 0, 20).unwrap();
        assert_eq!(a, &data[0..20]);
        // Local sample miss: blocking `want` only (no bg quantum).
        assert_eq!(counting.reads.load(Ordering::SeqCst), 1);
        assert_eq!(counting.bytes_fetched.load(Ordering::SeqCst), 20);

        let b = poll_ready(&cached, 4, 8).unwrap();
        assert_eq!(b, &data[4..12]);
        assert_eq!(
            counting.reads.load(Ordering::SeqCst),
            1,
            "hit must not re-fetch"
        );
    }

    #[test]
    fn oversized_read_bypasses_cache() {
        let data: Vec<u8> = (0u8..64).collect();
        let counting = CountingSource::new(data.clone());
        let cached = CachedSource::with_budget(counting.clone(), 16, 256, None);

        let big = poll_ready(&cached, 0, 32).unwrap();
        assert_eq!(big, &data[0..32]);
        assert_eq!(counting.reads.load(Ordering::SeqCst), 1);
        assert_eq!(counting.bytes_fetched.load(Ordering::SeqCst), 32);

        let _ = poll_ready(&cached, 0, 4).unwrap();
        assert_eq!(counting.reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn budget_keeps_disjoint_windows_hot() {
        let data: Vec<u8> = (0u8..128).collect();
        let counting = CountingSource::new(data.clone());
        let cached = CachedSource::with_budget(counting.clone(), 32, 96, None);

        let _ = poll_ready(&cached, 0, 20).unwrap();
        let _ = poll_ready(&cached, 64, 20).unwrap();
        // Local: one inner read per miss (want only, no bg quantum).
        assert_eq!(counting.reads.load(Ordering::SeqCst), 2);

        let _ = poll_ready(&cached, 4, 4).unwrap();
        let _ = poll_ready(&cached, 68, 4).unwrap();
        assert_eq!(counting.reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fetch_map_mirrors_resident_windows() {
        let data: Vec<u8> = (0u8..128).collect();
        let map = Arc::new(FetchMap::new());
        let cached = CachedSource::with_budget(
            MemorySource::from_vec(data),
            32,
            256,
            Some(Arc::clone(&map)),
        );

        let _ = poll_ready(&cached, 0, 8).unwrap();
        assert!(map.file_fractions(128).is_empty());

        let _ = poll_ready(&cached, 0, 20).unwrap();
        let _ = poll_ready(&cached, 64, 20).unwrap();
        // Exact resident windows stay split; paint soft-merges sub‑pixel holes.
        assert_eq!(map.windows().len(), 2);
        assert_eq!(map.file_fractions(128).len(), 1);
    }

    #[test]
    fn seed_windows_fills_fetch_map_from_open_ranges() {
        let data: Vec<u8> = (0u8..128).collect();
        let map = Arc::new(FetchMap::new());
        let cached = CachedSource::with_budget(
            MemorySource::from_vec(data.clone()),
            32,
            256,
            Some(Arc::clone(&map)),
        );

        // Progressive demux already read ftyp+moov-sized head; seed before sample I/O.
        cached.seed_windows([(0, data[0..48].to_vec())]);
        let fracs = map.file_fractions(128);
        assert_eq!(fracs.len(), 1);
        assert_eq!(fracs[0].0, 0.0);
        assert!((fracs[0].1 - (48.0 / 128.0) as f32).abs() < 1e-5);

        // Seeded bytes are cache hits (no inner read).
        let counting = CountingSource::new(data);
        let map2 = Arc::new(FetchMap::new());
        let cached2 = CachedSource::with_budget(counting.clone(), 32, 256, Some(Arc::clone(&map2)));
        let head: Vec<u8> = (0u8..48).collect();
        cached2.seed_windows([(0, head)]);
        let hit = poll_ready(&cached2, 0, 20).unwrap();
        assert_eq!(hit.len(), 20);
        assert_eq!(counting.reads.load(Ordering::SeqCst), 0);
        assert!(!map2.file_fractions(128).is_empty());
        assert_eq!(map2.file_fractions(128)[0].0, 0.0);
    }

    #[test]
    fn over_budget_evicts_oldest() {
        let mut data = vec![0u8; 64 * 1024];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let counting = CountingSource::new(data);
        let cached = CachedSource::with_budget(counting.clone(), 32, 64, None);

        let _ = poll_ready(&cached, 0, 20).unwrap();
        let _ = poll_ready(&cached, 16 * 1024, 20).unwrap();
        let _ = poll_ready(&cached, 32 * 1024, 20).unwrap();
        let after_fills = counting.reads.load(Ordering::SeqCst);
        assert!(after_fills >= 3);

        let _ = poll_ready(&cached, 4, 4).unwrap();
        // Evicted head may miss again; trailing windows stay hot.
        let mid = counting.reads.load(Ordering::SeqCst);
        let _ = poll_ready(&cached, 16 * 1024 + 4, 4).unwrap();
        let _ = poll_ready(&cached, 32 * 1024 + 4, 4).unwrap();
        assert_eq!(counting.reads.load(Ordering::SeqCst), mid);
    }

    #[test]
    fn sequential_miss_extends_fill() {
        let data: Vec<u8> = (0u8..=255).collect();
        let counting = CountingSource::new(data);
        let cached = CachedSource::with_budget(NetworkCounting(counting.clone()), 32, 512, None);

        let _ = poll_ready(&cached, 0, 20).unwrap();
        assert_eq!(counting.bytes_fetched.load(Ordering::SeqCst), 32);

        let _ = poll_ready(&cached, 32, 20).unwrap();
        assert_eq!(counting.bytes_fetched.load(Ordering::SeqCst), 64);

        let _ = poll_ready(&cached, 64, 20).unwrap();
        let fetched = counting.bytes_fetched.load(Ordering::SeqCst);
        assert!(
            fetched >= 64 + 32,
            "expected grow past 64-byte region, got {fetched}"
        );
    }

    #[test]
    fn sample_miss_unblocks_before_full_quantum() {
        // URL: consumer `want` above header peek; blocking floor + bg quantum.
        let data: Vec<u8> = (0u8..=255).collect();
        let counting = CountingSource::new(data.clone());
        let cached = CachedSource::with_budget(NetworkCounting(counting.clone()), 64, 512, None);

        let out = poll_ready(&cached, 0, 20).unwrap();
        assert_eq!(out, &data[0..20]);
        assert!(
            counting.bytes_fetched.load(Ordering::SeqCst) >= 64,
            "bg kick should finish the quantum, got {}",
            counting.bytes_fetched.load(Ordering::SeqCst)
        );
        assert!(counting.reads.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn local_sample_miss_no_bg_quantum() {
        let data: Vec<u8> = (0u8..=255).collect();
        let counting = CountingSource::new(data.clone());
        let cached = CachedSource::with_budget(counting.clone(), 64, 512, None);

        let out = poll_ready(&cached, 0, 20).unwrap();
        assert_eq!(out, &data[0..20]);
        assert_eq!(
            counting.bytes_fetched.load(Ordering::SeqCst),
            20,
            "local must not bg-finish the quantum into WASM"
        );
        assert_eq!(counting.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn local_mib_quantum_blocks_want_only() {
        let data = vec![7u8; MIN_LOAD + 4096];
        let counting = CountingSource::new(data.clone());
        let cached = CachedSource::with_budget(counting.clone(), MIN_LOAD, MIN_LOAD * 2, None);
        let want = 100usize;
        let out = poll_ready(&cached, 0, want).unwrap();
        assert_eq!(out, &data[..want]);
        assert_eq!(
            counting.first_read_len.load(Ordering::SeqCst),
            want,
            "local sample miss must not floor blocking at MIN_LOAD"
        );
        assert_eq!(
            counting.bytes_fetched.load(Ordering::SeqCst),
            want,
            "local must not bg-finish MiB quantum"
        );
    }

    #[test]
    fn network_mib_quantum_floors_blocking_at_min_load() {
        let data = vec![9u8; MIN_LOAD + 4096];
        let counting = CountingSource::new(data.clone());
        let cached = CachedSource::with_budget(
            NetworkCounting(counting.clone()),
            MIN_LOAD,
            MIN_LOAD * 2,
            None,
        );
        let want = 100usize;
        let out = poll_ready(&cached, 0, want).unwrap();
        assert_eq!(out, &data[..want]);
        assert_eq!(
            counting.first_read_len.load(Ordering::SeqCst),
            MIN_LOAD,
            "URL sample miss should floor blocking at URL_SOURCE_MIN_LOAD"
        );
    }

    #[test]
    fn margin_hit_prefetches_ahead() {
        // Window 0..64 (two fills). Hit in right half triggers ahead fill past 64.
        let data: Vec<u8> = (0u8..=255).collect();
        let counting = CountingSource::new(data.clone());
        let cached = CachedSource::with_budget(NetworkCounting(counting.clone()), 32, 512, None);

        let _ = poll_ready(&cached, 0, 20).unwrap();
        let _ = poll_ready(&cached, 32, 20).unwrap();
        let after_fill = counting.bytes_fetched.load(Ordering::SeqCst);
        assert_eq!(after_fill, 64);

        // Past midpoint of 0..64 → background prefetch from 64 (native: polled inline).
        let hit = poll_ready(&cached, 40, 8).unwrap();
        assert_eq!(hit, &data[40..48]);
        let fetched = counting.bytes_fetched.load(Ordering::SeqCst);
        assert!(
            fetched > after_fill,
            "margin hit should prefetch ahead, fetched={fetched} after_fill={after_fill}"
        );
        assert!(
            cached
                .debug_windows()
                .iter()
                .any(|&(s, e)| s <= 70 && 74 <= e),
            "prefetch should cover past former edge, wins={:?}",
            cached.debug_windows()
        );

        // Consumer read in the prefetched tail is a hit (may arm another prefetch).
        let out = poll_ready(&cached, 70, 4).unwrap();
        assert_eq!(out, &data[70..74]);
    }

    #[test]
    fn local_margin_hit_does_not_prefetch() {
        let data: Vec<u8> = (0u8..=255).collect();
        let counting = CountingSource::new(data.clone());
        let cached = CachedSource::with_budget(counting.clone(), 64, 512, None);
        let _ = poll_ready(&cached, 0, 64).unwrap();
        let after = counting.bytes_fetched.load(Ordering::SeqCst);
        assert_eq!(after, 64);
        let _ = poll_ready(&cached, 40, 8).unwrap();
        assert_eq!(
            counting.bytes_fetched.load(Ordering::SeqCst),
            after,
            "local hit must not margin-prefetch into WASM"
        );
    }

    #[test]
    fn fetch_map_budget_local_vs_network() {
        let map = Arc::new(FetchMap::new());
        let local =
            CachedSource::with_fetch_map(CountingSource::new(vec![0; 64]), Arc::clone(&map));
        assert_eq!(local.budget(), LOCAL_CACHE_BUDGET);
        let net = CachedSource::with_fetch_map(
            NetworkCounting(CountingSource::new(vec![0; 64])),
            Arc::clone(&map),
        );
        assert_eq!(net.budget(), DEFAULT_CACHE_BUDGET);
    }
}
