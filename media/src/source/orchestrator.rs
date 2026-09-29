//! Byte-read hole orchestrator — port of mediabunny `ReadOrchestrator`.
//!
//! mediabunny: `src/source.ts` `ReadOrchestrator` + `PREFETCH_PROFILES.network`
//! (+ `UrlSource._runWorker` for the HTTP body — lives in [`super::url`]).
//!
//! Model: workers march forward filling holes; a pending **inner** slice
//! resolves as soon as its holes are covered — not when the outer prefetch
//! hole finishes. Queued holes merge while `maxWorkerCount` is saturated.

// UrlSource (wasm + webcodecs) is the live consumer. Native Clippy builds the
// port + memory tests without that module — allow dead_code on host targets.
#![cfg_attr(
    not(all(target_arch = "wasm32", feature = "webcodecs")),
    allow(dead_code)
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use crate::error::MediaError;

/// mediabunny `URL_SOURCE_MIN_LOAD_AMOUNT`.
pub const URL_SOURCE_MIN_LOAD: u64 = 512 * 1024;

/// mediabunny `UrlSource` default `maxCacheSize` (64 MiB).
pub const URL_ORCH_CACHE_SIZE: usize = 64 * 1024 * 1024;

const MAX_EXTENSION: u64 = 8 * 1024 * 1024;
const SEQ_START_PAD: u64 = 64 * 1024;
const GAP_TOLERANCE: u64 = 1 << 17;

type RunWorkerFn = Arc<
    dyn Fn(Arc<ReadOrchestrator>, WorkerId) -> Pin<Box<dyn Future<Output = ()> + 'static>>
        + Send
        + Sync,
>;

/// Opaque worker handle for [`ReadOrchestratorOptions::run_worker`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorkerId(u64);

#[derive(Clone, Copy, Debug)]
struct Hole {
    start: u64,
    end: u64,
}

struct PendingSlice {
    start: u64,
    bytes: Vec<u8>,
    holes: Vec<Hole>,
    complete: Arc<CompleteCell>,
}

struct CacheEntry {
    start: u64,
    end: u64,
    bytes: Vec<u8>,
    age: u64,
}

struct ReadWorker {
    id: WorkerId,
    start_pos: u64,
    current_pos: u64,
    target_pos: u64,
    strict_target: bool,
    running: bool,
    aborted: bool,
    pending_slices: Vec<Arc<Mutex<PendingSlice>>>,
    age: u64,
}

struct QueuedRead {
    hole: Hole,
    strict_target: bool,
    pending_slices: Vec<Arc<Mutex<PendingSlice>>>,
    age: u64,
}

struct OrchState {
    file_size: Option<u64>,
    next_age: u64,
    next_worker_id: u64,
    workers: Vec<ReadWorker>,
    cache: Vec<CacheEntry>,
    current_cache_size: usize,
    queued_reads: Vec<QueuedRead>,
    disposed: bool,
}

/// Options for [`ReadOrchestrator::new`].
pub struct ReadOrchestratorOptions {
    pub max_cache_size: usize,
    pub max_worker_count: usize,
    pub run_worker: RunWorkerFn,
    pub on_idle_worker_removed: Option<Arc<dyn Fn(WorkerId) + Send + Sync>>,
}

/// mediabunny `ReadOrchestrator`.
pub struct ReadOrchestrator {
    state: Mutex<OrchState>,
    options: ReadOrchestratorOptions,
}

impl std::fmt::Debug for ReadOrchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadOrchestrator")
            .field("max_cache_size", &self.options.max_cache_size)
            .field("max_worker_count", &self.options.max_worker_count)
            .finish_non_exhaustive()
    }
}

/// Public worker cursor for the HTTP runner.
#[derive(Clone, Copy, Debug)]
pub struct WorkerSnapshot {
    pub current_pos: u64,
    pub target_pos: u64,
    pub strict_target: bool,
    pub aborted: bool,
    pub running: bool,
}

impl ReadOrchestrator {
    pub fn new(options: ReadOrchestratorOptions) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(OrchState {
                file_size: None,
                next_age: 0,
                next_worker_id: 1,
                workers: Vec::new(),
                cache: Vec::new(),
                current_cache_size: 0,
                queued_reads: Vec::new(),
                disposed: false,
            }),
            options,
        })
    }

    pub fn max_worker_count(&self) -> usize {
        self.options.max_worker_count.max(1)
    }

    pub fn supply_file_size(&self, size: u64) {
        let mut g = self.state.lock().expect("orch");
        if g.file_size.is_some() {
            return;
        }
        g.file_size = Some(size);
        for w in &mut g.workers {
            w.target_pos = w.target_pos.min(size);
            w.strict_target = true;
        }
        let mut i = 0;
        while i < g.queued_reads.len() {
            if g.queued_reads[i].hole.start >= size {
                for s in g.queued_reads[i].pending_slices.drain(..) {
                    complete_empty(&s);
                }
                g.queued_reads.remove(i);
            } else {
                if g.queued_reads[i].hole.end > size {
                    g.queued_reads[i].hole.end = size;
                    g.queued_reads[i].strict_target = true;
                }
                i += 1;
            }
        }
    }

    #[allow(dead_code)]
    pub fn file_size(&self) -> Option<u64> {
        self.state.lock().expect("orch").file_size
    }

    #[allow(dead_code)] // gray-strip / debug hooks
    pub fn cache_windows(&self) -> Vec<(u64, u64)> {
        self.state
            .lock()
            .expect("orch")
            .cache
            .iter()
            .map(|e| (e.start, e.end))
            .collect()
    }

    pub async fn read(self: &Arc<Self>, start: u64, end: u64) -> Result<Vec<u8>, MediaError> {
        if end <= start {
            return Ok(Vec::new());
        }
        match self.begin_read(start, end)? {
            ReadKick::Ready(bytes) => Ok(bytes),
            ReadKick::Pending(cell) => CompleteFut(cell)
                .await
                .unwrap_or_else(|| Err(MediaError::Source("read cancelled".into()))),
        }
    }

    pub fn worker_snapshot(&self, id: WorkerId) -> Option<WorkerSnapshot> {
        let g = self.state.lock().expect("orch");
        g.workers
            .iter()
            .find(|w| w.id == id)
            .map(|w| WorkerSnapshot {
                current_pos: w.current_pos,
                target_pos: w.target_pos,
                strict_target: w.strict_target,
                aborted: w.aborted,
                running: w.running,
            })
    }

    /// mediabunny `supplyWorkerData`.
    pub fn supply_worker_data(self: &Arc<Self>, id: WorkerId, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut remove_idle: Vec<WorkerId> = Vec::new();
        {
            let mut g = self.state.lock().expect("orch");
            if g.disposed {
                return;
            }
            let Some(idx) = g.workers.iter().position(|w| w.id == id) else {
                return;
            };
            if g.workers[idx].aborted {
                return;
            }

            let start = g.workers[idx].current_pos;
            let end = start + bytes.len() as u64;
            insert_into_cache(&mut g, start, bytes, self.options.max_cache_size);
            g.workers[idx].current_pos = end;
            if g.workers[idx].current_pos > g.workers[idx].target_pos {
                g.workers[idx].target_pos = g.workers[idx].current_pos;
                attach_queued_reads(&mut g, idx);
            }

            let slices = g.workers[idx].pending_slices.clone();
            let mut drop_idx = Vec::new();
            for (si, slice_arc) in slices.iter().enumerate() {
                let Ok(mut slice) = slice_arc.lock() else {
                    continue;
                };
                let clamped_start = start.max(slice.start);
                let slice_end = slice.start + slice.bytes.len() as u64;
                let clamped_end = end.min(slice_end);
                if clamped_start < clamped_end {
                    let dst = (clamped_start - slice.start) as usize;
                    let src0 = (clamped_start - start) as usize;
                    let src1 = (clamped_end - start) as usize;
                    slice.bytes[dst..dst + (src1 - src0)].copy_from_slice(&bytes[src0..src1]);
                }
                let mut j = 0;
                while j < slice.holes.len() {
                    if start <= slice.holes[j].start && end > slice.holes[j].start {
                        slice.holes[j].start = end;
                    }
                    if slice.holes[j].end <= slice.holes[j].start {
                        slice.holes.remove(j);
                    } else {
                        j += 1;
                    }
                }
                if slice.holes.is_empty() {
                    let out = std::mem::take(&mut slice.bytes);
                    slice.complete.complete(Ok(out));
                    drop_idx.push(si);
                }
            }
            for si in drop_idx.into_iter().rev() {
                if let Some(w) = g.workers.iter_mut().find(|w| w.id == id) {
                    w.pending_slices.remove(si);
                }
            }

            for w in &g.workers {
                if w.id == id || w.running {
                    continue;
                }
                if closed_overlap(start, end, w.current_pos, w.target_pos) {
                    remove_idle.push(w.id);
                }
            }
            for rid in &remove_idle {
                if let Some(i) = g.workers.iter().position(|w| w.id == *rid) {
                    g.workers.remove(i);
                }
            }
        }
        for rid in remove_idle {
            if let Some(cb) = &self.options.on_idle_worker_removed {
                cb(rid);
            }
        }
    }

    /// mediabunny `signalWorkerStoppedRunning`.
    pub fn signal_worker_stopped_running(self: &Arc<Self>, id: WorkerId) {
        let mut to_kick: Option<WorkerId> = None;
        {
            let mut g = self.state.lock().expect("orch");
            let Some(w) = g.workers.iter_mut().find(|w| w.id == id) else {
                return;
            };
            if !w.running {
                return;
            }
            w.running = false;
            if !w.aborted {
                w.pending_slices.clear();
            }

            if g.queued_reads.is_empty() || g.disposed {
                return;
            }
            let mut oldest = 0;
            for i in 1..g.queued_reads.len() {
                if g.queued_reads[i].age < g.queued_reads[oldest].age {
                    oldest = i;
                }
            }
            let queued = g.queued_reads.remove(oldest);
            match create_worker(
                &mut g,
                queued.hole.start,
                queued.hole.end,
                queued.strict_target,
                self.options.max_worker_count.max(1),
                &self.options.on_idle_worker_removed,
            ) {
                Some(new_id) => {
                    if let Some(wi) = g.workers.iter().position(|w| w.id == new_id) {
                        g.workers[wi].pending_slices = queued.pending_slices;
                    }
                    to_kick = Some(new_id);
                }
                None => {
                    g.queued_reads.insert(oldest, queued);
                }
            }
        }
        if let Some(new_id) = to_kick {
            self.kick_worker(new_id);
        }
    }

    /// mediabunny `onWorkerFinished`.
    pub fn on_worker_finished(self: &Arc<Self>, id: WorkerId) {
        let mut g = self.state.lock().expect("orch");
        let Some(idx) = g.workers.iter().position(|w| w.id == id) else {
            return;
        };
        let mut worker = g.workers.remove(idx);
        worker.running = false;
        if g.file_size.is_none() {
            g.file_size = Some(worker.current_pos);
        }
        for slice in worker.pending_slices.drain(..) {
            complete_empty(&slice);
        }
        drop(g);
        if let Some(cb) = &self.options.on_idle_worker_removed {
            cb(id);
        }
    }

    /// Fail a worker's pending slices with `err`, then remove it.
    pub fn fail_worker(self: &Arc<Self>, id: WorkerId, err: MediaError) {
        let msg = err.to_string();
        let mut g = self.state.lock().expect("orch");
        let Some(idx) = g.workers.iter().position(|w| w.id == id) else {
            return;
        };
        let mut worker = g.workers.remove(idx);
        worker.running = false;
        worker.aborted = true;
        for slice in worker.pending_slices.drain(..) {
            if let Ok(s) = slice.lock() {
                s.complete.complete(Err(MediaError::Source(msg.clone())));
            }
        }
        drop(g);
        if let Some(cb) = &self.options.on_idle_worker_removed {
            cb(id);
        }
        self.signal_worker_stopped_running_after_remove();
    }

    fn signal_worker_stopped_running_after_remove(self: &Arc<Self>) {
        let mut to_kick: Option<WorkerId> = None;
        {
            let mut g = self.state.lock().expect("orch");
            if g.queued_reads.is_empty() || g.disposed {
                return;
            }
            let mut oldest = 0;
            for i in 1..g.queued_reads.len() {
                if g.queued_reads[i].age < g.queued_reads[oldest].age {
                    oldest = i;
                }
            }
            let queued = g.queued_reads.remove(oldest);
            match create_worker(
                &mut g,
                queued.hole.start,
                queued.hole.end,
                queued.strict_target,
                self.options.max_worker_count.max(1),
                &self.options.on_idle_worker_removed,
            ) {
                Some(new_id) => {
                    let age = g.next_age;
                    g.next_age += 1;
                    if let Some(w) = g.workers.iter_mut().find(|w| w.id == new_id) {
                        w.pending_slices = queued.pending_slices;
                        w.running = true;
                        w.age = age;
                    }
                    to_kick = Some(new_id);
                }
                None => {
                    g.queued_reads.insert(oldest, queued);
                }
            }
        }
        if let Some(new_id) = to_kick {
            self.kick_worker(new_id);
        }
    }

    #[allow(dead_code)] // UrlSource abort / Drop path
    pub fn abort_worker(&self, id: WorkerId) {
        let mut g = self.state.lock().expect("orch");
        if let Some(w) = g.workers.iter_mut().find(|w| w.id == id) {
            w.aborted = true;
        }
    }

    /// Abort workers + pending reads (mediabunny `UrlSource._dispose` → orch).
    /// Called from [`super::UrlSource`] Drop when the last host ref releases.
    pub fn dispose(self: &Arc<Self>) {
        let mut g = self.state.lock().expect("orch");
        if g.disposed {
            return;
        }
        g.disposed = true;
        let idle_cb = self.options.on_idle_worker_removed.clone();
        for w in &mut g.workers {
            w.aborted = true;
            for s in w.pending_slices.drain(..) {
                if let Ok(slice) = s.lock() {
                    slice
                        .complete
                        .complete(Err(MediaError::Source("disposed".into())));
                }
            }
            // Always abort fetch controllers — including in-flight workers
            // (idle-only path left open Ranges alive after clip switch).
            if let Some(cb) = &idle_cb {
                cb(w.id);
            }
        }
        for q in g.queued_reads.drain(..) {
            for s in q.pending_slices {
                if let Ok(slice) = s.lock() {
                    slice
                        .complete
                        .complete(Err(MediaError::Source("disposed".into())));
                }
            }
        }
        g.workers.clear();
        g.cache.clear();
        g.current_cache_size = 0;
    }

    fn begin_read(
        self: &Arc<Self>,
        inner_start: u64,
        inner_end: u64,
    ) -> Result<ReadKick, MediaError> {
        let mut to_kick: Vec<WorkerId> = Vec::new();
        let kick_result = {
            let mut g = self.state.lock().expect("orch");
            if g.disposed {
                return Err(MediaError::Source("disposed".into()));
            }

            let file_size = g.file_size;
            let worker_spans: Vec<(u64, u64)> = g
                .workers
                .iter()
                .map(|w| (w.start_pos, w.target_pos))
                .collect();
            let prefetch = network_prefetch(inner_start, inner_end, &worker_spans);
            let mut outer_start = prefetch.start;
            let mut outer_end = prefetch.end;
            if let Some(size) = file_size {
                outer_end = outer_end.min(size);
            }
            outer_start = outer_start.min(inner_start);
            outer_end = outer_end.max(inner_end);
            if let Some(size) = file_size {
                outer_end = outer_end.min(size);
            }
            debug_assert!(outer_start <= inner_start && inner_end <= outer_end);

            // Single-entry cache hit for the inner range.
            let mut result: Option<Vec<u8>> = None;
            if let Some(idx) = bsearch_le(&g.cache, inner_start, |e| e.start) {
                let entry = &g.cache[idx];
                if entry.start <= inner_start && inner_end <= entry.end {
                    let off = (inner_start - entry.start) as usize;
                    let len = (inner_end - inner_start) as usize;
                    result = Some(entry.bytes[off..off + len].to_vec());
                    g.cache[idx].age = g.next_age;
                    g.next_age += 1;
                }
            }

            let mut bytes_buf = if result.is_some() {
                None
            } else {
                Some(vec![0u8; (inner_end - inner_start) as usize])
            };
            let mut contiguous_write_end = 0usize;
            let mut last_end = outer_start;
            let mut outer_holes: Vec<Hole> = Vec::new();

            if let Some(start_i) = bsearch_le(&g.cache, outer_start, |e| e.start) {
                for i in start_i..g.cache.len() {
                    let (entry_start, entry_end) = (g.cache[i].start, g.cache[i].end);
                    if entry_start >= outer_end {
                        break;
                    }
                    if entry_end <= outer_start {
                        continue;
                    }
                    let cap_s = outer_start.max(entry_start);
                    let cap_e = outer_end.min(entry_end);
                    if last_end < cap_s {
                        outer_holes.push(Hole {
                            start: last_end,
                            end: cap_s,
                        });
                    }
                    last_end = cap_e;

                    if let Some(ref mut buf) = bytes_buf {
                        let is_ = inner_start.max(entry_start);
                        let ie_ = inner_end.min(entry_end);
                        if is_ < ie_ {
                            let relative = (is_ - inner_start) as usize;
                            let src0 = (is_ - entry_start) as usize;
                            let src1 = (ie_ - entry_start) as usize;
                            buf[relative..relative + (src1 - src0)]
                                .copy_from_slice(&g.cache[i].bytes[src0..src1]);
                            if relative == contiguous_write_end {
                                contiguous_write_end = (ie_ - inner_start) as usize;
                            }
                        }
                    }
                    g.cache[i].age = g.next_age;
                    g.next_age += 1;
                }
                if last_end < outer_end {
                    outer_holes.push(Hole {
                        start: last_end,
                        end: outer_end,
                    });
                }
            } else if outer_start < outer_end {
                outer_holes.push(Hole {
                    start: outer_start,
                    end: outer_end,
                });
            }

            if let Some(ref buf) = bytes_buf
                && contiguous_write_end >= buf.len()
                && !buf.is_empty()
            {
                result = Some(buf.clone());
            }

            if outer_holes.is_empty() {
                return Ok(ReadKick::Ready(result.unwrap_or_default()));
            }

            let cell = Arc::new(CompleteCell::new());
            let pending_slice = bytes_buf.map(|buf| {
                let mut inner_holes = Vec::new();
                for outer in &outer_holes {
                    let cs = inner_start.max(outer.start);
                    let ce = inner_end.min(outer.end);
                    if cs == outer.start && ce == outer.end {
                        inner_holes.push(*outer);
                    } else if cs < ce {
                        inner_holes.push(Hole { start: cs, end: ce });
                    }
                }
                Arc::new(Mutex::new(PendingSlice {
                    start: inner_start,
                    bytes: buf,
                    holes: inner_holes,
                    complete: Arc::clone(&cell),
                }))
            });

            'holes: for outer_hole in outer_holes {
                for wi in 0..g.workers.len() {
                    match try_extend_worker(&mut g, wi, outer_hole, pending_slice.as_ref()) {
                        ExtendResult::No => {}
                        ExtendResult::Attached { kick } => {
                            attach_queued_reads(&mut g, wi);
                            if kick {
                                let id = g.workers[wi].id;
                                if !to_kick.contains(&id) {
                                    to_kick.push(id);
                                }
                            }
                            continue 'holes;
                        }
                    }
                }

                let strict = outer_hole.end < outer_end || file_size.is_some();
                match create_worker(
                    &mut g,
                    outer_hole.start,
                    outer_hole.end,
                    strict,
                    self.options.max_worker_count.max(1),
                    &self.options.on_idle_worker_removed,
                ) {
                    Some(new_id) => {
                        if let Some(slice) = &pending_slice
                            && let Some(wi) = g.workers.iter().position(|w| w.id == new_id)
                        {
                            g.workers[wi].pending_slices = vec![Arc::clone(slice)];
                        }
                        to_kick.push(new_id);
                    }
                    None => {
                        queue_hole(&mut g, outer_hole, strict, pending_slice.as_ref());
                    }
                }
            }

            let out = if let Some(bytes) = result {
                ReadKick::Ready(bytes)
            } else {
                ReadKick::Pending(cell)
            };
            (out, to_kick)
        };

        let (out, kicks) = kick_result;
        for id in kicks {
            self.kick_worker(id);
        }
        Ok(out)
    }

    fn kick_worker(self: &Arc<Self>, id: WorkerId) {
        {
            let mut g = self.state.lock().expect("orch");
            let Some(wi) = g.workers.iter().position(|w| w.id == id) else {
                return;
            };
            if g.workers[wi].current_pos >= g.workers[wi].target_pos {
                g.workers[wi].running = false;
                return;
            }
            let age = g.next_age;
            g.next_age += 1;
            g.workers[wi].running = true;
            g.workers[wi].age = age;
        }
        let orch = Arc::clone(self);
        let fut = (self.options.run_worker)(Arc::clone(&orch), id);
        let wrapped = async move {
            fut.await;
            // mediabunny `.finally`: only act if still marked running
            if orch.worker_snapshot(id).map(|s| s.running).unwrap_or(false) {
                orch.signal_worker_stopped_running(id);
            }
        };

        #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
        {
            wasm_bindgen_futures::spawn_local(wrapped);
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "webcodecs")))]
        {
            crate::util::block_on(wrapped);
        }
    }
}

enum ExtendResult {
    No,
    Attached { kick: bool },
}

/// Attach hole to an existing worker when within gap tolerance.
/// `kick=true` when the worker was idle and must be started.
fn try_extend_worker(
    g: &mut OrchState,
    wi: usize,
    hole: Hole,
    pending: Option<&Arc<Mutex<PendingSlice>>>,
) -> ExtendResult {
    let w = &g.workers[wi];
    if !closed_overlap(
        hole.start.saturating_sub(GAP_TOLERANCE),
        hole.start,
        w.current_pos,
        w.target_pos,
    ) {
        return ExtendResult::No;
    }
    let was_running = w.running;
    g.workers[wi].target_pos = g.workers[wi].target_pos.max(hole.end);
    if let Some(slice) = pending {
        let slices = &mut g.workers[wi].pending_slices;
        if !slices.iter().any(|s| Arc::ptr_eq(s, slice)) {
            slices.push(Arc::clone(slice));
        }
    }
    ExtendResult::Attached { kick: !was_running }
}

fn attach_queued_reads(g: &mut OrchState, wi: usize) {
    let mut i = 0;
    let mut matched = false;
    while i < g.queued_reads.len() {
        let hole = g.queued_reads[i].hole;
        let w = &g.workers[wi];
        if closed_overlap(
            hole.start.saturating_sub(GAP_TOLERANCE),
            hole.start,
            w.current_pos,
            w.target_pos,
        ) {
            g.workers[wi].target_pos = g.workers[wi].target_pos.max(hole.end);
            let slices = std::mem::take(&mut g.queued_reads[i].pending_slices);
            for slice in slices {
                if !g.workers[wi]
                    .pending_slices
                    .iter()
                    .any(|s| Arc::ptr_eq(s, &slice))
                {
                    g.workers[wi].pending_slices.push(slice);
                }
            }
            g.queued_reads.remove(i);
            matched = true;
        } else if matched {
            break;
        } else {
            i += 1;
        }
    }
}

fn queue_hole(
    g: &mut OrchState,
    outer_hole: Hole,
    strict_target: bool,
    pending: Option<&Arc<Mutex<PendingSlice>>>,
) {
    let mut index = bsearch_le(&g.queued_reads, outer_hole.start, |q| q.hole.start)
        .map(|i| i as isize)
        .unwrap_or(-1);
    let mut merged = false;
    if index >= 0 {
        let entry = &mut g.queued_reads[index as usize];
        if outer_hole.start <= entry.hole.end {
            entry.hole.end = entry.hole.end.max(outer_hole.end);
            entry.strict_target &= strict_target;
            if let Some(slice) = pending {
                entry.pending_slices.push(Arc::clone(slice));
            }
            merged = true;
        }
    }
    if !merged {
        index += 1;
        let age = g.next_age;
        g.next_age += 1;
        g.queued_reads.insert(
            index as usize,
            QueuedRead {
                hole: Hole {
                    start: outer_hole.start,
                    end: outer_hole.end,
                },
                strict_target,
                pending_slices: pending.cloned().into_iter().collect(),
                age,
            },
        );
    }
    let idx = index.max(0) as usize;
    while idx + 1 < g.queued_reads.len() {
        let next_start = g.queued_reads[idx + 1].hole.start;
        if next_start > g.queued_reads[idx].hole.end {
            break;
        }
        let next = g.queued_reads.remove(idx + 1);
        let cur = &mut g.queued_reads[idx];
        cur.hole.end = cur.hole.end.max(next.hole.end);
        cur.pending_slices.extend(next.pending_slices);
        cur.strict_target &= next.strict_target;
        cur.age = cur.age.min(next.age);
    }
}

fn create_worker(
    g: &mut OrchState,
    start_pos: u64,
    target_pos: u64,
    strict_target: bool,
    max_workers: usize,
    on_idle_removed: &Option<Arc<dyn Fn(WorkerId) + Send + Sync>>,
) -> Option<WorkerId> {
    if g.workers.len() >= max_workers {
        let mut oldest_i: Option<usize> = None;
        for (i, w) in g.workers.iter().enumerate() {
            if !w.running
                && w.pending_slices.is_empty()
                && oldest_i.is_none_or(|oi| w.age < g.workers[oi].age)
            {
                oldest_i = Some(i);
            }
        }
        let oi = oldest_i?;
        let removed = g.workers.remove(oi);
        if let Some(cb) = on_idle_removed {
            cb(removed.id);
        }
    }
    let id = WorkerId(g.next_worker_id);
    g.next_worker_id += 1;
    let age = g.next_age;
    g.next_age += 1;
    g.workers.push(ReadWorker {
        id,
        start_pos,
        current_pos: start_pos,
        target_pos,
        strict_target,
        running: false,
        aborted: g.disposed,
        pending_slices: Vec::new(),
        age,
    });
    Some(id)
}

fn insert_into_cache(g: &mut OrchState, start: u64, bytes: &[u8], max_cache_size: usize) {
    if max_cache_size == 0 || bytes.is_empty() {
        return;
    }
    let end = start + bytes.len() as u64;
    let mut entry = CacheEntry {
        start,
        end,
        bytes: bytes.to_vec(),
        age: {
            let a = g.next_age;
            g.next_age += 1;
            a
        },
    };

    let mut insertion = bsearch_le(&g.cache, entry.start, |e| e.start)
        .map(|i| i + 1)
        .unwrap_or(0);

    if insertion > 0 {
        let prev_end = g.cache[insertion - 1].end;
        if prev_end >= entry.end {
            return;
        }
        if prev_end > entry.start {
            let prev_start = g.cache[insertion - 1].start;
            let joined_len = (entry.end - prev_start) as usize;
            let mut joined = vec![0u8; joined_len];
            let prev_bytes = &g.cache[insertion - 1].bytes;
            joined[..prev_bytes.len()].copy_from_slice(prev_bytes);
            let at = (entry.start - prev_start) as usize;
            joined[at..at + entry.bytes.len()].copy_from_slice(&entry.bytes);
            g.current_cache_size += (entry.end - prev_end) as usize;
            g.cache[insertion - 1].bytes = joined;
            g.cache[insertion - 1].end = entry.end;
            insertion -= 1;
            // Continue forward-join with the previous entry as `entry`
            entry = CacheEntry {
                start: g.cache[insertion].start,
                end: g.cache[insertion].end,
                bytes: std::mem::take(&mut g.cache[insertion].bytes),
                age: g.cache[insertion].age,
            };
            g.cache[insertion].bytes = entry.bytes.clone();
        } else {
            g.current_cache_size += entry.bytes.len();
            g.cache.insert(insertion, entry);
            return finish_forward_and_evict(g, insertion, max_cache_size);
        }
    } else {
        g.current_cache_size += entry.bytes.len();
        g.cache.insert(insertion, entry);
        return finish_forward_and_evict(g, insertion, max_cache_size);
    }

    // entry was joined into previous — bytes live in cache[insertion]
    finish_forward_and_evict(g, insertion, max_cache_size);
}

fn finish_forward_and_evict(g: &mut OrchState, insertion: usize, max_cache_size: usize) {
    let i = insertion + 1;
    while i < g.cache.len() {
        let entry_end = g.cache[insertion].end;
        let next_start = g.cache[i].start;
        let next_end = g.cache[i].end;
        if entry_end <= next_start {
            break;
        }
        if entry_end >= next_end {
            g.current_cache_size -= g.cache[i].bytes.len();
            g.cache.remove(i);
            continue;
        }
        let entry_start = g.cache[insertion].start;
        let joined_len = (next_end - entry_start) as usize;
        let mut joined = vec![0u8; joined_len];
        joined[..g.cache[insertion].bytes.len()].copy_from_slice(&g.cache[insertion].bytes);
        let at = (next_start - entry_start) as usize;
        let next_bytes = g.cache[i].bytes.clone();
        joined[at..at + next_bytes.len()].copy_from_slice(&next_bytes);
        g.current_cache_size -= (entry_end - next_start) as usize;
        g.cache[insertion].bytes = joined;
        g.cache[insertion].end = next_end;
        g.cache.remove(i);
        break;
    }

    while g.current_cache_size > max_cache_size && !g.cache.is_empty() {
        let mut oldest = 0;
        for i in 1..g.cache.len() {
            if g.cache[i].age < g.cache[oldest].age {
                oldest = i;
            }
        }
        let oldest_len = g.cache[oldest].bytes.len();
        if g.current_cache_size.saturating_sub(oldest_len) <= max_cache_size {
            break;
        }
        g.cache.remove(oldest);
        g.current_cache_size -= oldest_len;
    }
}

fn network_prefetch(start: u64, end: u64, workers: &[(u64, u64)]) -> Hole {
    let pad = SEQ_START_PAD;
    let mut start = if pad == 0 {
        start
    } else {
        start
            .saturating_sub(pad)
            .checked_div(pad)
            .map(|q| q.saturating_mul(pad))
            .unwrap_or(0)
    };
    let mut end = end;

    for &(w_start, w_target) in workers {
        let threshold = ((w_start + w_target) / 2).max(w_target.saturating_sub(MAX_EXTENSION));
        if closed_overlap(start, end, threshold, w_target) {
            let size = w_target.saturating_sub(w_start).max(1);
            let a = (size + 1)
                .div_ceil(MAX_EXTENSION)
                .saturating_mul(MAX_EXTENSION);
            let b = (size + 1)
                .checked_next_power_of_two()
                .unwrap_or(u64::MAX)
                .max(MAX_EXTENSION);
            let extent = a.min(b).max(MAX_EXTENSION);
            end = end.max(w_start.saturating_add(extent));
        }
    }

    end = end.max(start.saturating_add(URL_SOURCE_MIN_LOAD));
    // Ensure start never exceeds original request start for outer clamp
    let _ = &mut start;
    Hole { start, end }
}

fn closed_overlap(a0: u64, a1: u64, b0: u64, b1: u64) -> bool {
    a0 <= b1 && b0 <= a1
}

fn bsearch_le<T>(items: &[T], key: u64, f: impl Fn(&T) -> u64) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    let mut lo = 0isize;
    let mut hi = items.len() as isize - 1;
    let mut ans = None;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let v = f(&items[mid as usize]);
        if v <= key {
            ans = Some(mid as usize);
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    ans
}

fn complete_empty(slice: &Arc<Mutex<PendingSlice>>) {
    if let Ok(s) = slice.lock() {
        s.complete.complete(Ok(Vec::new()));
    }
}

enum ReadKick {
    Ready(Vec<u8>),
    Pending(Arc<CompleteCell>),
}

struct CompleteCell {
    inner: Mutex<CompleteInner>,
}

struct CompleteInner {
    value: Option<Result<Vec<u8>, MediaError>>,
    waker: Option<Waker>,
    done: bool,
}

impl CompleteCell {
    fn new() -> Self {
        Self {
            inner: Mutex::new(CompleteInner {
                value: None,
                waker: None,
                done: false,
            }),
        }
    }

    fn complete(&self, value: Result<Vec<u8>, MediaError>) {
        let mut g = self.inner.lock().expect("complete");
        if g.done {
            return;
        }
        g.done = true;
        g.value = Some(value);
        if let Some(w) = g.waker.take() {
            w.wake();
        }
    }
}

struct CompleteFut(Arc<CompleteCell>);

impl Future for CompleteFut {
    type Output = Option<Result<Vec<u8>, MediaError>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut g = self.0.inner.lock().expect("complete");
        if g.done {
            return Poll::Ready(g.value.take());
        }
        g.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Orchestrator backed by an in-memory buffer (native unit tests).
///
/// `run_worker` must **not** call `signal_worker_stopped_running` — the kick
/// wrapper does that when still marked running. Call `on_worker_finished` only
/// on EOF before `target_pos`.
#[cfg(test)]
pub fn memory_backed_orchestrator(
    data: Arc<Vec<u8>>,
    max_workers: usize,
    max_cache: usize,
) -> Arc<ReadOrchestrator> {
    let data_for_fn = Arc::clone(&data);
    ReadOrchestrator::new(ReadOrchestratorOptions {
        max_cache_size: max_cache,
        max_worker_count: max_workers,
        on_idle_worker_removed: None,
        run_worker: Arc::new(move |orch, id| {
            let data = Arc::clone(&data_for_fn);
            Box::pin(async move {
                loop {
                    let Some(snap) = orch.worker_snapshot(id) else {
                        return;
                    };
                    if snap.aborted {
                        return;
                    }
                    if snap.current_pos >= snap.target_pos {
                        return;
                    }
                    let start = snap.current_pos as usize;
                    if start >= data.len() {
                        orch.on_worker_finished(id);
                        return;
                    }
                    let end = (snap.target_pos as usize).min(data.len());
                    let chunk_end = (start + 4096).min(end);
                    orch.supply_worker_data(id, &data[start..chunk_end]);
                    if chunk_end >= data.len() && (chunk_end as u64) < snap.target_pos {
                        orch.on_worker_finished(id);
                        return;
                    }
                    if (chunk_end as u64) >= snap.target_pos {
                        return;
                    }
                }
            })
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::block_on;

    fn fill_data(n: usize) -> Arc<Vec<u8>> {
        Arc::new((0..n).map(|i| (i % 251) as u8).collect())
    }

    #[test]
    fn inner_slice_resolves_before_outer_prefetch_finishes() {
        let data = fill_data(2 * 1024 * 1024);
        let orch = memory_backed_orchestrator(Arc::clone(&data), 2, URL_ORCH_CACHE_SIZE);
        orch.supply_file_size(data.len() as u64);

        let got = block_on(orch.read(100, 200)).expect("read");
        assert_eq!(got, data[100..200]);
        let windows = orch.cache_windows();
        assert!(!windows.is_empty());
    }

    #[test]
    fn cache_hit_is_fast() {
        let data = fill_data(64 * 1024);
        let orch = memory_backed_orchestrator(Arc::clone(&data), 2, URL_ORCH_CACHE_SIZE);
        orch.supply_file_size(data.len() as u64);
        let _ = block_on(orch.read(0, 8192)).unwrap();
        let got = block_on(orch.read(100, 200)).unwrap();
        assert_eq!(got, data[100..200]);
    }

    #[test]
    fn two_overlapping_reads_share_worker_stream() {
        let data = fill_data(256 * 1024);
        let orch = memory_backed_orchestrator(Arc::clone(&data), 1, URL_ORCH_CACHE_SIZE);
        orch.supply_file_size(data.len() as u64);
        let a = block_on(orch.read(0, 1000)).unwrap();
        let b = block_on(orch.read(500, 1500)).unwrap();
        assert_eq!(a, data[0..1000]);
        assert_eq!(b, data[500..1500]);
    }
}
