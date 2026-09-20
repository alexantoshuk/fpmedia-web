//! Demo Axum shell: static site + same-origin media proxy (CORS bypass for Range GETs).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Router,
    body::Body,
    extract::{Query, State},
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
    log!("media proxy: GET/HEAD /proxy?url=<encoded https URL> (forwards Range)");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service())
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

async fn proxy_preflight() -> impl IntoResponse {
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
    res
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

async fn media_proxy(
    State(state): State<AppState>,
    Query(q): Query<ProxyQuery>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response<Body>, (StatusCode, String)> {
    let original = validate_proxy_target(&q.url).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let original_s = original.to_string();
    let target = cached_redirect(&state, &original_s)
        .and_then(|s| reqwest::Url::parse(&s).ok())
        .unwrap_or(original);

    let mut upstream = state.http.request(method.clone(), target);
    if let Some(range) = headers.get(RANGE) {
        upstream = upstream.header(RANGE, range);
    }

    let resp = upstream.send().await.map_err(|e| {
        if let Ok(mut map) = state.redirects.lock() {
            map.remove(&original_s);
        }
        (StatusCode::BAD_GATEWAY, format!("proxy fetch failed: {e}"))
    })?;

    store_redirect(&state, &original_s, resp.url().as_str());

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

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
        CONTENT_LENGTH,
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
        return out
            .body(Body::empty())
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    let stream = resp.bytes_stream().map(|chunk| {
        chunk
            .map(Bytes::from)
            .map_err(|e| std::io::Error::other(e.to_string()))
    });
    out.body(Body::from_stream(stream))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn add_cors_headers(headers: &mut HeaderMap) {
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
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
