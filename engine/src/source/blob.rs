//! Browser [`web_sys::Blob`] / `File` source via mediabunny `ReadOrchestrator`.
//!
//! mediabunny: `src/source.ts` `BlobSource` — `maxWorkerCount: 4`,
//! `maxCacheSize ?? 8 MiB`, `PREFETCH_PROFILES.fileSystem`; `_runWorker` keeps
//! one `blob.slice(pos).stream()` reader per worker (sequential, file stays
//! open). WebKit (`isWebKit`) falls back to `slice(cur, target).arrayBuffer()`.
//! A per-sample `arrayBuffer()` re-opens the file on Android (IPC) and
//! starves Play.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, ReadableStreamDefaultReader};

use crate::error::MediaError;

use super::orchestrator::{PrefetchProfile, ReadOrchestrator, ReadOrchestratorOptions, WorkerId};
use super::{MediaSource, ReadFuture};

/// mediabunny `BlobSource` `maxWorkerCount`.
const BLOB_MAX_WORKERS: usize = 4;
/// mediabunny `BlobSource` `maxCacheSize` default.
const BLOB_ORCH_CACHE_SIZE: usize = 8 * 1024 * 1024;

/// Stream readers kept per worker (mediabunny `_readers` WeakMap).
type ReaderMap = Arc<Mutex<HashMap<WorkerId, ReadableStreamDefaultReader>>>;

/// Ranged reads from a browser [`Blob`] (including `File`).
///
/// Not [`Clone`] — share via [`std::sync::Arc`]. Drop disposes the orchestrator.
#[derive(Debug)]
pub struct BlobSource {
    blob: Blob,
    orch: Arc<ReadOrchestrator>,
}

impl Drop for BlobSource {
    fn drop(&mut self) {
        self.orch.dispose();
    }
}

impl BlobSource {
    pub fn new(blob: Blob) -> Self {
        let orch = build_orchestrator(blob.clone(), !is_webkit());
        orch.supply_file_size(blob.size() as u64);
        Self { blob, orch }
    }

    pub fn blob(&self) -> &Blob {
        &self.blob
    }
}

impl MediaSource for BlobSource {
    fn size(&self) -> u64 {
        self.blob.size() as u64
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        let orch = Arc::clone(&self.orch);
        let size = self.size();
        Box::pin(async move {
            if offset > size {
                return Err(MediaError::ReadOutOfRange { offset, len, size });
            }
            let want = len.min((size - offset) as usize);
            if want == 0 {
                return Ok(Bytes::new());
            }
            // Do not console.log every read — floods the main thread on play.
            orch.read(offset, offset + want as u64).await
        })
    }

    fn dispose(&self) {
        self.orch.dispose();
    }
}

fn build_orchestrator(blob: Blob, use_stream: bool) -> Arc<ReadOrchestrator> {
    let readers: ReaderMap = Arc::new(Mutex::new(HashMap::new()));
    let readers_for_idle = Arc::clone(&readers);
    ReadOrchestrator::new(ReadOrchestratorOptions {
        max_cache_size: BLOB_ORCH_CACHE_SIZE,
        max_worker_count: BLOB_MAX_WORKERS,
        prefetch_profile: PrefetchProfile::FileSystem,
        on_idle_worker_removed: Some(Arc::new(move |id| {
            // mediabunny: cancel the idle worker's reader or memory leaks.
            if let Some(reader) = readers_for_idle.lock().ok().and_then(|mut m| m.remove(&id)) {
                let _ = reader.cancel();
            }
        })),
        run_worker: Arc::new(move |orch, id| {
            let blob = blob.clone();
            let readers = Arc::clone(&readers);
            Box::pin(async move {
                run_worker(orch, id, &blob, &readers, use_stream).await;
            })
        }),
    })
}

/// mediabunny `BlobSource._runWorker`.
async fn run_worker(
    orch: Arc<ReadOrchestrator>,
    id: WorkerId,
    blob: &Blob,
    readers: &ReaderMap,
    use_stream: bool,
) {
    loop {
        let Some(snap) = orch.worker_snapshot(id) else {
            return;
        };
        if snap.aborted {
            if let Some(reader) = readers.lock().ok().and_then(|mut m| m.remove(&id)) {
                let _ = reader.cancel();
            }
            return;
        }
        if snap.current_pos >= snap.target_pos {
            return;
        }

        let chunk = if use_stream {
            let reader = match stream_reader(blob, readers, id, snap.current_pos) {
                Ok(r) => r,
                Err(e) => {
                    orch.fail_worker(id, e);
                    return;
                }
            };
            let result = match JsFuture::from(reader.read()).await {
                Ok(v) => v,
                Err(e) => {
                    orch.fail_worker(id, MediaError::from(e));
                    return;
                }
            };
            let done = js_sys::Reflect::get(&result, &"done".into())
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if done {
                // Stream ended before `target_pos` (file size is known, so only
                // a truncated Blob gets here) — resolve pending slices short.
                if let Some(reader) = readers.lock().ok().and_then(|mut m| m.remove(&id)) {
                    let _ = reader.cancel();
                }
                orch.on_worker_finished(id);
                return;
            }
            match js_sys::Reflect::get(&result, &"value".into()) {
                Ok(v) => Uint8Array::new(&v).to_vec(),
                Err(_) => continue,
            }
        } else {
            let slice = match blob
                .slice_with_f64_and_f64(snap.current_pos as f64, snap.target_pos as f64)
            {
                Ok(s) => s,
                Err(e) => {
                    orch.fail_worker(id, MediaError::from(e));
                    return;
                }
            };
            match JsFuture::from(slice.array_buffer()).await {
                Ok(buf) => Uint8Array::new(&buf).to_vec(),
                Err(e) => {
                    orch.fail_worker(id, MediaError::from(e));
                    return;
                }
            }
        };

        if orch.worker_snapshot(id).is_none_or(|s| s.aborted) {
            continue;
        }
        if !chunk.is_empty() {
            orch.supply_worker_data(id, &chunk);
        }
    }
}

/// Reader for worker `id`, opened at `pos` on first use and kept for later runs.
fn stream_reader(
    blob: &Blob,
    readers: &ReaderMap,
    id: WorkerId,
    pos: u64,
) -> Result<ReadableStreamDefaultReader, MediaError> {
    let mut map = readers
        .lock()
        .map_err(|_| MediaError::Source("blob readers lock poisoned".into()))?;
    if let Some(r) = map.get(&id) {
        return Ok(r.clone());
    }
    let reader: ReadableStreamDefaultReader = blob
        .slice_with_f64(pos as f64)
        .map_err(MediaError::from)?
        .stream()
        .get_reader()
        .dyn_into()
        .map_err(|_| MediaError::Source("blob.stream().getReader failed".into()))?;
    map.insert(id, reader.clone());
    Ok(reader)
}

/// mediabunny `isWebKit` — Safari and every iOS browser (WebKit `blob.stream()`
/// bugs: large-file errors, memory build-up, backpressure stalls).
fn is_webkit() -> bool {
    let ua = web_sys::window()
        .and_then(|w| js_sys::Reflect::get(&w, &"navigator".into()).ok())
        .and_then(|n| js_sys::Reflect::get(&n, &"userAgent".into()).ok())
        .and_then(|u| u.as_string())
        .unwrap_or_default();
    ua.contains("AppleWebKit") && !ua.contains("Chrome/") && !ua.contains("Chromium/")
}
