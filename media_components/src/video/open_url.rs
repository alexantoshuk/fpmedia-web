//! URL open path — `/proxy` rewrite, Range probe, full-download fallback, staged open.

use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{HtmlCanvasElement, Response};
use webmedia::{
    BlobSource, MediaError, MediaSource, OPEN_URL_PARALLELISM, OpenMp4Options, UrlSource,
    UrlSourceOptions, VideoPlayer, is_no_http_range, open_mp4_media_source_cached_with_progress,
};

/// Resolve `https:` via [`UrlSource`] (HTTP Range); `blob:` / no-Range hosts via
/// full fetch → [`BlobSource`]. Status line only — no confirm dialogs.
///
/// Remote `http(s):` URLs are rewritten to same-origin `/proxy?url=…` when the
/// page has a `window` (demo Axum shell) so GitHub Release / S3 CORS does not
/// block Range GETs.
pub(super) fn same_origin_media_proxy_url(remote: &str) -> Option<String> {
    let trimmed = remote.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("https:") || lower.starts_with("http:")) {
        return None;
    }
    if trimmed.contains("/proxy?") {
        return None;
    }
    let window = web_sys::window()?;
    let origin = window.location().origin().ok()?;
    if trimmed.starts_with(&origin) {
        return None;
    }
    let enc = String::from(js_sys::encode_uri_component(trimmed));
    Some(format!("{origin}/proxy?url={enc}"))
}

pub(super) async fn resolve_url_source(
    url: &str,
    on_status: impl Fn(&str),
) -> Result<std::sync::Arc<dyn MediaSource>, String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("blob:") {
        on_status("Opening blob…");
        let blob = fetch_url_as_blob(url).await?;
        return Ok(std::sync::Arc::new(BlobSource::new(blob)));
    }
    if !(lower.starts_with("https:") || lower.starts_with("http:")) {
        return Err(format!(
            "unsupported URL scheme (need https:, http:, or blob:): {url}"
        ));
    }

    on_status("Probing HTTP Range…");
    // mediabunny DEFAULT_PARALLELISM=2; open uses 4 so mfra moof waves aren't gated.
    match UrlSource::connect_with(
        url.to_string(),
        UrlSourceOptions {
            parallelism: OPEN_URL_PARALLELISM,
        },
    )
    .await
    {
        Ok(src) => {
            let size_kb = src.size() / 1024;
            web_sys::console::log_1(
                &format!(
                    "webmedia: open_url UrlSource size={size_kb} KB parallelism={}",
                    src.parallelism()
                )
                .into(),
            );
            on_status(&format!("Opening (progressive, ~{size_kb} KB)…"));
            Ok(std::sync::Arc::new(src))
        }
        Err(e) if is_no_http_range(&e) => {
            // mediabunny: src/source.ts UrlSource — non-206 → sequential/full path;
            // we fall back to a whole-resource fetch into BlobSource.
            web_sys::console::warn_1(
                &format!("webmedia: no HTTP Range ({e}); full download fallback").into(),
            );
            let size_hint = content_length_from_no_range(&e)
                .map(format_byte_size)
                .map(|s| format!(" (~{s})"))
                .unwrap_or_default();
            on_status(&format!(
                "No HTTP Range — downloading full file{size_hint}…"
            ));
            let blob = fetch_url_as_blob(url).await?;
            let size_kb = (blob.size() as u64) / 1024;
            web_sys::console::log_1(&format!("webmedia: open_url full fetch ~{size_kb} KB").into());
            on_status(&format!(
                "Opening (full download, ~{})…",
                format_byte_size(blob.size() as u64)
            ));
            Ok(std::sync::Arc::new(BlobSource::new(blob)))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Parse `content-length=N` from [`is_no_http_range`] error text when present.
fn content_length_from_no_range(err: &MediaError) -> Option<u64> {
    let msg = err.to_string();
    let rest = msg.split("content-length=").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn format_byte_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

/// Full-resource fetch → [`web_sys::Blob`] (`blob:` / no-Range fallback).
async fn fetch_url_as_blob(url: &str) -> Result<web_sys::Blob, String> {
    let window = web_sys::window().ok_or_else(|| "no window".to_string())?;
    let resp_val = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| format_js_error("fetch", &e))?;
    let resp: Response = resp_val
        .dyn_into()
        .map_err(|_| "fetch did not return a Response".to_string())?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let blob_promise = resp
        .blob()
        .map_err(|e| format_js_error("response.blob", &e))?;
    let blob_val = JsFuture::from(blob_promise)
        .await
        .map_err(|e| format_js_error("blob", &e))?;
    blob_val
        .dyn_into::<web_sys::Blob>()
        .map_err(|_| "response body is not a Blob".to_string())
}

fn format_js_error(what: &str, err: &wasm_bindgen::JsValue) -> String {
    if let Some(s) = err.as_string() {
        return format!("{what}: {s}");
    }
    format!("{what}: {err:?}")
}

pub(super) async fn open_source_staged(
    source: std::sync::Arc<dyn MediaSource>,
    canvas: HtmlCanvasElement,
    on_status: impl Fn(&str),
) -> Result<VideoPlayer, String> {
    on_status("Starting demux…");
    web_sys::console::log_1(&"webmedia: demux start".into());
    let opened = open_mp4_media_source_cached_with_progress(
        source,
        OpenMp4Options {
            // Deferred AVC sync — play on-demand; scrub settle waits for full verify.
            // mediabunny: verifyKeyPackets on retrieve.
            verify_avc_sync: true,
            defer_avc_sync: true,
        },
        |msg| on_status(msg),
    )
    .await
    .map_err(|e| e.to_string())?;
    let video = opened.video.ok_or_else(|| "no video track".to_string())?;
    let audio = opened.audio;
    web_sys::console::log_1(
        &format!(
            "webmedia: demux ok, {} frames, audio={}",
            video.frame_count(),
            audio.is_some()
        )
        .into(),
    );
    on_status(&format!(
        "moov ok — {} frames{}, creating decoder…",
        video.frame_count(),
        if audio.is_some() { " + audio" } else { "" }
    ));
    VideoPlayer::from_tracks_with_fetch(video, audio, canvas, opened.fetch_map)
        .await
        .map_err(|e| e.to_string())
}
