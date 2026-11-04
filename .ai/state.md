# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **HEAD:** P1 LoopOwner + SeamPhase; Host LoopZone join EOF + scrub-outside → `loop_in` (no cancel race).
- **Do not land:** `b8206d1` Exact wipe-on-settle; WIP cut/LoopZone hard-reset — do not restore as-is.
- **Parked:** Pause densify pin-holes; idle Fetch gray; seam deeper stash / lead.
- **Unverified:** long scrub-outside stress on multi-clip Host zone.
- **Next chat (primary):** §2 P2 promote policy + LoopSeam port map — before CutWindow.

---

## Next up

### 1. CutWindow

Half-open `[cut_in, cut_out)` source FrameIds; playlist per-clip cut — Later. Prefer after §2 (host seam machine), else seams grow more flag soup. After LoopZone (shipped — `architecture.md` § LoopZone).

### 2. State / densify / playlist-loop refactor

Goal: races fixed by **transitions**, not TEMP APIs / bool latches. P0 enable/arm + P1 LoopOwner/SeamPhase shipped. DoD: no new `enable_*_without_*` helpers; P2–P3 below.

| Priority | Problem (today) | Target |
| -------- | ---------------- | ------ |
| P2 | Cut `@0` and wrap@`loop_in` share `promote_idle_at` but lead/stash depth still forked (`warm_play_origin` vs mid) | One promote policy table (local frame → warm/stash/play); deeper stash = parked residual only |
| P2 | `PlayPhase::LoopSeam` (single Video) vs host promote (playlist) — cousin paths, easy to diverge | Document port map; share stash/warm primitives; host stays slot-swap only |
| P3 | `pause_hard_no_densify` / `pause_warm` / `EnterScrub{densify}` / policy Off clear — densify intent in many doors | Densify arm/clear only via engine_mode cmds; host calls events not side-door aborts |
| P3 | `maybe_kick_idle_seam_warm` branches (`zone_full`, engine owns, mid-in) | Prewarm schedule from LoopOwner + SeamPhase, not ad-hoc ifs |

**Out of this refactor:** CutWindow product; compare dual-visible; densify pin-hole cosmetics; publish.

**Cite:** `no equivalent` for playlist host machine; densify enable/arm local (scrub is local — AGENTS). Engine LoopSeam stays Remotion-shaped (`skills/media.md`).

### Playlist

Shared volume + frame-sum scrub + origin-stash seam shipped (`0.1.0-alpha.4`). Design: `architecture.md` § Playlist.

**Optional:** cut markers / clip name in `[i]` (Later). Idle Fetch gray — parked.

**Out of v1:** compare dual-visible.

### Publish crates (`0.1.0-alpha`)

Layout/packages: `architecture.md` § Layout. Repo: `alexantoshuk/fpmedia-web`. Demo `demo-player` stays `publish = false`.

**DoD:** external Leptos CSR host `cargo add`s `fpmedia-web-components`, opens MP4 via Blob (and CORS-ok URL), play/pause/seek/scrub settle — no our Axum / Trunk.

**Hard gate:** do **not** publish until `re_mp4` is on crates.io with #35+#38 (no fork). Until then: `cargo publish` blocked. Keep git `re_mp4` pin until then.

**Hygiene done (still `publish = false`):** `[workspace.package] version = 0.1.0-alpha.4` (members `version.workspace = true`; path deps omit version). README badges → in-repo guides. Demo header = `fpmedia_web_components::VERSION`. Bumps: `architecture.md` § Versions.

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
- Playlist: clip **name** in `[i]` readout / go-to; cut markers on scrubber.
- Compare dual-visible (reuse 2-slot pool); pure `<Audio>` / `AudioPlayer`; thumb Worker; 4K loop residual; reverse playback (if revisited); production player UI.
