//! HTTP [`UrlSource`] — ranged `fetch` for progressive remote open.
//!
//! mediabunny: `src/source.ts` `UrlSource` / `_runWorker` — probe with
//! `Range: bytes=…`, require `206` + `Content-Range` for random access; up to
//! `DEFAULT_PARALLELISM` (2) concurrent Range workers via [`UrlFetchGate`].
//! Full `ReadOrchestrator` hole-cache + open-ended body streaming + mid-file
//! sequential abandon are deferred — [`super::CachedSource`] covers play-time
//! coalescing; connect-time no-Range → host full-download ([`is_no_http_range`]).

use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestInit, Response};

use crate::error::MediaError;

use super::url_gate::{DEFAULT_URL_PARALLELISM, UrlFetchGate};
use super::{MediaSource, ReadFuture};

/// Options for [`UrlSource::connect_with`].
#[derive(Clone, Debug)]
pub struct UrlSourceOptions {
    /// Max concurrent Range GETs (`mediabunny: UrlSource.parallelism`).
    pub parallelism: usize,
}

impl Default for UrlSourceOptions {
    fn default() -> Self {
        Self {
            parallelism: DEFAULT_URL_PARALLELISM,
        }
    }
}

/// Ranged HTTP(S) byte source (progressive open without full download).
#[derive(Clone, Debug)]
pub struct UrlSource {
    url: String,
    size: u64,
    gate: UrlFetchGate,
}

impl UrlSource {
    /// Probe `url` with `Range: bytes=0-0` and build a source when the server
    /// answers `206` with a total size in `Content-Range`.
    ///
    /// Returns [`MediaError::Source`] with a stable `no_http_range:` prefix when
    /// the server ignores Range (host may fall back to a full download).
    pub async fn connect(url: impl Into<String>) -> Result<Self, MediaError> {
        Self::connect_with(url, UrlSourceOptions::default()).await
    }

    /// Like [`connect`](Self::connect) with explicit [`UrlSourceOptions`].
    pub async fn connect_with(
        url: impl Into<String>,
        options: UrlSourceOptions,
    ) -> Result<Self, MediaError> {
        let url = url.into();
        let resp = range_get(&url, 0, 0).await?;
        let status = resp.status();
        let gate = UrlFetchGate::new(options.parallelism);

        if status == 206 {
            let size = parse_total_size_from_content_range(&resp)?.ok_or_else(|| {
                MediaError::Source("no_http_range: 206 without total size in Content-Range".into())
            })?;
            web_sys::console::log_1(
                &format!(
                    "webmedia: UrlSource connect 206 size={size} parallelism={} {url}",
                    gate.max()
                )
                .into(),
            );
            return Ok(Self { url, size, gate });
        }

        if status == 200 {
            // Server ignored Range — cannot random-access without streaming the body.
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

    /// Max concurrent Range GETs for this source.
    pub fn parallelism(&self) -> usize {
        self.gate.max()
    }
}

impl MediaSource for UrlSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        let url = self.url.clone();
        let size = self.size;
        let gate = self.gate.clone();
        Box::pin(async move {
            if offset > size {
                return Err(MediaError::ReadOutOfRange { offset, len, size });
            }
            let available = (size - offset) as usize;
            let want = len.min(available);
            if want == 0 {
                return Ok(Vec::new());
            }

            let end = offset + want as u64 - 1;
            web_sys::console::log_1(
                &format!("webmedia: UrlSource Range bytes={offset}-{end}").into(),
            );

            gate.with_slot(async { range_read_bytes(&url, offset, end, want).await })
                .await
        })
    }
}

async fn range_read_bytes(
    url: &str,
    offset: u64,
    end: u64,
    want: usize,
) -> Result<Vec<u8>, MediaError> {
    // Transient CDN / proxy errors (GitHub Releases via /proxy) — brief retry.
    let mut last_err = String::new();
    for attempt in 0..4u32 {
        if attempt > 0 {
            crate::util::yield_now().await;
            crate::util::yield_now().await;
        }
        let resp = match range_get(url, offset, end).await {
            Ok(r) => r,
            Err(e) => {
                last_err = e.to_string();
                if last_err.contains("timed out") {
                    continue;
                }
                return Err(e);
            }
        };
        let status = resp.status();
        if status == 206 || status == 200 {
            // A 200 on a mid-file Range means the server dumped the whole object —
            // refuse rather than allocating multi‑100 MB into WASM.
            if status == 200 && offset > 0 {
                return Err(MediaError::Source(
                    "no_http_range: mid-file read got HTTP 200 (Range ignored)".into(),
                ));
            }

            let buf = response_bytes(&resp).await?;
            if status == 200 && offset == 0 && buf.len() > want {
                return Ok(buf[..want].to_vec());
            }
            if buf.len() > want {
                return Ok(buf[..want].to_vec());
            }
            return Ok(buf);
        }
        last_err = format!("HTTP {status}");
        if status != 502 && status != 503 && status != 429 {
            break;
        }
    }

    Err(MediaError::Source(format!(
        "{last_err} reading bytes={offset}-{end}"
    )))
}

async fn range_get(url: &str, start: u64, end: u64) -> Result<Response, MediaError> {
    let window = web_sys::window().ok_or_else(|| MediaError::Source("no window".into()))?;
    let headers = Headers::new().map_err(js_to_source)?;
    headers
        .set("Range", &format!("bytes={start}-{end}"))
        .map_err(js_to_source)?;

    let controller = web_sys::AbortController::new().map_err(js_to_source)?;
    let signal = controller.signal();

    let init = RequestInit::new();
    init.set_method("GET");
    init.set_headers(&headers);
    init.set_signal(Some(&signal));

    let request = Request::new_with_str_and_init(url, &init).map_err(js_to_source)?;
    let fetch_promise = window.fetch_with_request(&request);

    // Scale with payload: 43 MiB through GitHub needs ~60 s budget, not 15 s.
    // Floor 15 s (small Ranges); cap 180 s.
    let nbytes = end.saturating_sub(start).saturating_add(1);
    let timeout_ms = (15_000i64 + (nbytes as i64 / 1_000)).clamp(15_000, 180_000) as i32;
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
                "Range bytes={start}-{end} timed out ({timeout_ms}ms)"
            ))
        } else {
            MediaError::Source(msg)
        }
    })?;

    if resp_val.is_null() || resp_val.is_undefined() {
        return Err(MediaError::Source(format!(
            "Range bytes={start}-{end} timed out ({timeout_ms}ms)"
        )));
    }

    resp_val
        .dyn_into::<Response>()
        .map_err(|_| MediaError::Source("fetch did not return a Response".into()))
}

async fn response_bytes(resp: &Response) -> Result<Vec<u8>, MediaError> {
    let buf_promise = resp.array_buffer().map_err(js_to_source)?;
    let buf = JsFuture::from(buf_promise).await.map_err(js_to_source)?;
    Ok(Uint8Array::new(&buf).to_vec())
}

/// `Content-Range: bytes <start>-<end>/<total|"*">` → total when numeric.
fn parse_total_size_from_content_range(resp: &Response) -> Result<Option<u64>, MediaError> {
    let Some(cr) = resp.headers().get("content-range").map_err(js_to_source)? else {
        return Ok(None);
    };
    // Prefer the `/total` suffix (mediabunny: `/\/(\d+)/`).
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
