# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **Branch:** `main` — Covered + Fetch + thumb markers sole on VT; Exact bitmaps = slot; **playlist thumb bitmaps = VT `VtThumbCache` + 1 `ThumbProducer`** (not idle ensure); idle = nearest cut; Pause cut-join Exact@join warm; VT LoopZone + SeamPhase FSM; Host wrap exec = `loop_seam_exec`; thumb max box = `set_thumb_max_size` (default 160²); ImageBitmap cap = `thumb_cache_frame_budget` @ 128 MiB.
- **Shipped (do not re-open):** `919322d` idle_neighbor_near_cut + reverse-cut Exact harden; `36d0e09` Covered sole on VT; cut-join PresentHit warm; Fetch strip = playable (buffered-like); Fetch sole on VT; VT LoopZone wall + wrap/EOF/seam-next + SeamPhase/`zone_owner_from_spans`; Host LoopSeam adapter (`loop_seam_exec`); VT thumb wallet + producer (`60d955d`).
- **In this change:** host `set_thumb_max_size` + computed thumb ImageBitmap cap (MiB÷box). **Verify:** change max size mid-session (caches resize); Pause progressive still fills; hover tip follows timeline max signals.
- **OK in browser (prior):** Pause Exact grow; scrub Up densify; cut-join Pause scrub 1↔2 / reverse; Covered + Fetch gray survive clip leave. Pages demo live (`alexantoshuk.github.io/fpmedia-web`).
- **Parked debris (do not use):** VT Exact take/import / densify orch (`vt_densify_authority` stays false). Exact-take backend helpers removed; `VideoHost` take/present methods may still exist unused.
- **Archive (reference only — do not merge):** `archive/playlist-exact-cut-hold` — SoftPark/cut-hold anti-pattern.
- **Open (unchanged):** mobile LoopPlayWarm unverified. Residual light mobile pause on Host mid-zone LoopZone seam — later.
- **Stash:** `stash@{0}` densify-free `clear_pending` after `EndExact` — optional (unrelated).
- **Do not land:** `b8206d1`; `39d20a4`; cut-warm v1 (playhead×ensure thrash).
- **Parked:** Pause densify pin-holes.

---

## Next up

### Playlist — Virtual Timeline

1. **VT Play · LoopZone:** shipped (VT law + SeamPhase; Host exec = `loop_seam_exec` / `start_host_wrap`). **Next:** CutWindow (after seam machine). Thumbs progressive already on VT.

Also later: LoopPlayWarm on mobile / Engine zone; cut markers / clip name in `[i]`.

**Out of v1:** compare dual-visible.

### Ownership / type debt

Rules: `architecture.md` § Ownership & types. Each item leaves this list in the commit that ships it (`closes:` line; agent commits on explicit `commit` — `AGENTS.md` § Commits). **Order:** 1b only (needs a design pass). Each plan needs a `docs:` line (see `AGENTS.md`). Browser e2e (`skills/media.md` Triage) — run before 3 / 4. **Known e2e state:** the `trunk serve` **dev** build of the demo crashes on load (`RuntimeError: memory access out of bounds` in `player::__component_demo_player`; `trunk build --release` is fine — suspect dev-profile stack/frame size, undiagnosed), so run e2e against a release build (`trunk build --release --dist <dir>` + a Range-capable static server, `WEBMEDIA_BASE_URL`). Stale specs vs demo chrome (not engine): `player.jkl` (rate badge now `25fps`), `player.scrub.sticky-error` (`empty URL` text), `player.smoke` audio mute (`is-on` class). Also failing on baseline (undiagnosed): `player.fetchmap.fmp4-gap` (`engine frame did not reach 1102`).

1. **Engine guards.**
   - **1b `PlayIter` guard (last):** ~20 `iter.finish()` sites (`play.rs`, `play_iter.rs`) each hand the main decoder back to `decoder_slot`. Design pass first: table of exit paths (cancel, EOF, error, seek-restart, `LoopPlayWarm` adopt, clip switch, player Drop) → who gets the decoder; `Drop` is sync (`try_borrow`). Cite `mediabunny: src/media-sink.ts` `mediaSamplesInRange` / `iterator.return()`. Then drop the item (`closes: Ownership debt - Engine helpers`).

### Publish crates (`0.1.0-alpha`)

Layout/packages: `architecture.md` § Layout. Repo: `alexantoshuk/fpmedia-web`. Demo `demo-player` stays `publish = false`.

**DoD:** external Leptos CSR host `cargo add`s `fpmedia-web-components`, opens MP4 via Blob (and CORS-ok URL), play/pause/seek/scrub settle — no our Axum / Trunk.

**Hard gate:** do **not** publish until `re_mp4` is on crates.io with #35+#38 (no fork). Until then: `cargo publish` blocked. Keep git `re_mp4` pin until then.

**Hygiene done (still `publish = false`):** `[workspace.package] version = 0.1.0-alpha.5` (members `version.workspace = true`; path deps omit version). README badges → in-repo guides. Demo header = `fpmedia_web_components::VERSION`. Bumps: `architecture.md` § Versions.

1. **Deps unblock** — switch workspace `re_mp4` from git pin to crates.io when #35+#38 ship; `cargo package` dry-run.
2. **Ship** — flip `publish`; CONTRIBUTING gates + e2e; lockstep publish order timecode → engine → components; GH Release notes; restore crates.io/docs.rs badges; drop this block from state.

## Later

- **Supplied open meta (unified)** — host/DB timing with media for both `<Video>` and `<VideoPlaylist>` (same shape; playlist = per-clip). Skip probe when meta present; `open_blob`/`open_url` / playlist probe stay fallback. Not current — do not land playlist-only `open_entries` first.
- **CutWindow** — half-open `[cut_in, cut_out)` source FrameIds; playlist per-clip cut. After LoopZone (shipped — `architecture.md` § LoopZone). Prefer after host seam machine (§2), else seams grow more flag soup. The Clip↔Source seam is already typed (`ClipFrame::source()` — `architecture.md` § Ownership & types); `cut_in` lands there. Not current priority.
- Optional: harden Pause densify pin-holes on slow PreferSoftware (salvage + skip-bridge `put_chunk`; no Exact wipe) — only if strip cosmetics stop being acceptable (`product.md`).
- Revisit seek Exact follow **without** wiping Exact on settle (densify yield / unbanned walk only — prove scrub still paints).
- Optional: unthrottle play `sync_ui` for truer e2e playhead sampling (host only; not an engine judder fix).
- Classify browser codec faults by DOMException `name` (`EncodingError` / `InvalidStateError` / `DataError`) instead of message text in `MediaError::from_codec_message` — behavior change (per-browser wording).
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Playlist: clip **name** in `[i]` readout / go-to; cut markers on scrubber.
- Compare dual-visible (reuse 2-slot pool); pure `<Audio>` / `AudioPlayer`; thumb Worker; 4K loop residual; reverse playback (if revisited); production player UI.
