# Scenarios

> Contract: user scenario → observable → layer owner → e2e spec. Maintenance: `AGENTS.md` § Scenario hygiene.
> UX rules: `product.md`. Shipped layers: `architecture.md`. Traps: skills.
> Seed below = current product invariants. Add a row before a new player spec. `—` = no spec owns that expect.

**Surface** `both` = one law for Single and Playlist. `single` or `playlist` alone is a unify/split candidate.

**Expect** is what Playwright can read on the demo (playhead, engine frame, `playing`, rate, empty error, `data-playhead`). One line; the product paragraph stays in `product.md`.

**Picture** (fixtures only, stage burn-in `f N` + clip label): while a scrub finger is still, and again on pause release. Those two reads are the same frame as the aim. Cold aim with no Exact yet only checks the picture is not stuck on the previous frame. Not during the moving samples, and not frame-by-frame during Play. Paused scrub and scrub-with-resume both exercise a slow drag, a fast drag, and quick timeline clicks (`e2e/player.picture.spec.ts`).

**Review** is empty, or `unify` / `split` / `move→components` / `move→engine` / `demo-only`. Clear it in the same change that updates `architecture.md`.

**E2e** is a spec that asserts this id, `partial: path` when a smoke/probe only touches it, or `—`. New specs take the id. `e2e/player.matrix.spec.ts` and `e2e/perf.scrub.*` stay probes.

---

## Open

| Id | Surface | User action | Expect | Owner now | Review | E2e |
| --- | --- | --- | --- | --- | --- | --- |
| `open-file` | both | Open a local file | Ready, playhead 0, error empty | demo picker → `open_blob` | | partial: `e2e/player.smoke.spec.ts` (single fixture) |
| `open-url` | both | Open a same-origin or CORS-ok URL | Ready, error empty | `open_url` | | partial: `e2e/player.play.gop12-url-vs-local.spec.ts` |
| `open-playlist` | playlist | Several files, or `;`-separated URLs | One timeline; one file stays a single `<Video>` | demo session → `<VideoPlaylist>` | | — |

## Transport

| Id | Surface | User action | Expect | Owner now | Review | E2e |
| --- | --- | --- | --- | --- | --- | --- |
| `play-pause` | both | Play, then Pause | Status `playing`, playhead advances, then not `playing` | `MediaRef` | | partial: `e2e/player.smoke.spec.ts` |
| `play-at-last` | both | Play on the last frame | Restarts at frame 0 and plays, loop on or off (`product.md` § Play at last frame) | `VideoRef::play` / `plan_play_at_clip_last` | unify | `e2e/player.scenarios.spec.ts` |
| `play-chrome-scrub` | both | Scrub while playing, through release | Pause stays shown for the whole gesture; no Play flash (`product.md` § Play chrome) | `TransportState` / playlist `playing` | unify | `e2e/player.scenarios.spec.ts` |
| `space-toggle` | both | Space | Toggles play; rate unchanged (`product.md` § Playback rate) | demo keys → `MediaRef` | | `e2e/player.jkl.spec.ts` |
| `jkl-shuttle` | both | J / K / L | L walks 1→2→4→8, J walks back, K → 1× and toggles play (`product.md` § Playback rate) | demo keys | | `e2e/player.jkl.spec.ts` |
| `rate-8x-silent` | both | Rate at 8× | Picture runs; audio silent. Pitch holds through 4× (`product.md` § Playback rate) | engine audio | | — |
| `shared-rate-gain` | both | Change rate, volume, or mute | One rate, one gain, one play intent on the whole timeline (`product.md` § Playlist) | `MediaRef` | | partial: `e2e/player.smoke.spec.ts` (single mute) |

## Position

| Id | Surface | User action | Expect | Owner now | Review | E2e |
| --- | --- | --- | --- | --- | --- | --- |
| `step-next-prev` | both | Next or Prev (button or ← / →) | Play stops; ±1 frame; LoopZone ignored (`product.md` § Loop zone). **Picture:** ±1 | `MediaRef` → `video::host` / `plan_step` | unify | `e2e/player.scenarios.spec.ts` |
| `goto-ignores-zone` | both | Go to a frame or timecode | Play stops; aim is not clamped to the loop (`product.md` § Loop zone). **Picture:** that frame | `goto_*` | unify | `e2e/player.scenarios.spec.ts` |
| `scrub-paused` | both | Drag and release while paused | Aim stays where released; zone ignored (`product.md` § Loop zone). **Picture:** hold-still and pause release. Gestures: slow drag, fast drag, quick clicks | `<Scrubber>` → transport | unify | `e2e/player.picture.spec.ts` |
| `scrub-up-resume` | both | Release a scrub that started during Play | Play resumes; aim outside `[in, out)` wraps to `loop_in` (`product.md` § Loop zone). **Picture:** hold-still matches aim; release resumes Play. Same three gestures | `ScrubRelease` / `plan_scrub_up` | unify | `e2e/player.picture.spec.ts` |

## Loop and end

| Id | Surface | User action | Expect | Owner now | Review | E2e |
| --- | --- | --- | --- | --- | --- | --- |
| `loop-wrap` | both | Play reaches `loop_out` with loop on | Wraps to `loop_in` (`product.md` § Loop zone) | `virtual_timeline` wall; exec = engine `LoopSeam` or `loop_seam_exec` | | partial: `e2e/player.play.eof-loop.spec.ts` |
| `play-outside-zone` | both | Play while the playhead is outside `[in, out)` | Wraps to `loop_in` (`product.md` § Loop zone) | `plan_host_outside` / engine loop | unify | — |
| `playlist-eof` | playlist | Play hits the end of a clip | Advances to the next clip; last clip ends or wraps with loop (`architecture.md` § Playlist host plans). **Picture:** next plate from its start | `plan_advance_ended` | | `e2e/player.scenarios.spec.ts` |
| `playlist-readout` | playlist | Look at position / go to `[i] tc` | Readout is `[i] local / end`. `[i] <tc\|frame>` jumps that clip; a bare value stays on the current clip (`product.md` § Playlist position UI). **Picture:** that clip's frame | demo chrome | | `e2e/player.scenarios.spec.ts` |

## Hover

| Id | Surface | User action | Expect | Owner now | Review | E2e |
| --- | --- | --- | --- | --- | --- | --- |
| `hover-play-pause` | both | Hover the timeline during Play or Pause | Thumb shows. Hidden while scrubbing (`product.md` § Hover thumbs) | `<HoverThumb>` / `TimelineRef` | | `e2e/player.scenarios.spec.ts` |
| `hover-none` | both | Pointer has no hover (typical phone) | Thumbs off; no progressive fill (`product.md` § Hover thumbs) | `ThumbPolicy::Off` | | — |
| `thumb-resize` | both | Host changes thumb max size | Hover repaints in the new box (`product.md` § Hover thumbs) | `set_thumb_max_size` | unify | `e2e/player.thumbs.resize.spec.ts` |
