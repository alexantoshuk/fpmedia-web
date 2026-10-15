# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **HEAD:** soft-drag scrub paints; Exact trail intact on settle (`skills/media.md` triage «Soft-drag / cold scrub canvas frozen»).
- **Ignore / do not land on `main`:** `b8206d1` Exact `blit.clear` / wipe-on-settle — freezes cold scrub. Seek Exact-follow goal is real; that approach is wrong.
- **Parked (known):** residual Covered+Exact pin-holes after Pause densify on constrained machines (PreferSoftware omit / densify soft-ban) — accepted strip cosmetics (`product.md` Play vs scrub residency). Do not Exact-wipe to “fix”. Revisit: Later.
- **Do not:** prune Exact on every drag `set_playhead`; soft-drag preview from Covered nearest; wipe Exact on settle.

---

## Next up

### Publish crates (`0.1.0-alpha`)

**Names (agreed):** package rename for crates.io (dirs may stay; `webmedia` / `timecode` names taken upstream):

| Package | Role | Was |
| ------- | ---- | --- |
| `fpmedia-web` | engine | `webmedia` (`media/`) |
| `fpmedia-web-leptos` | `<Video>` + scrub helpers | `media_components` |
| `fpmedia-timecode` | SMPTE frames ↔ HH:MM:SS:FF math | `timecode` |

`app` / `frontend` / `server` stay `publish = false` (demo only).

**DoD:** external Leptos CSR host `cargo add`s leptos crate, opens MP4 via Blob (and CORS-ok URL), play/pause/seek/scrub settle — no our Axum.

**Hard gate:** do **not** publish until `re_mp4` is on crates.io with #35+#38 (no fork). Until then: rename/docs/hygiene OK; `cargo publish` blocked.

1. **Rename + metadata** — package rename; `license` GPL, repo, readme; keep git `re_mp4` pin until upstream release.
2. **Lib hygiene** — remove auto `/proxy` rewrite from leptos open_url (demo owns proxy); `#[doc(hidden)]` internals; host CORS contract in crate README.
3. **Docs + example** — crate READMEs + root README; minimal Blob CSR example (no Axum).
4. **Deps unblock** — switch workspace `re_mp4` from git pin to crates.io when #35+#38 ship; `cargo package` dry-run.
5. **Ship** — CONTRIBUTING gates + e2e; lockstep `0.1.0-alpha.1` publish order timecode → engine → leptos; GH Release notes; drop this block from state.

### Online demo (free static)

Hosted CSR demo without Axum / without public open `/proxy` (localhost proxy stays local/e2e only).

- **Host:** Cloudflare Pages or GitHub Pages — deploy `cargo leptos build` site (`target/site/`).
- **Media:** file picker (Blob) + short same-origin samples from `public/`; no remote-URL open in the public build (or only CORS-ok same-origin assets).
- **Out of scope:** public CORS proxy; shipping large 4K masters on free tier.
- Independent of crates.io hard gate (`re_mp4`); can ship before or after alpha publish.

---

## Later

- Optional: harden Pause densify pin-holes on slow PreferSoftware (salvage + skip-bridge `put_chunk`; no Exact wipe) — only if strip cosmetics stop being acceptable (`product.md`).
- Revisit seek Exact follow **without** wiping Exact on settle (densify yield / unbanned walk only — prove scrub still paints).
- Optional: unthrottle play `sync_ui` for truer e2e playhead sampling (host only; not an engine judder fix).
- Classify browser codec faults by DOMException `name` (`EncodingError` / `InvalidStateError` / `DataError`) instead of message text in `MediaError::from_codec_message` — behavior change (per-browser wording).
- `ThumbCache` on `residency::AimWindow` (aim-distance instead of GOP LRU) — behavior change; probe hover first.
- Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Tab-hidden `AudioContinue`; pure `<Audio>` / `AudioPlayer` (reuses engine `audio`); thumb Worker; 4K loop residual; reverse playback (if revisited); dual-source; production player UI.
