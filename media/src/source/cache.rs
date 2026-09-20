//! Multi-window read cache (LRU of ~8 MiB slots).
//!
//! mediabunny `BlobSource` default is a **single** 8 MiB window. Progressive URL
//! play over high-RTT hosts (GitHub Releases via `/proxy`) thrashes that window
//! when A/V sample reads alternate — underrun holds while the scrubber gray bar
//! still looked “warm”. Local invention: keep several resident windows so gray
//! coverage matches hot bytes (`FetchMap` rebuilt from slots).

use std::sync::{Arc, Mutex};

use crate::error::MediaError;

use super::fetch_map::FetchMap;
use super::{MediaSource, ReadFuture};

/// Per-window fill size (mediabunny `BlobSource` default).
pub const DEFAULT_CACHE_SIZE: usize = 8 * 1024 * 1024;

/// How many [`DEFAULT_CACHE_SIZE`] windows to retain (≈32 MiB total).
pub const DEFAULT_CACHE_SLOTS: usize = 4;

/// ISOBMFF headers are ≤16 bytes — never prefetch on those, or an `mdat` header
/// touch pulls multi‑MiB of media into WASM.
const NO_PREFETCH_MAX: usize = 16;

#[derive(Debug, Default)]
struct Window {
    /// File-absolute start of cached bytes.
    offset: u64,
    bytes: Vec<u8>,
}

impl Window {
    fn contains(&self, offset: u64, len: usize) -> bool {
        if self.bytes.is_empty() {
            return false;
        }
        let end = offset.saturating_add(len as u64);
        let win_end = self.offset.saturating_add(self.bytes.len() as u64);
        offset >= self.offset && end <= win_end
    }

    fn slice(&self, offset: u64, len: usize) -> Vec<u8> {
        let start = (offset - self.offset) as usize;
        self.bytes[start..start + len].to_vec()
    }
}

#[derive(Debug, Default)]
struct CacheState {
    /// LRU order: index 0 = oldest, last = most recently used.
    windows: Vec<Window>,
}

impl CacheState {
    fn hit(&mut self, offset: u64, len: usize) -> Option<Vec<u8>> {
        let idx = self.windows.iter().position(|w| w.contains(offset, len))?;
        let out = self.windows[idx].slice(offset, len);
        let win = self.windows.remove(idx);
        self.windows.push(win);
        Some(out)
    }

    fn insert(&mut self, win: Window, max_slots: usize) {
        if win.bytes.is_empty() {
            return;
        }
        // Replace overlapping / contained older windows so we don't keep duplicates.
        let new_start = win.offset;
        let new_end = win.offset.saturating_add(win.bytes.len() as u64);
        self.windows.retain(|w| {
            let s = w.offset;
            let e = w.offset.saturating_add(w.bytes.len() as u64);
            e <= new_start || s >= new_end
        });
        self.windows.push(win);
        while self.windows.len() > max_slots.max(1) {
            self.windows.remove(0);
        }
    }
}

/// Wraps an inner [`MediaSource`] with an LRU of contiguous cache windows.
///
/// Requests larger than `window_size` bypass the cache. Smaller misses insert a
/// new window (capped by EOF and `window_size`), evicting the oldest slot.
pub struct CachedSource<S> {
    inner: S,
    window_size: usize,
    max_slots: usize,
    state: Mutex<CacheState>,
    /// Demo scrubber gray strip — mirrors **actual** resident windows.
    fetch_map: Option<Arc<FetchMap>>,
}

impl<S> CachedSource<S> {
    pub fn new(inner: S) -> Self {
        Self::with_capacity(inner, DEFAULT_CACHE_SIZE)
    }

    pub fn with_capacity(inner: S, window_size: usize) -> Self {
        Self::with_slots(inner, window_size, DEFAULT_CACHE_SLOTS, None)
    }

    pub fn with_fetch_map(inner: S, fetch_map: Arc<FetchMap>) -> Self {
        Self::with_slots(
            inner,
            DEFAULT_CACHE_SIZE,
            DEFAULT_CACHE_SLOTS,
            Some(fetch_map),
        )
    }

    pub fn with_capacity_and_fetch_map(
        inner: S,
        window_size: usize,
        fetch_map: Arc<FetchMap>,
    ) -> Self {
        Self::with_slots(inner, window_size, DEFAULT_CACHE_SLOTS, Some(fetch_map))
    }

    fn with_slots(
        inner: S,
        window_size: usize,
        max_slots: usize,
        fetch_map: Option<Arc<FetchMap>>,
    ) -> Self {
        Self {
            inner,
            window_size: window_size.max(1),
            max_slots: max_slots.max(1),
            state: Mutex::new(CacheState::default()),
            fetch_map,
        }
    }

    pub fn into_inner(self) -> S {
        self.inner
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }

    pub fn max_cache_size(&self) -> usize {
        self.window_size
    }

    pub fn max_slots(&self) -> usize {
        self.max_slots
    }

    fn sync_fetch_map(&self, state: &CacheState) {
        let Some(map) = &self.fetch_map else {
            return;
        };
        let windows: Vec<(u64, u64)> = state
            .windows
            .iter()
            .filter(|w| w.bytes.len() > NO_PREFETCH_MAX)
            .map(|w| (w.offset, w.offset.saturating_add(w.bytes.len() as u64)))
            .collect();
        map.set_windows(&windows);
    }
}

impl<S: MediaSource> MediaSource for CachedSource<S> {
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
                return Ok(Vec::new());
            }

            // Bypass for oversized requests — do not thrash slots.
            if want > self.window_size {
                return self.inner.read(offset, want).await;
            }

            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| MediaError::Source("cache lock poisoned".into()))?;
                if let Some(hit) = state.hit(offset, want) {
                    return Ok(hit);
                }
            }

            let fill_len = if want <= NO_PREFETCH_MAX {
                want
            } else {
                want.max(self.window_size.min(available))
            };
            let filled = self.inner.read(offset, fill_len).await?;
            let out = filled.get(..want).unwrap_or(&filled).to_vec();

            // Tiny ISOBMFF header reads must not occupy an LRU slot (would
            // evict an 8 MiB media window after a cold miss).
            if filled.len() > NO_PREFETCH_MAX {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| MediaError::Source("cache lock poisoned".into()))?;
                state.insert(
                    Window {
                        offset,
                        bytes: filled,
                    },
                    self.max_slots,
                );
                self.sync_fetch_map(&state);
            }

            Ok(out)
        })
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
    }

    impl MediaSource for CountingSource {
        fn size(&self) -> u64 {
            self.inner.size()
        }

        fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let inner = self.inner.read(offset, len);
            let bytes_fetched = Arc::clone(&self.bytes_fetched);
            Box::pin(async move {
                let data = inner.await?;
                bytes_fetched.fetch_add(data.len(), Ordering::SeqCst);
                Ok(data)
            })
        }
    }

    fn poll_ready<S: MediaSource>(src: &S, offset: u64, len: usize) -> Result<Vec<u8>, MediaError> {
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
        let counting = CountingSource {
            inner: MemorySource::from_vec(data.clone()),
            reads: Arc::new(AtomicUsize::new(0)),
            bytes_fetched: Arc::new(AtomicUsize::new(0)),
        };
        let cached = CachedSource::with_capacity(counting.clone(), 32);

        let a = poll_ready(&cached, 0, 20).unwrap();
        assert_eq!(a, &data[0..20]);
        assert_eq!(counting.reads.load(Ordering::SeqCst), 1);
        assert_eq!(counting.bytes_fetched.load(Ordering::SeqCst), 32);

        let b = poll_ready(&cached, 20, 8).unwrap();
        assert_eq!(b, &data[20..28]);
        assert_eq!(counting.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn oversized_read_bypasses_cache() {
        let data: Vec<u8> = (0u8..64).collect();
        let counting = CountingSource {
            inner: MemorySource::from_vec(data.clone()),
            reads: Arc::new(AtomicUsize::new(0)),
            bytes_fetched: Arc::new(AtomicUsize::new(0)),
        };
        let cached = CachedSource::with_capacity(counting.clone(), 16);

        let big = poll_ready(&cached, 0, 32).unwrap();
        assert_eq!(big, &data[0..32]);
        assert_eq!(counting.reads.load(Ordering::SeqCst), 1);
        assert_eq!(counting.bytes_fetched.load(Ordering::SeqCst), 32);

        let _ = poll_ready(&cached, 0, 4).unwrap();
        assert_eq!(counting.reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn lru_keeps_disjoint_windows_hot() {
        let data: Vec<u8> = (0u8..128).collect();
        let counting = CountingSource {
            inner: MemorySource::from_vec(data.clone()),
            reads: Arc::new(AtomicUsize::new(0)),
            bytes_fetched: Arc::new(AtomicUsize::new(0)),
        };
        let cached = CachedSource::with_slots(counting.clone(), 32, 2, None);

        // want > NO_PREFETCH_MAX and ≤ window_size → full-slot fill + LRU insert.
        let _ = poll_ready(&cached, 0, 20).unwrap(); // fills 0..32
        let _ = poll_ready(&cached, 64, 20).unwrap(); // fills 64..96
        assert_eq!(counting.reads.load(Ordering::SeqCst), 2);

        // Both still resident.
        let _ = poll_ready(&cached, 4, 4).unwrap();
        let _ = poll_ready(&cached, 68, 4).unwrap();
        assert_eq!(counting.reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fetch_map_mirrors_resident_slots() {
        let data: Vec<u8> = (0u8..128).collect();
        let map = Arc::new(FetchMap::new());
        let cached =
            CachedSource::with_slots(MemorySource::from_vec(data), 32, 2, Some(Arc::clone(&map)));

        let _ = poll_ready(&cached, 0, 8).unwrap(); // header-sized fill — skipped in map
        assert!(map.file_fractions(128).is_empty());

        let _ = poll_ready(&cached, 0, 20).unwrap();
        let _ = poll_ready(&cached, 64, 20).unwrap();
        let f = map.file_fractions(128);
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn third_window_evicts_oldest() {
        let data: Vec<u8> = (0u8..192).collect();
        let counting = CountingSource {
            inner: MemorySource::from_vec(data),
            reads: Arc::new(AtomicUsize::new(0)),
            bytes_fetched: Arc::new(AtomicUsize::new(0)),
        };
        let cached = CachedSource::with_slots(counting.clone(), 32, 2, None);

        let _ = poll_ready(&cached, 0, 20).unwrap();
        let _ = poll_ready(&cached, 64, 20).unwrap();
        let _ = poll_ready(&cached, 128, 20).unwrap(); // evicts 0..
        assert_eq!(counting.reads.load(Ordering::SeqCst), 3);

        // Oldest miss again (header-sized — no slot insert).
        let _ = poll_ready(&cached, 4, 4).unwrap();
        assert_eq!(counting.reads.load(Ordering::SeqCst), 4);
        // Newer slots still hot.
        let _ = poll_ready(&cached, 68, 4).unwrap();
        let _ = poll_ready(&cached, 132, 4).unwrap();
        assert_eq!(counting.reads.load(Ordering::SeqCst), 4);
    }
}
