# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **HEAD:** soft-drag Exact trail intact — `skills/media.md` triage «Soft-drag / cold scrub canvas frozen».
- **Do not land:** `b8206d1` Exact wipe-on-settle (seek Exact-follow ≠ wipe).
- **Parked:** Pause densify pin-holes — accepted strip cosmetics (`product.md`); Later if revisited.

---

## Next up

### Publish crates (`0.1.0-alpha`)

Layout/packages: `architecture.md` § Layout. Repo: `alexantoshuk/fpmedia-web`. Demo `demo-player` stays `publish = false`.

**DoD:** external Leptos CSR host `cargo add`s `fpmedia-web-components`, opens MP4 via Blob (and CORS-ok URL), play/pause/seek/scrub settle — no our Axum / Trunk.

**Hard gate:** do **not** publish until `re_mp4` is on crates.io with #35+#38 (no fork). Until then: docs/hygiene OK; `cargo publish` blocked. Keep git `re_mp4` pin until then.

1. **Deps unblock** — switch workspace `re_mp4` from git pin to crates.io when #35+#38 ship; `cargo package` dry-run.
2. **Ship** — CONTRIBUTING gates + e2e; lockstep `0.1.0-alpha.1` publish order timecode → engine → components; GH Release notes; drop this block from state.

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
- `ThumbCache` on `residency::AimWindow` (aim-distance instead of GOP LRU) — behavior change; probe hover first.
- Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Tab-hidden `AudioContinue`; pure `<Audio>` / `AudioPlayer` (reuses engine `audio`); thumb Worker; 4K loop residual; reverse playback (if revisited); dual-source; production player UI.
