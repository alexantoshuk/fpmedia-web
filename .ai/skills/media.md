# Media engine (`fpmedia-web`)

Isolated crate: MP4 demux + frame index + player API; WebCodecs on `wasm32` (`webcodecs`). No Leptos. Layout/layers: `architecture.md`. UX: `product.md`. Mediabunny-first: `AGENTS.md`.

## Triage

| Symptom | Look first | Notes |
| ------- | ---------- | ----- |
| Hang at `creating decoder…` (LAN HTTP) | `ensure_webcodecs_available` | HTTPS/`localhost` only |
| `codec not supported` / HEVC | `classify_codec` / `create_with_policy` | default build: `hevc` off; with feature: HW retry **main only** |
| `codec not supported: avc1.640003` (Telegram) | `canonicalize_avc_webcodecs_config` | SPS-first over lying `avcC` level; patch `description[1..4]`. MB header-only — no equiv |
| Scrub flicker + Play dead (HEVC) | `create_scrub` | densify PreferSoftware; `scrub_densify_blocked` |
| After scrub: blink / hang + `Unsupported configuration` | `HwAccel` / `from_config` | scrub/thumb PreferSoftware; HW recreate → soft fallback |
| New clip: Play disabled / audio dies | clip dispose | `Closed`+drop player; `Shared.disposed`; UrlSource orch abort |
| Open locks until gray full / slow stss | open warm / `defer_avc_sync` | warm lead 96; bg verify + `ensure_key_verified` |
| Local master ~0.75× (no audio clock) | `PlaybackClock` wall time | No AAC/PCM track → silent clock; Chrome `currentTime` lags under decode load — wall time + muted graph. Probe `e2e/player.play.residual.spec.ts` |
| QT PCM master silent / ignored | `build_audio_track` / `audio::pcm` | PCM via moov sound-desc + f32 schedule (no `AudioDecoder`). `mediabunny: media-sink.ts PcmAudioDecoderWrapper` |
| QT PCM = noise / static | `pcm_entry` `enda` in `wave` | Nested `wave/enda` sets LE (`in24`→`pcm-s24`); top-level-only scan → false BE. `mediabunny: isobmff-demuxer wave` |
| PCM rate ≠1× jerky / dropouts | `slice_planar_for_stretch` | Chunk ~1024 frames before WSOLA + time-sliced yield; giant QT packets stall main thread. `mediabunny: no rate API` |
| Rate >1 audio chops on heavy video (long-GOP 720p / 4K), light clip fine; worse after scrub tap / loop seam | `AudioOutput::feed_and_schedule` | park on decoder output/`dequeue` at queue 4 — **no** `setTimeout(0)` per packet/WSOLA chunk (timer cadence ≈ 2× realtime → every chunk late). Yield via `ScheduleSlice` only. `mediabunny: media-sink.ts mediaSamplesInRange` |
| Play residual fps_min≈23 @1 s | e2e residual / host rAF | Demo `sync_ui` ~100 ms — prefer `fps5s` / status fps meter; not engine judder. Parked. |
| `decoder busy` / key chunk / backpressure | DecoderSlot / pump | wall-clock take wait; wake on new output; `q≈41,held=0` → reset |
| Settle Exact poison / dual-HW race | densify yield + soft-cold | `await_scrub_densify_yield`; busy after yield → skip Exact; fault typing: `MediaError::from_codec_message` / `DecodeStall` |
| Seek eng≪aim (fMP4) | `seek_frame` cold | index + GOP warm then Exact — `e2e/player.fetchmap.fmp4-gap.spec.ts` |
| Soft-drag eng lags / bar lags cursor | `note_scrub_frame` / soft pause | drag = aim+blit only; `EnterScrub{soft, densify:false}`; `apply_engine_event` runs cmds |
| Pause strips don't grow / no hole-fill | `pause_warm_idle` | Pause button: `SoftPause{densify:true}` — not `pause_for_scrub` (drag-only) |
| Soft-drag / cold scrub canvas frozen (no frame change) | `scrub_preview_frame` / Exact blit | preview = Exact only — **never** `blit.clear` / full Exact wipe on settle, seek prep, or pointer-up (`b8206d1` `clear_exact_for_aim` broke scrub). Aim/evict OK; full clear not |
| Dense empty / black flash densify | predictive densify / rAF pace | finish in-flight GOP; one rAF between GOPs |
| Densify fills far/edge before near aim | `nearer_gap_with_dir` | distance-first; `dir` tie-break only |
| Scrub UI off but scrub decoder / densify still alive | `set_scrub_cache_policy` / `scrub_cache_off` | gate densify + gray + `ingest_scrub_bitmap_only`; checked-out decoder dropped in `return_scrub_decoder`, never closed mid-`decode()`. Host re-applies in `run_open` before `seek_frame(0)` |
| Hover CPU / freeze / stuck far | ThumbPreview isolation | 3rd decoder; Play: no key_fill; 800 ms aim + 150 ms UI; no microtask spin |
| Play stutter / freeze / underrun | `PlayIter` / `update_next_frame` | mediabunny iterator: `MAX_QUEUE_SIZE` 4 + 2-canvas pool for every resolution; HoldUnderrun only when no `nextFrame` and a frame is due. Demo `wait/s` line (`PlayWaits`): `rd` = byte reads, `out` = consumer wait, `cap` = pump parked full, `raf>50` = main-thread stalls |
| `flush/idle` pts=0 (All-Intra, mobile) | `run_key_cascade` / `force_prefer_software` | key inclusive after wipe; HW silent miss → sticky PreferSoftware; snap fed **before** finish_gop wipe. `mediabunny: media-sink prefer-software` |
| Cold scrub freezes tab / Play deaf | soft settle + transport | `pending_cold_exact`; Settling+Play Preempt — `e2e/player.scrub.cold-open.spec.ts` |
| Gray/green strip holes / blur | FetchMap + float 1 px canvas | gray may soft-merge; greens honest `f/n` — host: `skills/leptos.md` |
| Gray holes never fill (pause/URL) | `schedule_fetchmap_gap_fill` + FetchMap accumulate | own `SingleFlight` (`gray_fill`); never share densify flight; URL accumulate |
| Gray fill floods drag / stays slow after | `scrub_gesture` + gray pace | drag: 1×quantum / 150 ms; idle: 2× / 16 ms; clear gesture on pipeline + `play` |
| URL play ~0.6× / first-play lag | `CachedSource` sample miss | return `want` immediately; quantum fill bg |
| Mobile local Play starves (`wait/s rd` high, `raf>50` ≈0) | `BlobSource` orch worker | one `blob.slice(pos).stream()` reader per worker, `FileSystem` prefetch; `arrayBuffer()` per read only on WebKit. `mediabunny: source.ts BlobSource._runWorker` |
| Play fps decays with position (mobile / weak CPU) | `CacheState::insert` | join on real overlap only — abutting windows stay separate (else one growing buffer re-copied per sample, never evicted); grow/margin use `run_span`. `mediabunny: source.ts ReadOrchestrator.insertIntoCache` |
| Dense wiped / Next-Prev lag / Pause densify dead | Play trim + densify exit | budgets→park→densify; fill order `architecture.md` § Predictive densify; unbanned walk = full indexed span; PTS multimap; drag kills densify |
| Exact 1‑frame holes every GOP (Pause densify) | `fill_one_gap` | PreferSoftware `decode_gop` omit → Exact `decode_frame` salvage before Covered skip-bridge |
| Covered+Exact pin-holes after Pause (slow PC / bad I/O) | densify soft-ban | Accepted strip cosmetics for now (`product.md`); not settle/seek break — do not Exact wipe. Revisit: `state.md` Later |
| Exact 1 px orphan past hole after Pause densify | `evict_exact_orphans_beyond_aim` | farthest-evict every densify tick (not gated on Exact near-gap idle — under-cap soft-drag holes skipped eviction); not on drag `set_playhead` |
| Loop seam wipe / underrun hold | `start_loop_seam` (`PlayPhase::LoopSeam`) | hold clock, `start_play_iter(0)`, resume; keep Covered+Exact |
| Pause→Play / scrub→Play lag or no-op | seed + `ScrubRelease{then_play}` | host: `skills/leptos.md`; probes gray-first / cold-resume / stress |
| Sticky error after Ok settle | `host_ops` clear error on Ok | `e2e/player.scrub.sticky-error.spec.ts` |
| Tab hidden: play dies / densify keeps burning | `yield_scrub_work_for_hidden` | Play+clock continue (MB); scrub densify/gray/thumbs yield; host ~500 ms present. `mediabunny: media-player setInterval(render,500)` |
| Tab hidden: audio choppy / gaps | `play_underrun_hold` / `scrub_bg_suspended` | never `hold_underrun` while hidden (video lag ≠ silence audio); ahead 12 s still helps refill. `mediabunny: no underrun→audio hold` |
| Tab return / scrub→play audio micro-gap | `underrun_catchup_until_ms` / `begin_play_session` | ~1 s grace on visible + every PlayPrime; then_play prewarm + no 2nd `restart_warm` if playing. play→pause→play after scrub = underrun hold, not 2nd warm |
| Rate / shuttle / JKL wrong | `normalize_playback_rate` / eof_wall | ≤4× WSOLA; >4 wall+mute; K pauses before 1× |
| `RefCell already borrowed` | [RefCell](#refcell--wasm-re-entry) | bind then work; no borrow across await |
| fMP4 / URL open hang / Range stampede | mfra warm + orch | moof-only parallel; never bulk 0..mfra; settle ≠ await full sync verify |
| UI stuck (not decode) | `skills/leptos.md` | OpQueue / playing / spawn |

**Probes (paths):** `e2e/player.smoke.spec.ts`, `e2e/player.matrix.spec.ts`, `e2e/player.play.long-gop.spec.ts`, `e2e/player.play.gop12-url-vs-local.spec.ts`, `e2e/player.play.residual.spec.ts`, `e2e/player.play.fmp4-warm-seam.spec.ts`, `e2e/player.scrub.cold-open.spec.ts`, `e2e/player.scrub.cold-resume.spec.ts`, `e2e/player.scrub.sticky-error.spec.ts`, `e2e/player.play.gray-first.spec.ts`, `e2e/player.stress.scrub-av.spec.ts`, `e2e/player.play.eof-loop.spec.ts`, `e2e/player.jkl.spec.ts`, `e2e/player.fetchmap.fmp4-gap.spec.ts`, `e2e/perf.scrub.*.spec.ts`, `e2e/perf.scrub.baseline.spec.ts`.

## Module map (`engine/src`)

```
lib.rs, types.rs, clock.rs, frame_index.rs, error.rs, util.rs, timer.rs (wasm)
model/         bg_task (SingleFlight) · clock_state · decoder_slot · engine_mode · play_phase · session · transport
policy/        clip (crate::policy::*) · playback (EOF/loop) · workload · scrub_plan (native tests)
host/          host_op_coalesce (OpQueue) · open_phase (open status)
residency.rs   AimWindow (aim-distance eviction) · ScrubChunkStore · FrameRunIndex (native tests)
bitstream/     AVC + HEVC packet type / sanitize
source/        MediaSource, CachedSource, FetchMap, ReadOrchestrator (+ wasm Blob/Url)
audio/         demux, pcm (+ pcm_entry moov), stretch (WSOLA), output, PlaybackClock
demux/         boxes, coalesce, mp4, tmcd
video_player/  mod · api · shared · open · control · clock_ctrl · present · decode_ops · play (`begin_play_session` / `play_raf_tick`) · play_pool · scrub · scrub_gap · scrub_cold · seek · thumb (`thumb_off` when feature off)
decode/        webcodecs/{mod,core,pump,play_iter}, frame_cache, scrub_blit, thumb_preview, canvas, owned_*
```

Host-facing `VideoPlayer` = methods `fpmedia-web-components` calls (`pub`); sibling-only helpers = `pub(super)` (e.g. scrub pipeline / clock stop).

Design (caches, Play/Scrub/Settle policy, decode flow diagrams, audio clock): `architecture.md` § Play / scrub residency / Decode flows.

## Gotchas

### Demux / source

- **tmcd start TC** → walk raw moov (`demux/timecode.rs`); `re_mp4` drops body. mediabunny: no equivalent.
- **Gray hole at file start (Blob)** → demux on raw source; `CachedSource::seed_windows` for ftyp/moov.
- **Cold scrub freeze** → densify waits FetchMap+indexed; soft settle commits indexed only (`pending_cold_exact`). `mediabunny: mediaSamplesAtTimestamps`.
- **Clip switch zombies** → `MediaSource::dispose` / Drop abort orch; densify must not adopt epoch after `Shared.disposed`; stop audio + close PlaybackClock.
- **Sample miss blocks Play** → URL/Blob return `want` (URL floors 0.5 MiB); quantum fill bg. Do not await 8 MiB on A/V path.
- **fMP4 warm** → first moofs ready; chunked mfra backfill; adopt audio samples; provisional length from tfra; never bulk-read to mfra into WASM; settle must not `await_sync_verify_complete`.
- **Lying sync / B-frames** → demote non-IDR/non-IRAP; index presentation order; cascade uses `decode_order`. Deferred verify + on-retrieve `ensure_key_verified`.
- **re_mp4 QT mp4a v2 / PCM coalesce** → pin `rerun-io/re_mp4` git (#35/#38); crates.io 0.5.1 lacks both. Play PCM: `audio::pcm` + moov sound-desc (re_mp4 `Unknown` drops body).

### WebCodecs / play

- **Secure context** → gate before `isConfigSupported` or UI freezes on create.
- **First packet sanitize** → strip Chromium-hostile AVC NALs; HEVC NAL-order + drop RASL after CRA (`sanitize_*` / `has_hevc_rasl_picture`).
- **Lifecycle** → feed-ahead over flush; soft sequential next-key on Play; after flush clear warm state; `take_decoder` ~5s wall-clock. All-Intra: never hard-recreate on every key when idle.
- **Held / PTS** → park `PTS >= target`; prune_floor + cascade through aim; PTS assign = FIFO fed stamps (`inputTimestamps.shift`); main HW prefer Hd+Uhd; scrub/thumb `PreferSoftware`; HW configure fail → PreferSoftware sticky; `optimizeForLatency: false`. `mediabunny: media-sink.ts sampleHandler` / alpha prefer-software.
- **Backpressure** → wait on new output/take (not buffered frames as progress); dual-async pump parks at `q==0` for consumer; densify cancel → hard reset, fill task returns.
- **Closures** → never sync-resolve inside output/dequeue; `spawn_local`. Do not pass bare Promise resolve to rAF/`setTimeout` — use `fpmedia_web::timer` (engine + host).
- **Op cancel** → `op_epoch`; Cancelled = soft; invalidate uses reset when checked out.
- **Clock / rate** → `ClockState` + `media_time()`; soft-hold scrub/Pause; resume AudioContext before long prime; silent `PlaybackClock` advances on wall time (Chrome `currentTime` lags without audible output under WebCodecs load — diverge from mediabunny `getPlaybackTime`); AAC still context clock. PCM rate≠1: `slice_planar_for_stretch` (~1024) before WSOLA — do not feed whole QT packets. UX ladder: `product.md`.
- **Underrun** → HoldUnderrun + soft audio hold (keep decoder); not skip spiral. EOF: don’t End while `play_pending()` (iterator still has samples) / while playhead mid at shuttle.
- **Warm pump (seek / step)** → after take, retarget cursor (clear `saw_target` on retarget); park on capacity — don’t exit/respawn thrash. Short B-frame hole: `pts_aim_hole` + prune floor.
- **Play iterator** → Play never reads the presentation queue or `PlayPolicy`; don’t reintroduce Hd/Uhd Play caps / loop prime / EOF lead (mobile stutter came from bytes, not queue depth). Pause / seek / scrub → `clear_play_session` (`PlayIter::finish` returns the decoder). Densify off; behind cap 0; Exact trim farthest. `mediabunny: media-sink.ts mediaSamplesInRange`, `examples/media-player`.
- **Canvas size** → lock buffer + CSS aspect on open/`relayout`; fullscreen wipe → repaint `displayed`/blit; keep scrub markers. Clip switch: `reset_canvas_layout` (shared DOM canvas) or open B inherits A’s intrinsic size.
- **RAII frames** → move into caches; borrow to draw; `try_clone` → `Result`.
- **No Blob/Url per-read `console.log`** → floods main thread on multi‑GB masters.

### Scrub / thumbs

- Model: `architecture.md` § Play / scrub residency.
- Soft-drag: blit + aim only — densify task exits on `scrub_gesture` / drag `cancel_ops_and_scrub_fill`. Pause: warm audio + pipeline (budgets→park→densify); fill order `architecture.md` § Predictive densify; blit cap full → keep marker gaps; `next_gap` none → exit. Settle: second densify yield before soft-cold skip Exact. Strip sticky: accept coverage growth.
- **Do not wipe Exact to “unstick” seek/settle** → `scrub_preview_frame` reads Exact only; `blit.clear` / full Exact trim on pointer-up or seek prep freezes cold scrub (regressed in `b8206d1`). Keep trail; retarget aim + farthest-evict; fix densify yield / gap pick instead. Do not gate all Exact puts behind Covered-only `fill_exact` such that Exact never rebuilds under the finger.
- Do not cancel in-flight GOP densify on aim move; do not blit interim; do not wipe markers on settle/Play; do not drop prune salvage “to clean the strip” (re-decodes GOP — `AGENTS.md` Suboptimal).
- Step: ring → blit → soft goto without abort densify. Pause after Play: refill blit gaps (`next_gap` prefers blit holes). Exact `put` refuses far inserts at cap (no origin/EOF thrash).
- **GopProfile** → Short on **max** GOP only (scene-cut spread must not force Long).
- Thumbs: never densify decoder; Play no sparse key_fill; far refine throttled; `hover_thumb_close_enough` = same sync key.

### RefCell / WASM re-entry

Browser callbacks can re-enter on the same stack. **Forbidden:** `while let` / `if let` scrutinee calling `borrow_mut()` when the body does more than bind. Take/bind first, then reset/await. No `Ref`/`RefMut` across `.await`. Host: `skills/leptos.md`.
