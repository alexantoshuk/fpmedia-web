# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **HEAD:** `<TimelineMeta>` gate on `timeline.ready` — no probe Fetch fill→wipe. Cite: **no equivalent** (playlist probe / strip host).
- **Prior:** playlist host polish `1185c4a` (visibility probe, mirror while playing, `scrub_resume`); seam `bd2aaf9`.
- **Smoke:** playlist load — strips stay empty until ready, then fill once; prior host polish smokes.
- **Do not land:** `b8206d1` Exact wipe-on-settle (seek Exact-follow ≠ wipe).
- **Parked:** Pause densify pin-holes — accepted strip cosmetics (`product.md`); Later if revisited.

---

## Next up

### Playlist

Element seek/scrub/step + seam + host polish shipped. Playlist UI in product/architecture.

**Optional next:** near-EOF idle Play-warm (loop-origin prewarm shape). Not gapless A/V.

**Out of v1:** clip name in readout/go-to; cut markers on bar; gapless audio; app-wide shared `ThumbSink`; compare dual-visible.

### Publish crates (`0.1.0-alpha`)

Layout/packages: `architecture.md` § Layout. Repo: `alexantoshuk/fpmedia-web`. Demo `demo-player` stays `publish = false`.

**DoD:** external Leptos CSR host `cargo add`s `fpmedia-web-components`, opens MP4 via Blob (and CORS-ok URL), play/pause/seek/scrub settle — no our Axum / Trunk.

**Hard gate:** do **not** publish until `re_mp4` is on crates.io with #35+#38 (no fork). Until then: `cargo publish` blocked. Keep git `re_mp4` pin until then.

**Hygiene done (still `publish = false`):** package versions `0.1.0-alpha.1`; path deps carry matching `version =`; README badges point at in-repo guides (not live crates.io/docs.rs). Demo header shows `fpmedia_web_components::VERSION`. Pre-publish bumps: `architecture.md` § Layout → Versions.

1. **Deps unblock** — switch workspace `re_mp4` from git pin to crates.io when #35+#38 ship; `cargo package` dry-run.
2. **Ship** — flip `publish`; CONTRIBUTING gates + e2e; lockstep publish order timecode → engine → components; GH Release notes; restore crates.io/docs.rs badges; drop this block from state.

### Online demo (free static)

Implementation in tree (Trunk `demo-player`, `.github/workflows/pages.yml`).

- **Do:** enable Settings → Pages → Source: GitHub Actions; push/merge; confirm site loads WASM + Browse open.
- **Out of scope:** public CORS / media proxy; sample buttons; large masters.
- **Note:** private repo — Pages may need Pro; published URL is often world-readable. Local e2e URL probes: `npm run media:fetch` (testvideo v0.3.0 → `demo-player/public/media/`).

---

## Later

- Optional: harden Pause densify pin-holes on slow PreferSoftware (salvage + skip-bridge `put_chunk`; no Exact wipe) — only if strip cosmetics stop being acceptable (`product.md`).
- Revisit seek Exact follow **without** wiping Exact on settle (densify yield / unbanned walk only — prove scrub still paints).
- Optional: unthrottle play `sync_ui` for truer e2e playhead sampling (host only; not an engine judder fix).
- Classify browser codec faults by DOMException `name` (`EncodingError` / `InvalidStateError` / `DataError`) instead of message text in `MediaError::from_codec_message` — behavior change (per-browser wording).
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Playlist: thumbs only on live clip today — progressive/hover across full timeline (or shared `ThumbSink`); clip **name** in `[i]` readout / go-to; cut markers on scrubber; gapless A/V.
- Compare dual-visible (reuse 2-slot pool); pure `<Audio>` / `AudioPlayer`; thumb Worker; 4K loop residual; reverse playback (if revisited); production player UI.
