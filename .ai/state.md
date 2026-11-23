# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **Branch:** `main` (Slice 1+4A + 2a + 2b + 3A + 3B + 4B + **5.1+5.2** landed — open matrix + crate-root trim; host seams `MediaError`; `ClockModel`; `LoopOriginWarm`; play intent ≡ `EngineMode::Play`; playlist protocol `pub(crate)` + `SeamLatch`; `play_update: SingleFlight`; demo step → `MediaRef`). Policy: `architecture.md` § Virtual Timeline (Covered + Fetch + thumb markers on VT; Exact bitmaps = slot; playlist thumbs = `VtThumbCache` + 1 `ThumbProducer`; idle = nearest cut; Pause cut-join Exact@join; LoopZone + `SeamPhase`; Host wrap = `loop_seam_exec`; thumb max box = `set_thumb_max_size`, default 160²; ImageBitmap cap = `thumb_cache_frame_budget` @ 128 MiB). Types: `architecture.md` § Ownership & types. Play session: `PlayPhase` + `SeamLatch`.
- **Next priority:** **Refactor — simplify ownership / API** — next **Slice 5.3+5.4**. CutWindow stays Later.
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

### Refactor — simplify ownership / API

Agreed code-review backlog. **Order below is the PR sequence.** Merge only within one axis (same table). Same-diff docs when behaviour / ownership tables change. **Approve-before-edit** still applies per PR.

**Context for a new chat:** product boundary = `product.md` § Element-layer helpers; ownership rules = `architecture.md` § Ownership & types + § State machines over flags. Goal = make illegal states unrepresentable and shrink the public surface — **not** a LOC hunt, **not** removing `Rc`/`dyn MediaSource`.

**PR pass order (do this, not “1+2+3 in one diff”):**

1. **Slice 5** — **5.3+5.4** one PR; **5.5** optional alone.

**OK to combine in one PR:** 5.3 with 5.4.

**Do not combine:** any of this with CutWindow / crates publish.

**Global do-not (all slices):**

- Do **not** replace `Rc` / `Cell` / `RefCell` on `VideoPlayer` / `Shared` with `Arc`/`Mutex` or generics that leak into the public API.
- Do **not** turn `Rc<dyn MediaSource>` into a closed enum (open set: Blob / Url / Memory / Cached / host sources).
- Do **not** merge host `OpQueue.async_id` with engine `op_epoch` / `scrub_epoch` (different crates, different awaits).
- Do **not** merge host `SeamPhase` (`components` VT) with engine `PlayPhase::SeamHold` (different machines: slots vs one decoder).
- Do **not** rewrite `demux/boxes.rs`, `source/orchestrator.rs`, `source/cache.rs`, `audio/output.rs`, or `policy/clip.rs` “for size”.
- Do **not** refactor `decode/webcodecs/mod.rs` or `strip.rs` unless a slice needs a real behaviour fix there.
- Orthogonal env flags stay flags (or a small bag), **not** `EngineMode` variants: `disposed`, `scrub_bg_suspended`, `scrub_densify_blocked`.
- Media edits: mediabunny cite **or** explicit **no equivalent** (scrub / playlist local) per `AGENTS.md`.

**Verify per PR:** `bash scripts/check.sh`; for ownership / play / scrub / seam changes also `npm run e2e` (`skills/media.md` Triage). Drop finished bullets from this block when they ship (no Done list).

---

#### Slice 5 — API hygiene (after 1–4)

Do when publishing / cleaning the crate surface. Lower urgency than 1–4. **Remaining PRs — not one mega-diff.**

**PR 5.3+5.4 (boundary types):**

1. **Boundary types** — `loop_zone() -> LoopZone` (not `(u32, u32)`); prefer `FrameId` / axis newtypes on signals where Leptos allows without breaking hosts; `zone_owner_from_spans` / `HostOp::Seek` take framed ids not raw `u32` where cheap.
2. **`MediaRef` boilerplate** — private trait implemented by `VideoRef` + `VideoPlaylistRef` to replace copy-pasted `match` arms (public API stays the enum facade).

**PR 5.5 (optional):**

3. **Demo session** — replace `use_playlist: bool` + always-mounted dual hosts with `Single | Playlist` session enum if it removes dead work; do not rewrite `DemoPlayer` chrome (~popovers) as part of this.

**DoD:** `cargo doc -p fpmedia-web --no-deps` surface readable for an external host; publish checklist in this file still blocked on `re_mp4` crates.io.

---

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
