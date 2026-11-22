# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **Branch:** `main` @ `abd7787`, clean, matches `origin/main`. Policy: `architecture.md` § Virtual Timeline (Covered + Fetch + thumb markers on VT; Exact bitmaps = slot; playlist thumbs = `VtThumbCache` + 1 `ThumbProducer`; idle = nearest cut; Pause cut-join Exact@join; LoopZone + `SeamPhase`; Host wrap = `loop_seam_exec`; thumb max box = `set_thumb_max_size`, default 160²; ImageBitmap cap = `thumb_cache_frame_budget` @ 128 MiB). Types: `architecture.md` § Ownership & types.
- **Open:** mobile `LoopPlayWarm` unverified. Residual light mobile pause on a Host mid-zone LoopZone seam — Later.
- **Stash:** `stash@{0}` densify-free `clear_pending` after `EndExact` — optional, unrelated.
- **Do not merge:**
  - `archive/playlist-exact-cut-hold` — SoftPark/cut-hold anti-pattern.
  - `backup/residual-densify-doors` (`39d20a4`) — residual densify via `BeginExact{rearm}` + `ScrubCacheOff`.
  - `backup/host-loopzone-18649c8` — earlier seam-mirror skip; the fix on main is `ce17420`.
  - `exp/thumb-grid` — Exact-first 4×4 thumb packs; parked experiment.
  - cut-warm v1 (playhead×ensure thrash).
- **Parked:** Pause densify pin-holes (Later).

---

## Next up

No engine slice in flight. Playlist successor is **CutWindow** — not current (Later).

### E2E

Browser e2e (`skills/media.md` Triage) before engine playback / ownership refactors. `trunk serve` dev build loads (8 MiB wasm stack in `.cargo/config.toml`). No known red spec.

### Publish crates (`0.1.0-alpha`)

Layout/packages: `architecture.md` § Layout. Repo: `alexantoshuk/fpmedia-web`. Demo `demo-player` stays `publish = false`.

**DoD:** external Leptos CSR host `cargo add`s `fpmedia-web-components`, opens MP4 via Blob (and CORS-ok URL), play/pause/seek/scrub settle — no our Axum / Trunk.

**Hard gate:** do **not** publish until `re_mp4` is on crates.io with #35+#38 (no fork). Until then: `cargo publish` blocked. Keep git `re_mp4` pin until then.

**Hygiene done (still `publish = false`):** `[workspace.package] version = 0.1.0-alpha.5` (members `version.workspace = true`; path deps omit version). README badges → in-repo guides. Demo header = `fpmedia_web_components::VERSION`. Bumps: `architecture.md` § Versions.

1. **Deps unblock** — switch workspace `re_mp4` from git pin to crates.io when #35+#38 ship; `cargo package` dry-run.
2. **Ship** — flip `publish`; CONTRIBUTING gates + e2e; lockstep publish order timecode → engine → components; GH Release notes; restore crates.io/docs.rs badges; drop this block from state.

## Later

- **Supplied open meta (unified)** — host/DB timing with media for both `<Video>` and `<VideoPlaylist>` (same shape; playlist = per-clip). Skip probe when meta present; `open_blob`/`open_url` / playlist probe stay fallback. Not current — do not land playlist-only `open_entries` first.
- **CutWindow** — half-open `[cut_in, cut_out)` source FrameIds; playlist per-clip cut. LoopZone and the host seam machine are shipped (`architecture.md` § LoopZone). The Clip↔Source seam is already typed (`ClipFrame::source()` — `architecture.md` § Ownership & types); `cut_in` lands there. Not current priority.
- Optional: harden Pause densify pin-holes on slow PreferSoftware (salvage + skip-bridge `put_chunk`; no Exact wipe) — only if strip cosmetics stop being acceptable (`product.md`).
- Revisit seek Exact follow **without** wiping Exact on settle (densify yield / unbanned walk only — prove scrub still paints).
- Optional: unthrottle play `sync_ui` for truer e2e playhead sampling (host only; not an engine judder fix).
- Classify browser codec faults by DOMException `name` (`EncodingError` / `InvalidStateError` / `DataError`) instead of message text in `MediaError::from_codec_message` — behavior change (per-browser wording).
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Playlist: clip **name** in `[i]` readout / go-to; cut markers on scrubber.
- Compare dual-visible (reuse 2-slot pool); pure `<Audio>` / `AudioPlayer`; thumb Worker; 4K loop residual; reverse playback (if revisited); production player UI.
