//! Demo player — Trunk CSR host over [`fpmedia_web_components::Video`].

use leptos::prelude::*;
use leptos_meta::{Title, provide_meta_context};
use wasm_bindgen::prelude::wasm_bindgen;

mod icon;
mod player;

#[wasm_bindgen(start)]
pub fn main() {
    console_error_panic_hook::set_once();
    _ = console_log::init_with_level(log::Level::Debug);
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    provide_meta_context();

    view! {
        <Title text="fpmedia-web"/>
        <main class="min-h-screen bg-zinc-950 text-zinc-100">
            <player::DemoPlayer/>
        </main>
    }
}
