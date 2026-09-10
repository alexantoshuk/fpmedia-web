//! Demo host — Leptos CSR shell + controls over [`video::Video`].

use leptos::prelude::*;

#[cfg(feature = "csr")]
use leptos_meta::{Stylesheet, Title, provide_meta_context};

#[cfg(feature = "csr")]
mod player;

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <link rel="stylesheet" id="leptos" href="/pkg/webmedia.css"/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
            </head>
            <body></body>
        </html>
    }
}

#[cfg(feature = "csr")]
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Stylesheet id="leptos" href="/pkg/webmedia.css"/>
        <Title text="webmedia"/>
        <main class="wm-page">
            <header class="wm-header">
                <h1 class="wm-brand">"webmedia"</h1>
                <p class="wm-sub">"Frame-accurate WebCodecs player — alpha"</p>
            </header>
            <player::DemoPlayer/>
        </main>
    }
}
