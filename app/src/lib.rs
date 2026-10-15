//! Demo host — Leptos CSR shell + controls over [`fpmedia_web_leptos::Video`].

use leptos::prelude::*;

#[cfg(feature = "csr")]
use leptos_meta::{Stylesheet, Title, provide_meta_context};

#[cfg(feature = "csr")]
mod player;

/// URL prefix for static assets (no trailing slash). Set at compile time for
/// project Pages (`FPMEDIA_SITE_BASE=/fpmedia-web`). Empty = site at `/`.
pub fn site_base() -> &'static str {
    option_env!("FPMEDIA_SITE_BASE")
        .unwrap_or("")
        .trim_end_matches('/')
}

/// Absolute site path for `pkg/…` (or other root-relative assets).
pub fn asset_href(path: &str) -> String {
    let path = path.trim_start_matches('/');
    let base = site_base();
    if base.is_empty() {
        format!("/{path}")
    } else {
        format!("{base}/{path}")
    }
}

pub fn shell(options: LeptosOptions) -> impl IntoView {
    let css = asset_href("pkg/fpmedia-web.css");
    let root = site_base().to_string();
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <link rel="stylesheet" id="leptos" href=css/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options root=root/>
            </head>
            <body></body>
        </html>
    }
}

#[cfg(feature = "csr")]
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    let css = asset_href("pkg/fpmedia-web.css");

    view! {
        <Stylesheet id="leptos" href=css/>
        <Title text="fpmedia-web"/>
        <main class="wm-page">
            <header class="wm-header">
                <h1 class="wm-brand">"fpmedia-web"</h1>
                <p class="wm-sub">"Frame-accurate WebCodecs player — alpha"</p>
            </header>
            <player::DemoPlayer/>
        </main>
    }
}
