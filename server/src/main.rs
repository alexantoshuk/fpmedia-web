use std::path::PathBuf;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, Response, StatusCode, Uri},
    response::IntoResponse,
    routing::get,
};
use leptos::logging::log;
use leptos::prelude::*;
use tower::ServiceExt;
use tower_http::services::ServeDir;

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

    let app = Router::new()
        .route("/", get(file_and_error_handler))
        .fallback(file_and_error_handler)
        .with_state(leptos_options);

    log!("listening on http://{}", &addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

async fn file_and_error_handler(
    uri: Uri,
    State(options): State<LeptosOptions>,
) -> impl IntoResponse {
    let root = options.site_root.clone();
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
