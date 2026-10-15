//! Minimal CSR host: file picker → `VideoRef::open_blob` — no Axum, no `/proxy`.

use fpmedia_web_components::{Video, VideoRef};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::wasm_bindgen;
use web_sys::{Blob, HtmlInputElement};

#[wasm_bindgen(start)]
pub fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let video = VideoRef::new();
    let status = video.status;

    view! {
        <div style="font-family: system-ui, sans-serif; max-width: 48rem; margin: 1rem auto;">
            <h1 style="font-size: 1.25rem; margin: 0 0 0.5rem;">"fpmedia-web — Blob open"</h1>
            <p style="margin: 0 0 1rem; opacity: 0.75; font-size: 0.9rem;">
                "Pick a local MP4. No server proxy — progressive open from the File blob."
            </p>
            <Video
                video=video
                class="blob-stage"
                viewport_class="blob-viewport"
                canvas_class="blob-canvas"
            />
            <div style="display: flex; flex-wrap: wrap; gap: 0.5rem; margin-top: 0.75rem; align-items: center;">
                <label>
                    "Open MP4 "
                    <input
                        type="file"
                        accept="video/mp4,video/*"
                        on:change=move |ev| {
                            let Some(input) = ev
                                .target()
                                .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
                            else {
                                return;
                            };
                            let Some(files) = input.files() else { return };
                            let Some(file) = files.get(0) else { return };
                            let blob: Blob = file.unchecked_into();
                            video.open_blob(blob);
                        }
                    />
                </label>
                <button type="button" on:click=move |_| video.play()>
                    "Play"
                </button>
                <button type="button" on:click=move |_| video.pause()>
                    "Pause"
                </button>
            </div>
            <p style="margin-top: 0.75rem; font-size: 0.85rem; opacity: 0.8;">
                {move || status.get()}
            </p>
        </div>
        <StyleSheet/>
    }
}

/// Inline stage CSS (no Tailwind in this example).
#[component]
fn StyleSheet() -> impl IntoView {
    view! {
        <style>
            ".blob-stage { width: 100%; aspect-ratio: 16 / 9; background: #111; }
             .blob-viewport, .blob-canvas { width: 100%; height: 100%; display: block; }"
        </style>
    }
}
