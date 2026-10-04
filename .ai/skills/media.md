# Media engine (`webmedia`)

Isolated crate: MP4 demux + frame index + player API; WebCodecs on `wasm32` (`webcodecs`). No Leptos. Layout/layers: `architecture.md`. UX: `product.md`. Mediabunny-first: `AGENTS.md`.

## Triage

| Symptom | Look first | Notes |
| ------- | ---------- | ----- |
| Hang at `creating decoder…` (LAN HTTP) | `ensure_webcodecs_available` | HTTPS/`localhost` only |
| `codec not supported` / HEVC | `classify_codec` / `create_with_policy` | default build: `hevc` off; with feature: HW retry **main only** |
| Scrub flicker + Play dead (HEVC) | `create_scrub` | densify never HW; `scrub_densify_blocked` |
| New clip: Play disabled / audio dies | clip dispose | `Closed`+drop player; `Shared.disposed`; UrlSource orch abort |
| Open locks until gray full / slow stss | open warm / `defer_avc_sync` | warm lead 96; bg verify + `ensure_key_verified` |
| Local master judder ~25 fps | `frames_after_decode` + silent clock | ChG: skip gate + thin held; not holds |
| `decoder busy` / key chunk / backpressure | DecoderSlot / pump | wall-clock take wait; wake on new output; `q≈41,held=0` → reset |
| Settle Exact poison / dual-HW race | densify yield + soft-cold | `await_scrub_densify_yield`; busy after yield → skip Exact |
| Seek eng≪aim (fMP4) | `seek_frame` cold | index + GOP warm then Exact — `e2e/player.fetchmap.fmp4-gap.spec.ts` |
| Soft-drag eng lags / bar lags cursor | ScrubPolicy + host playhead | soft = blit only; sync playhead on move; no `decode_frame` on drag |
| Dense empty / black flash densify | predictive densify / rAF pace | finish in-flight GOP; one rAF between GOPs |
| Hover CPU / freeze / stuck far | ThumbPreview isolation | 3rd decoder; Play: no key_fill; 800 ms aim + 150 ms UI; no microtask spin |
| Play freeze / flush/idle / underrun | presentation cap / `clock_state` | Hd cap 4; HoldUnderrun; prune floor + cascade — see Gotchas |
| Cold scrub freezes tab / Play deaf | soft settle + transport | `pending_cold_exact`; Settling+Play Preempt — `e2e/player.scrub.cold-open.spec.ts` |
| Gray/green strip holes / blur | FetchMap + 1 px canvas | soft-merge; `paint_strip_canvas_1px` — host: `skills/leptos.md` |
| URL play ~0.6× / first-play lag | `CachedSource` sample miss | return `want` immediately; quantum fill bg |
| Dense wiped / Next-Prev lag / Pause densify dead | Play trim + blit gaps | markers kept; Exact trim on Play; blit gaps refill |
| Loop seam wipe / pause at 1.1–1.75× | loop_prime / SessionModel | keep Covered+Exact; BeginPrepareLoop only |
| Pause→Play / scrub→Play lag or no-op | seed + `ScrubRelease{then_play}` | host: `skills/leptos.md`; probes gray-first / cold-resume / stress |
| Sticky error after Ok settle | `host_ops` clear error on Ok | `e2e/player.scrub.sticky-error.spec.ts` |
| A/V desync / tab-hidden race | `media_time` / HoldHidden | never host wall clock; HoldHidden ∉ underrun_holding |
| Rate / shuttle / JKL wrong | `normalize_playback_rate` / eof_wall | ≤4× WSOLA; >4 wall+mute; K pauses before 1× |
| `RefCell already borrowed` | [RefCell](#refcell--wasm-re-entry) | bind then work; no borrow across await |
| fMP4 / URL open hang / Range stampede | mfra warm + orch | moof-only parallel; never bulk 0..mfra; settle ≠ await full sync verify |
| UI stuck (not decode) | `skills/leptos.md` | OpQueue / playing / spawn |

**Probes (paths):** `e2e/player.smoke.spec.ts`, `e2e/player.matrix.spec.ts`, `e2e/player.play.long-gop.spec.ts`, `e2e/player.play.gop12-url-vs-local.spec.ts`, `e2e/player.play.fmp4-warm-seam.spec.ts`, `e2e/player.scrub.cold-open.spec.ts`, `e2e/player.scrub.cold-resume.spec.ts`, `e2e/player.scrub.sticky-error.spec.ts`, `e2e/player.play.gray-first.spec.ts`, `e2e/player.stress.scrub-av.spec.ts`, `e2e/player.play.eof-loop.spec.ts`, `e2e/player.jkl.spec.ts`, `e2e/player.fetchmap.fmp4-gap.spec.ts`, `e2e/perf.scrub.*.spec.ts`, `e2e/perf.scrub.baseline.spec.ts`.

## Module map (`media/src`)

```
lib.rs, types.rs, clock.rs, clock_state.rs, session.rs, transport.rs,
engine_mode.rs, decoder_slot.rs, workload.rs, host_op_coalesce.rs,
open_phase.rs, playback_policy.rs, frame_index.rs, player.rs, error.rs
bitstream/     AVC + HEVC packet type / sanitize
source/        MediaSource, CachedSource, FetchMap, ReadOrchestrator (+ wasm Blob/Url)
audio/         demux, stretch (WSOLA), output, PlaybackClock
demux/         boxes, coalesce, mp4, tmcd
video_player/  mod · shared · util · clock_ctrl · present · decode_ops · open · play · scrub · scrub_gap · scrub_cold · seek
scrub_plan.rs  pure densify gap / cold-commit (native tests)
decode/        webcodecs/{mod,core,pump}, frame_cache, scrub_blit, thumb_preview, canvas, owned_*
```

Design (caches, Play/Scrub/Settle policy, audio clock diagram): `architecture.md` § Play / scrub residency.

## Gotchas

### Demux / source

- **tmcd start TC** → walk raw moov (`demux/timecode.rs`); `re_mp4` drops body. mediabunny: no equivalent.
- **Gray hole at file start (Blob)** → demux on raw source; `CachedSource::seed_windows` for ftyp/moov.
- **Cold scrub freeze** → densify waits FetchMap+indexed; soft settle commits indexed only (`pending_cold_exact`). `mediabunny: mediaSamplesAtTimestamps`.
- **Clip switch zombies** → `MediaSource::dispose` / Drop abort orch; densify must not adopt epoch after `Shared.disposed`; stop audio + close PlaybackClock.
- **Sample miss blocks Play** → URL/Blob return `want` (URL floors 0.5 MiB); quantum fill bg. Do not await 8 MiB on A/V path.
- **fMP4 warm** → first moofs ready; chunked mfra backfill; adopt audio samples; provisional length from tfra; never bulk-read to mfra into WASM; settle must not `await_sync_verify_complete`.
- **Lying sync / B-frames** → demote non-IDR/non-IRAP; index presentation order; cascade uses `decode_order`. Deferred verify + on-retrieve `ensure_key_verified`.
- **re_mp4 QT mp4a v2** → workspace git pin (#35/#37); drop when crates.io catches up (`state.md`).

### WebCodecs / play

- **Secure context** → gate before `isConfigSupported` or UI freezes on create.
- **First packet sanitize** → strip Chromium-hostile AVC NALs; HEVC NAL-order + drop RASL after CRA (`sanitize_*` / `has_hevc_rasl_picture`).
- **Lifecycle** → feed-ahead over flush; soft sequential next-key on Play; after flush clear warm state; `take_decoder` ~5s wall-clock. All-Intra: never hard-recreate on every key when idle.
- **Held / PTS** → park `PTS >= target`; prune_floor + cascade through aim; main HW prefer Hd+Uhd; scrub/thumb no HW; `optimizeForLatency: false`.
- **Backpressure** → wait on new output/take (not buffered frames as progress); dual-async pump parks at `q==0` for consumer; densify cancel → hard reset, fill task returns.
- **Closures** → never sync-resolve inside output/dequeue; `spawn_local`. Do not pass bare Promise resolve to rAF/`setTimeout`.
- **Op cancel** → `op_epoch`; Cancelled = soft; invalidate uses reset when checked out.
- **Clock / rate** → `ClockState` + `media_time()`; soft-hold scrub/Pause; resume AudioContext before long prime; forward rates only; decrease snaps to displayed. UX ladder: `product.md`.
- **Underrun** → HoldUnderrun + soft audio hold (keep decoder); not skip spiral. EOF: don’t End while presentation holds frames / while playhead mid at shuttle.
- **Warm pump** → after take, retarget cursor (clear `saw_target` on retarget); park on capacity — don’t exit/respawn thrash. Short B-frame hole: `pts_aim_hole` + prune floor.
- **Play VF pool** → densify off; behind cap 0 while playing; Exact trim farthest (not full wipe). Prefetch EOF `false` must await rAF (no spin).
- **Canvas size** → lock buffer + CSS aspect on open/`relayout`; fullscreen wipe → repaint `displayed`/blit; keep scrub markers.
- **RAII frames** → move into caches; borrow to draw; `try_clone` → `Result`.
- **No Blob/Url per-read `console.log`** → floods main thread on multi‑GB masters.

### Scrub / thumbs

- Model: `architecture.md` § Play / scrub residency.
- Soft-drag: blit only — no Exact decode on move. Settle: `prepare_scrub_settle` then Exact when warm; cold → soft path.
- Do not cancel in-flight GOP densify on aim move; do not blit interim; do not wipe markers on settle/Play; do not drop prune salvage “to clean the strip” (re-decodes GOP — `AGENTS.md` Suboptimal).
- Step: ring → blit → soft goto without abort densify. Pause after Play: refill blit gaps (`next_gap` prefers blit holes). Exact `put` refuses far inserts at cap (no origin/EOF thrash).
- **GopProfile** → Short on **max** GOP only (scene-cut spread must not force Long).
- Thumbs: never densify decoder; Play no sparse key_fill; far refine throttled; `hover_thumb_close_enough` = same sync key.

### RefCell / WASM re-entry

Browser callbacks can re-enter on the same stack. **Forbidden:** `while let` / `if let` scrutinee calling `borrow_mut()` when the body does more than bind. Take/bind first, then reset/await. No `Ref`/`RefMut` across `.await`. Host: `skills/leptos.md`.
