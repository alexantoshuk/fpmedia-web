//! HTTP [`UrlSource`] — ranged `fetch` for progressive remote open.
//!
//! mediabunny: `src/source.ts` `UrlSource` / `_runWorker` — probe with
//! `Range: bytes=…`, require `206` + `Content-Range` for random access; each
//! [`MediaSource::read`] is a bounded Range GET. Full orchestrator / sequential
//! fallback / parallel workers are deferred; [`super::CachedSource`] covers
//! play-time coalescing after demux.

use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestInit, Response};

use crate::error::MediaError;

use super::{MediaSource, ReadFuture};

/// Ranged HTTP(S) byte source (progressive open without full download).
#[derive(Clone, Debug)]
pub struct UrlSource {
    url: String,
    size: u64,
}

impl UrlSource {
    /// Probe `url` with `Range: bytes=0-0` and build a source when the server
    /// answers `206` with a total size in `Content-Range`.
    ///
    /// Returns [`MediaError::Source`] with a stable `no_http_range:` prefix when
    /// the server ignores Range (host may fall back to a full download).
    pub async fn connect(url: impl Into<String>) -> Result<Self, MediaError> {
        let url = url.into();
        let resp = range_get(&url, 0, 0).await?;
        let status = resp.status();

        if status == 206 {
            let size = parse_total_size_from_content_range(&resp)?.ok_or_else(|| {
                MediaError::Source("no_http_range: 206 without total size in Content-Range".into())
            })?;
            web_sys::console::log_1(
                &format!("webmedia: UrlSource connect 206 size={size} {url}").into(),
            );
            return Ok(Self { url, size });
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
}

impl MediaSource for UrlSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&self, offset: u64, len: usize) -> ReadFuture<'_> {
        let url = self.url.clone();
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

            let end = offset + want as u64 - 1;
            web_sys::console::log_1(
                &format!("webmedia: UrlSource Range bytes={offset}-{end}").into(),
            );

            let resp = range_get(&url, offset, end).await?;
            let status = resp.status();
            if status != 206 && status != 200 {
                return Err(MediaError::Source(format!(
                    "HTTP {status} reading bytes={offset}-{end}"
                )));
            }

            // A 200 on a mid-file Range means the server dumped the whole object —
            // refuse rather than allocating multi‑100 MB into WASM.
            if status == 200 && offset > 0 {
                return Err(MediaError::Source(
                    "no_http_range: mid-file read got HTTP 200 (Range ignored)".into(),
                ));
            }

            let buf = response_bytes(&resp).await?;
            if status == 200 && offset == 0 && buf.len() > want {
                // Probe said 206 earlier but this read returned the full body —
                // take only the requested prefix (rare); prefer not to keep the rest.
                return Ok(buf[..want].to_vec());
            }
            if buf.len() > want {
                return Ok(buf[..want].to_vec());
            }
            Ok(buf)
        })
    }
}

async fn range_get(url: &str, start: u64, end: u64) -> Result<Response, MediaError> {
    let window = web_sys::window().ok_or_else(|| MediaError::Source("no window".into()))?;
    let headers = Headers::new().map_err(js_to_source)?;
    headers
        .set("Range", &format!("bytes={start}-{end}"))
        .map_err(js_to_source)?;

    let init = RequestInit::new();
    init.set_method("GET");
    init.set_headers(&headers);

    let request = Request::new_with_str_and_init(url, &init).map_err(js_to_source)?;
    let resp_val = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| MediaError::Source(format_js_fetch_err(e)))?;
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
