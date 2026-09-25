//! HTTP [`UrlSource`] — ranged `fetch` via mediabunny `ReadOrchestrator`.
//!
//! mediabunny: `src/source.ts` `UrlSource` / `_runWorker` + `ReadOrchestrator`:
//! open-ended `Range: bytes={pos}-`, stream body chunks into
//! `supplyWorkerData`, resolve **inner** slices when their holes fill (outer
//! prefetch may still be running). `maxWorkerCount` = `parallelism` (default 2).
//!
//! no-Range servers still fail connect with `no_http_range:` (host full-download);
//! sequential-mode / `ReadableStreamSource` stays deferred (`.ai/state.md`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AbortController, Headers, ReadableStreamDefaultReader, Request, RequestInit, Response,
};

use crate::error::MediaError;

use super::orchestrator::{
    ReadOrchestrator, ReadOrchestratorOptions, URL_ORCH_CACHE_SIZE, WorkerId,
};
use super::url_gate::DEFAULT_URL_PARALLELISM;
use super::{MediaSource, ReadFuture};

/// Options for [`UrlSource::connect_with`].
#[derive(Clone, Debug)]
pub struct UrlSourceOptions {
    /// Max concurrent Range workers (`mediabunny: UrlSource.parallelism`).
    pub parallelism: usize,
}

impl Default for UrlSourceOptions {
    fn default() -> Self {
        Self {
            parallelism: DEFAULT_URL_PARALLELISM,
        }
    }
}

/// Abort controllers keyed by worker (mediabunny `_abortControllers`).
type AbortMap = Arc<Mutex<HashMap<WorkerId, AbortController>>>;

/// Ranged HTTP(S) byte source (progressive open without full download).
#[derive(Clone, Debug)]
pub struct UrlSource {
    url: String,
    size: u64,
    orch: Arc<ReadOrchestrator>,
}

impl UrlSource {
    /// Probe `url` with `Range: bytes=0-0` and build a source when the server
    /// answers `206` with a total size in `Content-Range`.
    pub async fn connect(url: impl Into<String>) -> Result<Self, MediaError> {
        Self::connect_with(url, UrlSourceOptions::default()).await
    }

    /// Like [`connect`](Self::connect) with explicit [`UrlSourceOptions`].
    pub async fn connect_with(
        url: impl Into<String>,
        options: UrlSourceOptions,
    ) -> Result<Self, MediaError> {
        let url = url.into();
        let resp = range_get_closed(&url, 0, 0).await?;
        let status = resp.status();

        if status == 206 {
            let size = parse_total_size_from_content_range(&resp)?.ok_or_else(|| {
                MediaError::Source("no_http_range: 206 without total size in Content-Range".into())
            })?;
            let parallelism = options.parallelism.max(1);
            web_sys::console::log_1(
                &format!(
                    "webmedia: UrlSource connect 206 size={size} parallelism={parallelism} {url}"
                )
                .into(),
            );
            let orch = build_orchestrator(url.clone(), parallelism);
            orch.supply_file_size(size);
            return Ok(Self { url, size, orch });
        }

        if status == 200 {
            let len = resp
                .headers()
                .get("content-length")
                .ok()
                .flatten()
                .and_then(|s| s.parse::<u64>().ok());
            return Err(MediaError::Source(format!(
                "no_http_range: server returned 200 (Range ignored){}",
                len.map(|n| format!(" content-length={n}"))
                    .unwrap_or_default()
            )));
        }

        Err(MediaError::Source(format!(
            "HTTP {status} probing {}",
            shorten(&url)
        )))
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Max concurrent Range workers for this source.
    pub fn parallelism(&self) -> usize {
        self.orch.max_worker_count()
    }
}

impl MediaSource for UrlSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        let orch = Arc::clone(&self.orch);
        let size = self.size;
        Box::pin(async move {
            if offset > size {
                return Err(MediaError::ReadOutOfRange { offset, len, size });
            }
            let available = (size - offset) as usize;
            let want = len.min(available);
            if want == 0 {
                return Ok(Vec::new());
            }
            let end = offset + want as u64;
            web_sys::console::log_1(
                &format!("webmedia: UrlSource orch read bytes={offset}..{end}").into(),
            );
            orch.read(offset, end).await
        })
    }
}

fn build_orchestrator(url: String, parallelism: usize) -> Arc<ReadOrchestrator> {
    let aborts: AbortMap = Arc::new(Mutex::new(HashMap::new()));
    let aborts_for_run = Arc::clone(&aborts);
    let aborts_for_idle = Arc::clone(&aborts);
    let url_for_run = url;

    ReadOrchestrator::new(ReadOrchestratorOptions {
        max_cache_size: URL_ORCH_CACHE_SIZE,
        max_worker_count: parallelism,
        on_idle_worker_removed: Some(Arc::new(move |id| {
            if let Ok(mut m) = aborts_for_idle.lock()
                && let Some(ctrl) = m.remove(&id)
            {
                ctrl.abort();
            }
        })),
        run_worker: Arc::new(move |orch, id| {
            let url = url_for_run.clone();
            let aborts = Arc::clone(&aborts_for_run);
            Box::pin(async move {
                run_worker_loop(orch, id, &url, &aborts).await;
            })
        }),
    })
}

/// mediabunny `UrlSource._runWorker` — open-ended Range + body stream.
async fn run_worker_loop(orch: Arc<ReadOrchestrator>, id: WorkerId, url: &str, aborts: &AbortMap) {
    // Outer loop: resume if the response dies mid-stream (mediabunny while(true)).
    loop {
        let Some(snap) = orch.worker_snapshot(id) else {
            return;
        };
        if snap.aborted {
            orch.signal_worker_stopped_running(id);
            return;
        }

        let controller = match AbortController::new() {
            Ok(c) => c,
            Err(e) => {
                reject_worker(&orch, id, js_to_source(e));
                return;
            }
        };
        if let Ok(mut m) = aborts.lock() {
            m.insert(id, controller.clone());
        }

        let start = snap.current_pos;
        let resp = match range_get_open(url, start, &controller).await {
            Ok(r) => r,
            Err(_e) => {
                if controller.signal().aborted() {
                    orch.signal_worker_stopped_running(id);
                    return;
                }
                // Brief retry then fail the pending slices.
                crate::util::yield_now().await;
                match range_get_open(url, start, &controller).await {
                    Ok(r) => r,
                    Err(e2) => {
                        reject_worker(&orch, id, e2);
                        return;
                    }
                }
            }
        };

        let status = resp.status();
        if !resp.ok() {
            // Retry transient CDN errors.
            if matches!(status, 502 | 503 | 429) {
                crate::util::yield_now().await;
                continue;
            }
            reject_worker(
                &orch,
                id,
                MediaError::Source(format!("HTTP {status} open Range bytes={start}-")),
            );
            return;
        }

        if status != 206 {
            // no-Range mid-flight — refuse (host used connect probe; do not
            // download whole object into WASM). Sequential mode deferred.
            let _ = resp.body().map(|b| b.cancel());
            reject_worker(
                &orch,
                id,
                MediaError::Source(
                    "no_http_range: mid-file open Range got non-206 (Range ignored)".into(),
                ),
            );
            return;
        }

        let Some(body) = resp.body() else {
            reject_worker(
                &orch,
                id,
                MediaError::Source("Missing HTTP response body stream".into()),
            );
            return;
        };

        let reader: ReadableStreamDefaultReader = match body.get_reader().dyn_into() {
            Ok(r) => r,
            Err(_) => {
                reject_worker(
                    &orch,
                    id,
                    MediaError::Source("body.getReader failed".into()),
                );
                return;
            }
        };

        loop {
            let Some(snap) = orch.worker_snapshot(id) else {
                controller.abort();
                return;
            };
            if snap.current_pos >= snap.target_pos || snap.aborted {
                controller.abort();
                orch.signal_worker_stopped_running(id);
                return;
            }

            let read_result = match JsFuture::from(reader.read()).await {
                Ok(v) => v,
                Err(e) => {
                    if controller.signal().aborted() {
                        orch.signal_worker_stopped_running(id);
                        return;
                    }
                    // Resume outer loop with a new open Range (mediabunny retry).
                    web_sys::console::warn_1(
                        &format!(
                            "webmedia: UrlSource stream error, resume @{}: {}",
                            snap.current_pos,
                            format_js_fetch_err(e)
                        )
                        .into(),
                    );
                    crate::util::yield_now().await;
                    break;
                }
            };

            let done = js_sys::Reflect::get(&read_result, &"done".into())
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            if done {
                let Some(snap) = orch.worker_snapshot(id) else {
                    return;
                };
                if snap.current_pos >= snap.target_pos {
                    orch.signal_worker_stopped_running(id);
                    return;
                }
                if snap.strict_target {
                    // Server capped the open Range early — new request.
                    break;
                }
                orch.on_worker_finished(id);
                return;
            }

            let value = js_sys::Reflect::get(&read_result, &"value".into()).ok();
            let Some(value) = value else {
                continue;
            };
            let chunk = Uint8Array::new(&value).to_vec();
            if chunk.is_empty() {
                continue;
            }
            orch.supply_worker_data(id, &chunk);
        }
    }
}

fn reject_worker(orch: &Arc<ReadOrchestrator>, id: WorkerId, err: MediaError) {
    web_sys::console::error_1(&format!("webmedia: UrlSource worker {id:?} err: {err}").into());
    orch.fail_worker(id, err);
}

async fn range_get_open(
    url: &str,
    start: u64,
    controller: &AbortController,
) -> Result<Response, MediaError> {
    let window = web_sys::window().ok_or_else(|| MediaError::Source("no window".into()))?;
    let headers = Headers::new().map_err(js_to_source)?;
    // mediabunny: open-ended `bytes=${worker.currentPos}-`
    headers
        .set("Range", &format!("bytes={start}-"))
        .map_err(js_to_source)?;

    let init = RequestInit::new();
    init.set_method("GET");
    init.set_headers(&headers);
    init.set_signal(Some(&controller.signal()));

    let request = Request::new_with_str_and_init(url, &init).map_err(js_to_source)?;
    let fetch_promise = window.fetch_with_request(&request);

    // Long streams: floor 30 s to first byte; cap 180 s (body read has its own life).
    let timeout_ms = 180_000i32;
    let timeout = js_sys::Promise::new(&mut |resolve, _reject| {
        let ctrl = controller.clone();
        let resolve_ok = resolve.clone();
        let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
            ctrl.abort();
            let _ = resolve_ok.call0(&wasm_bindgen::JsValue::NULL);
        });
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                timeout_ms,
            )
            .is_err()
        {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        }
    });

    let raced = js_sys::Promise::race(&js_sys::Array::of2(&fetch_promise, &timeout));
    let resp_val = JsFuture::from(raced).await.map_err(|e| {
        let msg = format_js_fetch_err(e);
        if msg.to_ascii_lowercase().contains("abort") {
            MediaError::Source(format!(
                "open Range bytes={start}- timed out ({timeout_ms}ms)"
            ))
        } else {
            MediaError::Source(msg)
        }
    })?;

    if resp_val.is_null() || resp_val.is_undefined() {
        return Err(MediaError::Source(format!(
            "open Range bytes={start}- timed out ({timeout_ms}ms)"
        )));
    }

    resp_val
        .dyn_into::<Response>()
        .map_err(|_| MediaError::Source("fetch did not return a Response".into()))
}

/// Closed Range used only for the connect probe (`bytes=0-0`).
async fn range_get_closed(url: &str, start: u64, end: u64) -> Result<Response, MediaError> {
    let window = web_sys::window().ok_or_else(|| MediaError::Source("no window".into()))?;
    let headers = Headers::new().map_err(js_to_source)?;
    headers
        .set("Range", &format!("bytes={start}-{end}"))
        .map_err(js_to_source)?;

    let controller = AbortController::new().map_err(js_to_source)?;
    let init = RequestInit::new();
    init.set_method("GET");
    init.set_headers(&headers);
    init.set_signal(Some(&controller.signal()));

    let request = Request::new_with_str_and_init(url, &init).map_err(js_to_source)?;
    let fetch_promise = window.fetch_with_request(&request);
    let timeout_ms = 15_000i32;
    let timeout = js_sys::Promise::new(&mut |resolve, _reject| {
        let ctrl = controller.clone();
        let resolve_ok = resolve.clone();
        let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
            ctrl.abort();
            let _ = resolve_ok.call0(&wasm_bindgen::JsValue::NULL);
        });
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                timeout_ms,
            )
            .is_err()
        {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        }
    });

    let raced = js_sys::Promise::race(&js_sys::Array::of2(&fetch_promise, &timeout));
    let resp_val = JsFuture::from(raced).await.map_err(js_to_source)?;
    if resp_val.is_null() || resp_val.is_undefined() {
        return Err(MediaError::Source(format!(
            "Range bytes={start}-{end} timed out ({timeout_ms}ms)"
        )));
    }
    resp_val
        .dyn_into::<Response>()
        .map_err(|_| MediaError::Source("fetch did not return a Response".into()))
}

fn parse_total_size_from_content_range(resp: &Response) -> Result<Option<u64>, MediaError> {
    let Some(cr) = resp.headers().get("content-range").map_err(js_to_source)? else {
        return Ok(None);
    };
    let Some((_, total)) = cr.rsplit_once('/') else {
        return Ok(None);
    };
    if total == "*" {
        return Ok(None);
    }
    total
        .parse::<u64>()
        .map(Some)
        .map_err(|_| MediaError::Source(format!("bad Content-Range total: {cr}")))
}

fn js_to_source(err: wasm_bindgen::JsValue) -> MediaError {
    MediaError::Source(format_js_fetch_err(err))
}

fn format_js_fetch_err(err: wasm_bindgen::JsValue) -> String {
    if let Some(s) = err.as_string() {
        return s;
    }
    let msg = js_sys::Reflect::get(&err, &"message".into())
        .ok()
        .and_then(|v| v.as_string());
    msg.unwrap_or_else(|| format!("{err:?}"))
}

fn shorten(url: &str) -> String {
    const MAX: usize = 96;
    if url.len() <= MAX {
        url.to_string()
    } else {
        format!("{}…", &url[..MAX])
    }
}

/// True when progressive Range open is unavailable (host may full-fetch).
pub fn is_no_http_range(err: &MediaError) -> bool {
    matches!(err, MediaError::Source(msg) if msg.starts_with("no_http_range:"))
}
