//! Demo Axum shell: static site + same-origin media proxy (CORS bypass for Range GETs).
//!
//! `/proxy` is **localhost-only** (local tests / e2e). Non-loopback peers get 403.
//! Not a production open proxy — no DNS/resolve hardening beyond existing URL checks.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Query, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri,
        header::{
            ACCEPT_RANGES, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
            ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE,
            CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
        },
    },
    response::IntoResponse,
    routing::get,
};
use bytes::Bytes;
use futures_util::StreamExt;
use leptos::logging::log;
use leptos::prelude::*;
use serde::Deserialize;
use tower::ServiceExt;
use tower_http::services::ServeDir;

/// How long to reuse a resolved redirect target (GitHub signed asset URLs ≈1h).
const REDIRECT_CACHE_TTL: Duration = Duration::from_secs(30 * 60);

/// Buffer upstream body when ≤ this (Range windows / probes). Larger GETs stream
/// **without** forwarding `Content-Length` (chunked) so a truncated CDN body cannot
/// surface as Chrome `ERR_CONTENT_LENGTH_MISMATCH`.
const PROXY_BUFFER_MAX: u64 = 16 * 1024 * 1024;

#[tokio::main]
async fn main() {
    simple_logger::init_with_level(log::Level::Info).expect("failed to init logger");

    let conf = get_configuration(None).unwrap();
    let addr = conf.leptos_options.site_addr;
    let leptos_options = conf.leptos_options;

    let index_path = PathBuf::from(&*leptos_options.site_root).join("index.html");
    tokio::fs::write(index_path, app::shell(leptos_options.clone()).to_html())
        .await
        .expect("could not write index.html");

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("webmedia-demo-proxy/0.1")
        .pool_max_idle_per_host(4)
        .build()
        .expect("reqwest client");

    let app = Router::new()
        .route(
            "/proxy",
            get(media_proxy).head(media_proxy).options(proxy_preflight),
        )
        .route("/", get(file_and_error_handler))
        .fallback(file_and_error_handler)
        .with_state(AppState {
            leptos: leptos_options,
            http,
            redirects: Arc::new(Mutex::new(HashMap::new())),
        });

    log!("listening on http://{}", &addr);
    log!(
        "media proxy: GET/HEAD /proxy?url=<encoded https URL> (forwards Range; loopback peers only)"
    );
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

#[derive(Clone)]
struct AppState {
    leptos: LeptosOptions,
    http: reqwest::Client,
    /// original URL → (final URL after redirects, cached_at)
    redirects: Arc<Mutex<HashMap<String, (String, Instant)>>>,
}

#[derive(Debug, Deserialize)]
struct ProxyQuery {
    url: String,
}

async fn proxy_preflight(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    ensure_loopback_peer(peer)?;
    let mut res = Response::new(Body::empty());
    *res.status_mut() = StatusCode::NO_CONTENT;
    add_cors_headers(res.headers_mut());
    res.headers_mut().insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    res.headers_mut().insert(
        ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("range, content-type"),
    );
    res.headers_mut()
        .insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
    Ok(res)
}

fn cached_redirect(state: &AppState, original: &str) -> Option<String> {
    let mut map = state.redirects.lock().ok()?;
    let now = Instant::now();
    if let Some((final_url, at)) = map.get(original)
        && now.duration_since(*at) < REDIRECT_CACHE_TTL
    {
        return Some(final_url.clone());
    }
    map.retain(|_, (_, at)| now.duration_since(*at) < REDIRECT_CACHE_TTL);
    None
}

fn store_redirect(state: &AppState, original: &str, final_url: &str) {
    if original == final_url {
        return;
    }
    if let Ok(mut map) = state.redirects.lock() {
        map.insert(
            original.to_string(),
            (final_url.to_string(), Instant::now()),
        );
    }
}

fn clear_redirect(state: &AppState, original: &str) {
    if let Ok(mut map) = state.redirects.lock() {
        map.remove(original);
    }
}

async fn media_proxy(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    Query(q): Query<ProxyQuery>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response<Body>, (StatusCode, String)> {
    ensure_loopback_peer(peer)?;
    let original = validate_proxy_target(&q.url).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let original_s = original.to_string();
    let range = headers.get(RANGE).cloned();

    let used_cache = cached_redirect(&state, &original_s).is_some();
    let target = cached_redirect(&state, &original_s)
        .and_then(|s| reqwest::Url::parse(&s).ok())
        .unwrap_or_else(|| original.clone());

    let mut resp = match proxy_fetch(&state, method.clone(), target, range.as_ref()).await {
        Ok(r) => r,
        Err(_e) if used_cache => {
            clear_redirect(&state, &original_s);
            proxy_fetch(&state, method.clone(), original.clone(), range.as_ref()).await?
        }
        Err(e) => return Err(e),
    };

    // Signed CDN URLs expire; cached redirect → 401/403/404. Drop cache + retry once.
    if used_cache && matches!(resp.status().as_u16(), 401 | 403 | 404) {
        clear_redirect(&state, &original_s);
        resp = proxy_fetch(&state, method.clone(), original.clone(), range.as_ref()).await?;
    }

    store_redirect(&state, &original_s, resp.url().as_str());

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if matches!(status.as_u16(), 401 | 403 | 404) {
        clear_redirect(&state, &original_s);
    }

    let upstream_cl = resp
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let range_span = resp
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(content_range_span_len);
    let buffer_hint = upstream_cl.or(range_span).unwrap_or(u64::MAX);

    let mut out = Response::builder().status(status);
    let out_headers = out.headers_mut().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "response builder headers".into(),
    ))?;
    add_cors_headers(out_headers);
    out_headers.insert(
        ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("content-length, content-range, accept-ranges, content-type"),
    );
    out_headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));

    for name in [
        CONTENT_TYPE,
        CONTENT_RANGE,
        ACCEPT_RANGES,
        HeaderName::from_static("etag"),
        HeaderName::from_static("last-modified"),
    ] {
        if let Some(v) = resp.headers().get(&name) {
            out_headers.insert(name, v.clone());
        }
    }
    if !out_headers.contains_key(ACCEPT_RANGES) {
        out_headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }

    if method == Method::HEAD {
        if let Some(v) = resp.headers().get(CONTENT_LENGTH) {
            out_headers.insert(CONTENT_LENGTH, v.clone());
        }
        return out
            .body(Body::empty())
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    // Small / Range bodies: buffer so Content-Length always matches bytes sent.
    if buffer_hint <= PROXY_BUFFER_MAX {
        let bytes = resp.bytes().await.map_err(|e| {
            clear_redirect(&state, &original_s);
            (
                StatusCode::BAD_GATEWAY,
                format!("proxy body read failed: {e}"),
            )
        })?;
        out_headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&bytes.len().to_string()).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("content-length: {e}"),
                )
            })?,
        );
        return out
            .body(Body::from(bytes))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    // Large body: stream chunked — do **not** forward upstream Content-Length
    // (truncated CDN / reset mid-stream → Chrome ERR_CONTENT_LENGTH_MISMATCH).
    let stream = resp.bytes_stream().map(|chunk| {
        chunk
            .map(Bytes::from)
            .map_err(|e| std::io::Error::other(e.to_string()))
    });
    out.body(Body::from_stream(stream))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn proxy_fetch(
    state: &AppState,
    method: Method,
    target: reqwest::Url,
    range: Option<&HeaderValue>,
) -> Result<reqwest::Response, (StatusCode, String)> {
    let mut upstream = state.http.request(method, target);
    if let Some(range) = range {
        upstream = upstream.header(RANGE, range);
    }
    upstream
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("proxy fetch failed: {e}")))
}

/// `Content-Range: bytes start-end/total` → `end - start + 1`.
fn content_range_span_len(cr: &str) -> Option<u64> {
    let bytes = cr.strip_prefix("bytes ")?;
    let (span, _) = bytes.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let start: u64 = start.parse().ok()?;
    let end: u64 = end.parse().ok()?;
    end.checked_sub(start).map(|n| n + 1)
}

fn add_cors_headers(headers: &mut HeaderMap) {
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
}

/// `/proxy` is for local tests / e2e only — refuse non-loopback TCP peers.
fn ensure_loopback_peer(peer: SocketAddr) -> Result<(), (StatusCode, String)> {
    if is_loopback_peer_ip(peer.ip()) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            "media proxy is localhost-only (local tests / e2e)".into(),
        ))
    }
}

fn is_loopback_peer_ip(ip: IpAddr) -> bool {
    // `to_canonical` so IPv4-mapped `::ffff:127.0.0.1` counts as loopback.
    ip.to_canonical().is_loopback()
}

fn validate_proxy_target(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|e| format!("bad url: {e}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported scheme: {other}")),
    }
    let host = url
        .host_str()
        .ok_or_else(|| "url missing host".to_string())?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err("refusing to proxy localhost".into());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err("refusing to proxy private/loopback IP".into());
        }
    }
    Ok(url)
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4 == Ipv4Addr::BROADCAST
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
    }
}

async fn file_and_error_handler(uri: Uri, State(state): State<AppState>) -> impl IntoResponse {
    let root = state.leptos.site_root.clone();
    match get_static_file(uri.clone(), &root).await {
        Ok(res) if res.status().is_success() => res.into_response(),
        _ => get_static_file(Uri::from_static("/index.html"), &root)
            .await
            .expect("could not find index.html")
            .into_response(),
    }
}

async fn get_static_file(uri: Uri, root: &str) -> Result<Response<Body>, (StatusCode, String)> {
    let req = Request::builder()
        .uri(uri.clone())
        .body(Body::empty())
        .unwrap();
    match ServeDir::new(root).oneshot(req).await {
        Ok(res) => Ok(res.map(Body::new)),
        Err(err) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {err}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn loopback_peer_accepts_v4_v6_and_mapped() {
        assert!(is_loopback_peer_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_loopback_peer_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_loopback_peer_ip(IpAddr::V6(Ipv6Addr::new(
            0, 0, 0, 0, 0, 0xffff, 0x7f00, 1
        ))));
        assert!(!is_loopback_peer_ip(IpAddr::V4(Ipv4Addr::new(
            192, 168, 1, 2
        ))));
        assert!(!is_loopback_peer_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
    }

    #[test]
    fn content_range_span_len_parses() {
        assert_eq!(content_range_span_len("bytes 0-99/200"), Some(100));
        assert_eq!(content_range_span_len("bytes 100-199/200"), Some(100));
        assert_eq!(content_range_span_len("bytes 0-0/1"), Some(1));
        assert_eq!(content_range_span_len("bytes */200"), None);
        assert_eq!(content_range_span_len("invalid"), None);
    }
}
