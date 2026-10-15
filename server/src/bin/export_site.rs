//! Write `target/site/index.html` for static hosting (GitHub Pages) and exit.
//!
//! Does **not** use [`leptos::HydrationScripts`] — that path depends on
//! compile-time `LEPTOS_OUTPUT_NAME` inside the `leptos` crate. Names match
//! `[[workspace.metadata.leptos]]` / `cargo leptos build` output in `pkg/`.
//!
//! Optional compile-time `FPMEDIA_SITE_BASE=/fpmedia-web` (via [`app::site_base`]).

use std::path::PathBuf;

fn main() {
    let root = app::site_base(); // "" or "/fpmedia-web"
    let css = format!("{root}/pkg/fpmedia-web.css");
    let js = format!("{root}/pkg/fpmedia-web.js");
    let wasm = format!("{root}/pkg/fpmedia-web.wasm");

    // Same load sequence as leptos `hydration_script.js` (import → init → hydrate).
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<link rel="stylesheet" id="leptos" href="{css}"/>
<link rel="modulepreload" href="{js}" crossorigin/>
<link rel="preload" href="{wasm}" as="fetch" type="application/wasm" crossorigin/>
<script type="module">
import init, {{ hydrate }} from "{js}";
await init({{ module_or_path: "{wasm}" }});
hydrate();
</script>
</head>
<body></body>
</html>
"#
    );

    let site_root = std::env::var("LEPTOS_SITE_ROOT").unwrap_or_else(|_| "target/site".into());
    let index_path = PathBuf::from(&site_root).join("index.html");
    if let Some(parent) = index_path.parent() {
        std::fs::create_dir_all(parent).expect("create site root");
    }
    std::fs::write(&index_path, html).unwrap_or_else(|e| {
        panic!("write {}: {e}", index_path.display());
    });
    eprintln!("wrote {} (site_base={root:?})", index_path.display());
}
