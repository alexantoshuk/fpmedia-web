# Media engine (`webmedia`)

Isolated crate: MP4 demux + presentation-order frame index + player API.
Optional WebCodecs decode/render on `wasm32` (`feature = "webcodecs"`).
No Leptos/`app` dependency. Workspace layout: `.ai/architecture.md`.

## Triage

| Symptom | Look first | Notes |
| ------- | ---------- | ----- |
| Hang at `creating decoder…` (Android / LAN HTTP) | Secure context gate (`ensure_webcodecs_available`) | WebCodecs needs HTTPS/`localhost`; `http://LAN-IP` → no `VideoDecoder` |
| Hang / stuck on open, Play disabled | [WebCodecs decode](#webcodecs-decode), Chromium first AVC packet | `flush` hang, `busy`/`!ready`, lying sync |
| `decoder busy` / `needs a key chunk` | Decoder lifecycle, Op cancel, Yields | `take_decoder` waits ~5s wall-clock (Chrome clamps `setTimeout(0)`); clear warm state after `flush` |
| `backpressure timed out` | `decodeQueueSize` backpressure | wake on **new** output / take; `q≈41,held=0` → reset+retry; wire `skip_behind` on underrun |
| Scrub exact slow / tab freeze on settle | [Scrub cache](#scrub-cache), backpressure | one key→N cascade + salvage; probes below |
| Soft-drag eng lags playhead (Short) | ScrubPolicy soft-drag | expected until settle; blit/preview only — exact on pointer-up |
| Settle eng stuck / `q=41` or `flush/idle` | Scrub cache + `begin_key_cascade` | windowed salvage; recreate on key change; one cascade |
| Dense empty while scrubbing | Predictive densify, Scrub cache | finish in-flight GOP; don’t cancel mid-GOP on aim move |
| Whole Chrome window black flash while densify fills (cold scrub) | Densify blit + `scrub_cache_fill_task` | VF→`transferToImageBitmap` burst; pace **one rAF between GOPs** (keep Exact full viewport; do not shrink window / half-res) |
| Play freezes / low fps / mid-GOP `flush/idle` | presentation cap, warm held drain | Hd: small `PRESENTATION_QUEUE_CAP` (4); Uhd: `PlayPolicy` |
| Periodic ~0.5s Play freeze every N frames (1080p short/medium GOP) | GOP boundary cascade / `finish_gop` | soft sequential next-key (no recreate); short pre-flush drain |
| Chrome `flush/idle` at GOP end (`fed=N,q>0`) | `finish_gop_for_target` | wall-clock drain before flush; long-GOP mid-GOP end (fed=N,q>0) |
| Play underrun / huge `skipped`+`lag` then `q=41` | Underrun clock hold | empty present → freeze media clock; resume when buffered (not skip spiral) |
| Play ~0.6× / long holds on URL while gray bar warm (local OK) | `CachedSource` LRU slots | single 8 MiB window thrashed by A/V Range; keep 4×8 MiB — gray mirrors hot slots |
| Play jank after densify warm (even windowed) | demo strip + `pipeline_stats` | two gradient overlays from coalesced runs; Exact trimmed on `play()` (not full wipe) |
| Play ~0.75× realtime / underrun hold (long GOP ~48) | warm held + pump stop | after take, bg warm-ahead then stop (`media-sink.ts` range); Play behind cap 0 |
| Dense wiped on Play | `present_play_frame` / prime | do **not** trim densify markers on Play — budget/aim eviction only |
| Cached scrub / Next-Prev lag on large viewport | Scrub blit size | densify = VF→ImageBitmap; MiB→frames shrinks cap on large CSS×DPR |
| Fullscreen ↔ windowed black until Play/scrub | `WebPlayer::relayout` | keep markers+blit; redraw `displayed` (or warm blit); do **not** wipe store (old WebP still path) |
| Next/Prev hold slow / key→N every step | Step VF ring + warm blit | ring first; else densify ImageBitmap |
| Next/Prev hold: smooth then ~3s freeze (green strip) | Step soft goto + Exact=blit | miss must not abort densify; green = live blit only |
| Pause densify dead after Play (green empty) | blit gaps in `next_gap_target` | Play trims Exact, keeps markers — refill ImageBitmap holes on pause |
| Blit window slides only at hard edge | `ScrubBlitLru::next_gap_frame` + edge margins | `margin_left`/`right`; idle when pinned to `0`/`last` |
| Exact strip thrash / 100% CPU at origin/EOF | Exact `put` + gap saturate | refuse far inserts at cap (like Covered); `next_gap` None if would_evict |
| Loop seam wipes green strips | `apply_loop_prime` / `loop_to_start_cold` | keep Covered+Exact; only clear presentation/step rings |
| Loop pause before restart at 1.1–1.75× | `prepare_loop_if_needed` / near-EOF cap | rate-scaled drain budget; no `restarting` during prepare |
| 4K slight lag at loop/EOF (not audio cut) | Uhd `PlayPolicy` + residual first-GOP | holds=0 OK; brief fps dip / `skipped` 1–2 on wrap — decode catch-up, not clock hold |
| Settle quality pop after drag | present path | canvas = blit or decode VF only — never a soft still |
| Play jank / fullscreen scrub drag lag | densify path | blit LRU present; densify off on Play |
| Dense stuck empty after Pause | `pause` densify kick + hold latch | clear `hold_densify_for_exact`; deferred kick if `scrub_busy` race |
| Regress play/seek after a “local” decode knob tweak | `AGENTS.md` Mediabunny-first pre-edit gate | cite mediabunny path first — no latency/prune/queue invent |
| Pause→Seek same / Prev→Next ≠ pause frame | Scrub cache `put_chunk` / present_and_cache | canvas must replace scrub resident |
| Play lag right after scrub (long GOP) | Scrub cache + `skills/leptos.md` Play after scrub | seek restart / densify before PlayPrime |
| Play after scrub no-op (esp. after EOF) | `skills/leptos.md` Play after scrub | stale clock / UI `playing` early; **or** scrub throttle trailing drag after settle kills PlayPrime |
| A/V desync / silent play with audio track | [Audio + media clock](#audio--media-clock) | video must use `WebPlayer::media_time()` (AudioContext), not host wall-clock |
| Rate wrong / pitch stretch only / no slow-mo | Playback rate gotcha | `normalize_playback_rate` forward-only; clock `* rate`; ≤4× = WSOLA fixed-pitch |
| Shuttle / non-1× choppy / audio ticks | Playback rate gotcha | `>4` silent (`is_wall_clock_rate`); shuttle present still `≥2` |
| J rate-down still plays fast for ~1–2s | Playback rate gotcha | shuttle skips underrun hold → clock races; `set_playback_rate` snap to displayed on decrease |
| K at non-1×: video pauses, audio starts | demo JKL + `set_playback_rate(1)` | pause before 1× snap; spawn aborts if `!playing` |
| `RefCell already borrowed` (decode / scrub / open / Drop) | [RefCell / WASM re-entry](#refcell--wasm-re-entry) | `if let`/`while let` + `borrow_mut`; JS re-entry |
| QT / large `.mov` open freeze | Demux `re_mp4` sample ranges | size-0 `skip_box`; no `CachedSource` during verify |
| `mfhd not found` / fMP4 open fail | Demux `load_ftyp_moov` | load full `moof` bodies; probe `progressive_fmp4_from_url --ignored` |
| Remote URL open very slow (esp. fMP4 / GitHub Releases) | Demux scan + coalesce; demo `/proxy` redirect cache | sliding 64 KiB scan window; pack ≤1 MiB Range GETs for moof load; proxy caches signed redirect ~30 min |
| UI stuck / controls dead (not decode) | `skills/leptos.md` triage | OpQueue, `playing`, `spawn_local` |

**Probes:** `e2e/player.smoke.spec.ts`; **testvideo matrix** `e2e/player.matrix.spec.ts` / `npm run e2e:matrix` (GOP12, bf2, GOP48, fMP4, 60 fps, scenecut; HEVC via `WEBMEDIA_MATRIX_HEVC=1`); **Chrome long-GOP play** `e2e/player.play.long-gop.spec.ts`; **URL vs local play** `e2e/player.play.gop12-url-vs-local.spec.ts` (cache thrash); **EOF/loop** `e2e/player.play.eof-loop.spec.ts` (testvideo AVC 1080p GOP12); **JKL** `e2e/player.jkl.spec.ts`; scrub settle `e2e/perf.scrub.settle.spec.ts`; drag→settle `e2e/perf.scrub.drag-settle.spec.ts`; general scrub `e2e/perf.scrub.probe.spec.ts`. **Decode-sink gate:** `e2e/perf.scrub.baseline.spec.ts` / `npm run e2e:baseline:save|compare` (testvideo GOP12+GOP48; `e2e/baselines/`).

## Module map (`media/src`)

```
media/src/                 ← package name: webmedia
├── lib.rs
├── types.rs               ← FrameId, Timestamp, EncodedSample, Video/AudioMetadata
├── clock.rs               ← MediaClock (wall until audio; then AudioContext driver)
├── bitstream/             ← AVC packet type (IDR + Recovery Point SEI)
├── source/                ← MediaSource + MemorySource + CachedSource (+ wasm BlobSource / UrlSource)
├── frame_index.rs         ← presentation order; sync_by_decode O(log k) + by_decode GOP map
├── demux/
│   ├── boxes.rs           ← top-level ISOBMFF scan + ftyp/moov/moof stub
│   └── mp4.rs             ← re_mp4 → Mp4VideoTrack + Mp4AudioTrack; OpenedMp4
├── player.rs              ← play/pause/seek_frame/step → DecodeWindow
├── time_stretch.rs        ← WSOLA fixed-pitch (Vanilagy gist; ≤4× audio)
└── decode/                ← cfg(wasm32 + webcodecs)
    ├── webcodecs.rs       ← DecoderCore + dual-async decode_frame; decode_gop
    ├── audio_output.rs    ← AudioDecoder → (+WSOLA) → AudioBuffer → AudioContext
    ├── owned_audio_data.rs← OwnedAudioData (close on drop)
    ├── frame_cache.rs     ← ScrubChunkStore + FrameRunIndex + ThumbCache shell
    ├── scrub_encode.rs    ← VideoFrame → ImageBitmap (densify blit)
    ├── owned_video_frame.rs ← OwnedVideoFrame (close on drop)
    ├── canvas.rs          ← draw VideoFrame
    └── web_player.rs      ← WebPlayer façade (open_blob; optional audio)
```

### Audio + media clock

Layering (do **not** put decode/`AudioContext` in `VideoRef`):

```text
  Mp4AudioTrack (demux) ──► AudioOutput (decode + schedule)
                                    │
  MediaClock ◄── AudioContext.currentTime (when track exists)
       │         else wall Performance clock
       ▼
  WebPlayer::media_time() ──► video present_due / host rAF
```

Future pure-audio: same demux + `AudioOutput` + `MediaClock` behind `AudioPlayer` / `AudioRef` (no canvas).

**mediabunny:** `examples/media-player/media-player.ts` (`getPlaybackTime` / `runAudioIterator`); `AudioBufferSink` (`src/media-sink.ts`). Rate/pitch: **no equivalent** — review rates use `time_stretch::TimeStretcher` (Vanilagy WSOLA gist ≈ Chromium `preservesPitch`). Do not open `AudioContext` only as a disposable clock (`resume` + early `close` → `Closed before resume completed`).

## Frame vs time

- **Primary position** = presentation `FrameId` (0-based), sorted by composition timestamp.
- Timestamps come from the sample table — do **not** use `frame = time * fps` as truth (VFR).

## Seek cost

Exact frame N does **not** require all-intra video. Cascade:

```text
nearest sync (by decode timestamp) → decode … → target
```

`Player::seek_frame` returns a `DecodeWindow`; `WebPlayer` feeds it to WebCodecs, keeps the target `VideoFrame`, draws to canvas.

## Mediabunny reference

Required workflow: `AGENTS.md` Mediabunny-first. Cite `src/…` symbols in gotchas. Do **not** depend on the TS package.

Useful areas: packet/sample/canvas sinks, decode-order vs presentation-order, `decodeQueueSize` backpressure (`media-sink.ts` `queueDequeue` / `onQueueDequeue` on **output**), keyframe cascade / flush rules, playback clock pacing, `BlobSource` reads.

Rust/WASM friction (Closures, `JsFuture`, `RefCell`) is adaptation of the same control flow — not a license to redesign waits.

### Ported patterns (engine)

- Decode-order pump + held buffer + PTS remap + output-callback wait (`decode_frame`); prefer feed-ahead over flush.
- Bounded presentation queue + `decodeQueueSize` backpressure + skip-when-behind (`WebPlayer`).
- Bitstream keyframe verification (`verifyKeyPackets`) — AVC: demote lying `stss` unless IDR **or** exact Recovery Point SEI; Chromium &lt;145 demotes SEI keys; ≥145 / FF / Safari keep them (`mediabunny: src/codec-data.ts determineVideoPacketType`; w3c/webcodecs#650). **Async verify** for Blob/progressive (`verify_avc_sync: true`); sync `block_on` cannot drive `BlobSource`. HEVC IRAP still open → `.ai/state.md`.
- Seek/pause cancel — `WebPlayer` `op_epoch` (mediabunny dispose / asyncId). Host UI queue: `skills/leptos.md`.
- Blob reads: `blob.slice(start,end).arrayBuffer()` (avoid `blob.stream()` on Safari/iOS).
- Do not pass Promise `resolve` directly to `requestAnimationFrame` / `setTimeout`; use a `Closure`.
- **Chromium first AVC packet** — strip NALs before AUD and reserved types 20–31 (`VideoDecoderWrapper` / Chromium issues 470109459, #396). Race `flush()` with a timeout as a safety net.

UI host patterns (spawn_local, OpQueue, `playing` ownership, wall-clock play loop): `skills/leptos.md`. Backlog (audio clock, sinks, filmstrip): `.ai/state.md`.

## Frame and decode caches

Decoded Exact residency is **`ImageBitmap` blit + resident markers**. Dual `VideoDecoder` so scrub fill does not steal play.

```text
  Play path                          Scrub / pause path
 ─────────────                       ──────────────────
  main VideoDecoder                  scrub VideoDecoder
       │                                    │
       ▼                                    ▼
  presentation queue                 densify → ImageBitmap (ScrubBlitLru)
  (ahead ≤4 + behind ≤8 live VF)     + ScrubChunkStore (resident markers)
       │                                    │
       └──────────► canvas ◄────────────────┘
```

| Store | Role | Ownership |
| ----- | ---- | --------- |
| **Presentation queue** | Play-ahead + Next hits | `WebPlayer` deque; `PRESENTATION_QUEUE_CAP` (4) |
| **Step behind ring** | Prev hits (live VF) | `displayed` + `step_behind`; `STEP_BEHIND_CAP` (8); **Play uses cap 0** |
| **`ScrubChunkStore`** | Gap geometry | **markers only**; densify inserts resident |
| **`ScrubBlitLru`** | Scrub/step canvas present + Exact strip | densify `ImageBitmap`; cap from [`scrub_blit_frame_budget`] (`ScrubPolicy::blit_mib` × canvas buffer) |
| **`ThumbCache`** | Filmstrip bitmaps (shell; not wired) | Separate from Exact markers |

Densify `on_frame`: VF → `transferToImageBitmap` + marker. `mediabunny:` CanvasSink canvas pool — same idea.

| Constant | Default | Meaning |
| -------- | ------- | ------- |
| `DEFAULT_SCRUB_CACHE_MIB` | 8192 | Covered marker budget (YUV-sized **estimate**; store is markers only) |
| `MIN_SCRUB_CACHE_FRAMES` / `MAX_SCRUB_CACHE_FRAMES` | 8 / 4096 | Floor / ceiling after MiB→frames — Exact blit unchanged |
| `PRESENTATION_QUEUE_CAP` | 4 | Play/Next ahead depth (with `STEP_BEHIND_CAP` ≈ 1+8+4 live VF) |
| `STEP_BEHIND_CAP` | 8 | Live VF behind playhead for Prev without key→N |
| `SCRUB_BLIT_LRU` | 96 | Reference blit count at 1280×720 for Short; real cap = [`scrub_blit_frame_budget`] from `blit_mib` × CSS×DPR buffer — tune MiB per profile, not this alone |
| `DEFAULT_SCRUB_BLIT_MIB_SHORT` / `_LONG` | 338 / 512 | GPU MiB wallets on [`ScrubPolicy::blit_mib`] |
| `MIN_SCRUB_BLIT_FRAMES` / `MAX_SCRUB_BLIT_FRAMES` | 24 / 192 | Floor / ceiling after MiB→frames |
| `EDGE_MARGIN_LEFT_MIN` / `_MAX` / `RIGHT_MIN` | 8 / 24 / 4 | Clamp for [`scrub_edge_margins`] → `ScrubPolicy::margin_left`/`right` |
| `ScrubPolicy::margin_left` / `margin_right` | from GOP hint | After `no_gaps` at capacity: re-arm near expandable extent start/end (not when pinned to `0`/`last`); left > right |

**Byte → frames:** `scrub_cache_frame_budget(count, w, h, mib)` ≈ `mib×1024² / (w×h×1.5)` (YUV 4:2:0), then clamp to `[MIN, MAX]` and `frame_count`. MiB from `ScrubPolicy::scrub_cache_mib` (via `VideoMetadata::scrub_cache_frame_budget`). Short clips that fit stay fully resident. Live blit: `scrub_blit_frame_budget(buf_w, buf_h, blit_mib)` ≈ `mib×1024² / (w×h×4)` (RGBA), clamp `[MIN_SCRUB_BLIT, MAX_SCRUB_BLIT]`; recompute on open / fill / `relayout`.

Eviction (scrub store + blit): drop farthest from **aim**; protect the aim frame. Gap geometry in **`FrameRunIndex`**. Densify `on_frame`: VF → sync `ImageBitmap` + resident marker. `relayout` keeps store + blit (ImageBitmap scales on present) and refreshes blit frame cap from the new buffer; only repaints after buffer wipe. `mediabunny:` CanvasSink canvas pool — no still-encode store.

### Predictive densify (scrub direction)

Local invention (mediabunny has no equivalent). Unit of work stays **one GOP**; only **which gap** changes.

| | Neutral (`dir == 0`) | Predictive (`dir ±1`) |
| - | --- | --- |
| When | settle / pause / seek / no motion yet | scrub drag (`scrub_to(settle=false)` / `note_scrub_frame`) |
| Pick | `nearer_gap_prefer_behind` (min `\|gap−aim\|`, tie → behind) | hard prefer gap **along** `dir` if any; else the other side |
| State | `Shared.scrub_dir = 0`, `prev_scrub_aim = None` | `sign(aim − prev)`; equal aim keeps last `dir` |

```text
drag sample → note_scrub_direction(aim)
           → next_gap_frame(..., dir) → fill_one_gap(GOP)
settle/play → clear_scrub_direction (neutral fill after exact)
```

Finish in-flight GOP on aim move, settle hold, salvage, eviction — unchanged. Zigzag drag may spend one GOP against the new course; that is accepted (same as finish-GOP policy).

**GOP profile** (`GopProfile` on `VideoMetadata`, from `FrameIndex::gop_profile` after open / AVC sync verify) — drives **scrub** only:

| Profile | When | Densify | Drag |
| ------- | ---- | ------- | ---- |
| `AllIntra` | every sample sync | **off** | soft |
| `ShortFixed { len }` | max GOP ≤ 30 (scene-cut spread OK) | on | **soft** |
| `Long { median, max }` | max GOP > 30 | on | exact |

**`PlayPolicy`** (primary — `VideoMetadata::play_policy` from coded height):

| Class | When | presentation_cap | warm_ahead | prefer-hardware | eof_lead | loop_prime |
| ----- | ---- | ---------------- | ---------- | --------------- | -------- | ---------- |
| `Hd` | height &lt; 2160 | **4** (historical FHD) | **4** | no | **0** (no special non-loop EOF) | 8 / 0.4 s |
| `Uhd` | height ≥ 2160 | 10 | 8 | yes if HW ok | **1 s** (pull whole remaining tail) | 12 / 1.0 s |

Do **not** retune Hd when changing Uhd. Support-check never requires HW (open stays FHD-safe). Scrub decoder always uses `PlayPolicy::hd()` knobs so Uhd play keeps the sole HW session (mediabunny: `decoderOptions.hardwareAcceleration`, PR #406 — dual HW can stall silently). `optimizeForLatency` stays `false` for VOD. Uhd near-EOF lead covers **both** loop and non-loop underrun before stop (`mediabunny: no equivalent`).

**`ScrubPolicy`** (secondary — `VideoMetadata::scrub_policy` ← `GopProfile`): `drag` / `densify` / `blit_mib` / `scrub_cache_mib` / `margin_left` / `margin_right`. Soft-drag = blit/preview only during drag; exact VF on settle. Exact-drag (Long) may `goto_frame` while moving. Live blit frame cap = `scrub_blit_frame_budget(canvas_w, canvas_h, blit_mib)` (RGBA) — smaller viewport → more frames, fullscreen → fewer; Long MiB > Short. Edge margins from `scrub_edge_margins(gop_hint)`: left = clamp(hint, 8, 24), right = max(4, left/2) — densify fills key→forward so expanding **left** needs more lead time. Same pair for Covered + Exact. Local invention (mediabunny: CanvasSink sized canvases + `poolSize`; no GOP-profile knobs).

**All-Intra gotcha:** every frame is a sync, so a naïve `key_order` change must **not** hard-`recreate` the `VideoDecoder` (was: scrub felt uncached + inexact). Soft-clear when idle via `begin_key_cascade`. Do **not** run gap-fill densify — single-frame decode is enough; densify only wastes CPU. Filename may say “H265/4K” while demux picks an AVC 1080p track — trust `metadata`, not the name.

| Mode | Dense fill | Main decoder |
| ---- | ---------- | ------------ |
| **Play** | Off (scrub epoch cancelled) | Present + prefetch into presentation queue |
| **Scrub / paused** | On via scrub decoder when `ScrubPolicy::densify` | Seeks / steps / PREV GOP fill as needed |

**One entry:** `WebPlayer::enter_scrub_cache_pipeline(focus)` for pause, scrub drag/hold/release, and post-seek/step: scrub mode → park presentation into densify markers + blit → aim at `focus` → `scrub_cache_fill_task` densifies the **GOP** of the next gap until budget-saturated. `play()` switches to Play mode and cancels scrub fill. **In-flight GOP densify is not cancelled when aim moves**. Abort densify only on settle hold / play / scrub-epoch. **Scrub hybrid:** drag keeps densify on; pointer-up settle calls `prepare_scrub_settle` **before** the async op. Exact: **one** key→N cascade (`goto_frame_with_interim`). Salvage pruned PTS `< target` into scrub store **without blit**. Never blit interim. Fill task **exits** on hold. PREV uses mid-GOP `fill_gop_into_cache` present.

**Contiguous arming:** `ScrubChunkStore::no_gaps` when the extent is a single run. While `no_gaps` **and under frame budget**, keep expanding. Edge-idle (`ScrubPolicy::margin_left` / `margin_right`) applies only when **at capacity**, and only for edges that can still expand (`start > 0` / `end < last`). Visited scrub frames stay until budget eviction (farthest from aim).

Demo strip: dim Covered = store markers (coverage); bright Exact = live blit (step-ready). Two CSS `linear-gradient` overlays (O(1) DOM) from coalesced runs — no per-frame ticks; ahead/behind VF rings are buffer meters only. Marks stay visible during Play. `play()` **trims** Exact (free Play VF VRAM via farthest-from-playhead eviction; not a full wipe) and keeps markers. While paused, `video` keeps rAF ticking so marks update during fill.

## Gotchas

### Demux and bitstream

- **URL play underrun while gray bar looks warm** — mediabunny `BlobSource` keeps **one** ~8 MiB window; progressive A/V Range on high-RTT `/proxy` thrashed it (~0.6× realtime). Local invention: `CachedSource` LRU of **4×8 MiB**; `FetchMap` mirrors resident slots (not historical union). Header reads (≤16 B) neither prefetch nor occupy a slot. Probe: `e2e/player.play.gop12-url-vs-local.spec.ts`.
- **`re_mp4` sample ranges** are **file-absolute**. Progressive open loads full `ftyp`+`moov` (+ every `moof` for single-file fMP4) plus **headers** of all other top-level boxes (`mdat`/`free`/`wide`/`mfra`/…): `re_mp4` reads those headers before `skip_box` seeks, and **parses** full `moof` bodies (`mfhd`/`traf`/`trun`). Zero-filled gaps look like `size=0` and abort **or** — with QT padding — `skip_box(0)` **infinite-loops** (seek back to the size-0 header) and freezes the tab on large QT / padded files. Header-only `moof` → `mfhd not found`. mediabunny: `isobmff-demuxer.ts` `readFragment` `requestSlice(…, moof.totalSize)`. Mitigations: scan headers past `moov` with a **sliding 64 KiB window** (not one 8-byte Range per box); **coalesce** load Ranges up to **1 MiB** so fMP4 moofs share RTTs on high-latency hosts (GitHub Releases via `/proxy`); trim size-0 padding at the end of `moov` children; `SparseBoxReader` refuses size-0 `skip_box` rewinds. **Do not** wrap the source in `CachedSource` during demux or during full-file AVC sync verify — an 8 MiB prefetch per scattered keyframe pulls multi‑GB into WASM (~600 syncs on long 1080p); attach cache only after parse+verify for sample I/O. Header-sized reads (≤16 B) never prefetch. Demo `/proxy` caches the post-redirect asset URL (~30 min) so each Range skips the GitHub 302. Demo uses `open_blob` with **async** AVC sync verify after moov. Live/growing fMP4 and DASH playlists remain out of scope.
- **QT / `.mov` AAC (`mp4a` version 2)** — crates.io `0.5.1` fails open (`mp4a box contains a box with a larger size than it`). Workspace pins [rerun-io/re_mp4@aa5be9b](https://github.com/rerun-io/re_mp4/commit/aa5be9b10686f9cd1834cc8c8c75f6adbb7cf468) ([#35](https://github.com/rerun-io/re_mp4/pull/35) merged; drop git pin on next crates.io release).
- **AVCC vs Annex-B** — MP4 `avc1` samples are length-prefixed; WebCodecs wants that plus `avcC` in `VideoDecoderConfig.description`. Length size = `(avcC[4] & 0b11) + 1`.
- **Lying sync samples** — on AVC open, demote `is_sync` when the AU is neither IDR (NAL type 5) nor exact Recovery Point SEI (`sei_nal_is_exact_recovery_point`; mediabunny `determineVideoPacketType` / w3c/webcodecs#650). Chromium &lt;145: SEI keys demoted (`accept_avc_recovery_point_sei`); ≥145 / non-Chromium: kept. Does not yet handle HEVC IRAP.
- **B-frames** — index is presentation order; `nearest_keyframe` / `decode_window` use **`decode_order`** (not DTS range). Fixture `with_b_frames.mp4` covers this.
- **Sync index** — `FrameIndex` keeps `sync_by_decode` (sorted) + dense `by_decode[decode_order]→FrameId`. `nearest_keyframe` / `next_keyframe_after` are O(log k); `gop_frame_ids` / `decode_window` slice the decode map (O(gop)), not a full-frame scan. `set_sync` updates the sync table in place. Local invention (mediabunny has packet cursors, not this index).

### WebCodecs decode

- **Secure context** — `VideoDecoder` is HTTPS/`localhost` only. Opening the demo as `http://192.168.x.x:3000` from Android (or any non-localhost HTTP) → API missing; unguarded `isConfigSupported` panics the open task and freezes UI on `creating decoder…`. Gate in `WebCodecsDecoder::create` (`ensure_webcodecs_available`). Workaround: `adb reverse tcp:3000 tcp:3000` → `http://127.0.0.1:3000`, or `chrome://flags/#unsafely-treat-insecure-origin-as-secure`, or HTTPS. Platform constraint (no mediabunny equivalent).
- **Target match** — pick the sample with `frame == target_frame`. Never use `samples.last()`.
- **Decoder lifecycle** — warm decode-order pump (`decode_frame`): feed next packets in the GOP until the target PTS is in `held`. Prefer feeding ahead over `flush()`. Do **not** `flush()` between playback frames in the same GOP. **Play across GOP:** soft `begin_key_cascade(..., sequential_next_key=true)` when idle — feed the next IDR without `VideoDecoder` recreate (mediabunny range pump). Non-adjacent scrub jumps still hard-recreate. **After `flush()`, clear `key_order`/`fed_through`** — Chromium then requires a key chunk (`VideoDecoder needs a key chunk`); Play prime after open seek-0 failed when we flushed frame 0 but left warm state. Prefer one coax delta feed to emit the keyframe instead of flush. **`take_decoder` waits up to ~5s wall-clock** with macrotask yields — a fixed ~120×`setTimeout(0)` was only ~0.5s on Chromium (4ms clamp) and false-fired `decoder busy` during play while Firefox was fine. Cancel only on `op_epoch` change. **All-Intra:** never early-exit the key poll while `input_timestamps` remain; avoid multi-second flush (pins the slot) — hard re-feed once, short flush only as last resort. Soft `begin_key_cascade` only when queue empty and no pending remap.
- **B-frame held buffer + PTS remap** — park not-yet-presented frames in `held` (`PTS >= target` during seek prune — never drop post-target or play/prefetch loses warm ahead). Prefer matching `VideoFrame.timestamp()` when present in `input_timestamps`, else FIFO. After taking the target, briefly drain already-decoded outputs into `held` before stopping the pump. **`optimizeForLatency: false`** for VOD (true → `backpressure q≈41` / mid-GOP wedge on long-GOP play). Do **not** require `prefer-hardware` in `isConfigSupported`.
- **`decodeQueueSize` backpressure** — mediabunny: `sampleQueue.length + decodeQueueSize > computeMaxQueueSize(…)` then `await queueDequeue` resolved on **new** sample output **or consumer take**. Port: `held.len() + decodeQueueSize` vs `compute_max_queue_size(held.len())`; wait on `ondequeue` / new `output` / `setTimeout(0)`. Wake dequeue waiters on output, `take_held`, prune-close, and GOP consumer drain (mediabunny `onQueueDequeue`). **Never** treat already-buffered `frames` as progress (instant re-wake while queue stuck → false `backpressure timed out` after ~2–3s). Do not count `held` toward the encoded cap beyond empty/non-empty max pick — held is B-frame reorder. **Dual-async `decode_frame`** (`DecoderCore` + `spawn_local` pump + consumer pull; `mediabunny: src/media-sink.ts mediaSamplesInRange`): pump sets `wait_for_consumer` and awaits at `q==0` until take/prune (starved-spin safety if the needed frame is still unfed). **Single-task paths** (`decode_gop`, key cascade) still escape at `q==0` — no concurrent taker. Wake consumer via `not_empty_waiters` when `held` gains samples. **While cascading key→N, close PTS `< target` in the same remap pass** (`collect_and_prune_held`) — park `>= target` so play/prefetch warm-continue works. Use mediabunny empty/non-empty depths (40/8) only. After `flush()`, clear `key_order`/`fed_through` even on miss (else next delta → `key frame required`). **Play:** `present_and_cache` (not `present_only`); keep `PRESENTATION_QUEUE_CAP` small (4) + `STEP_BEHIND_CAP` (8). **`backpressure timed out (q≈41, held=0)`:** one hard `reset`+retry (same as flush/idle) — wedged Chromium after underrun catch-up. **After settle / sync exact:** `invalidate_decoder` before densify so leftover main `decodeQ` does not compete with the scrub decoder. **`decode_gop` (scrub fill):** drain remapped frames to `on_frame` / dense `insert_frame` as they emerge (`flush_held_into_gop` / `feed_sample_gop`) so `held` stays empty and the ready strip paints per tick — do **not** batch `insert_gop` after the whole GOP. Yield with **`setTimeout(0)`** (macrotask) between feeds so paused rAF can refresh the strip; microtask-only `Promise.resolve` chains starve painting. **Drag densify → settle:** `await_decode_capacity` / `feed_sample(_gop)` must honor cancel every spin (hold / scrub_epoch); do **not** run the 2s `flush_with_timeout` after cancel — abort hard-reset instead. Fill task on `Cancelled` + `hold_densify_for_exact` must **return immediately** (no sleep/continue) so `scrub_busy` clears and exact owns GPU.
- **Closure reentrancy** — never `resolve.call0()` synchronously inside `output`/`dequeue` callbacks. Defer with **`spawn_local`**. On teardown: bump generation, `set_ondequeue(None)`, `close()`, **forget** old Closures.
- **Op cancel** — `WebPlayer::op_epoch` / `cancel_ops` / `begin_op`. After every decode await, if epoch changed → drop owned frame (`OwnedVideoFrame` closes on drop) + `MediaError::Cancelled` (UI treats as soft). `pause()` bumps epoch. Prefetch returns `Ok(false)` on cancel. Host: open/play epoch guards so late async cannot install session/player. **Rapid PREV:** soft `invalidate` (clear timestamps/held only) while chunks are still in the browser decoder desyncs PTS remap → rare `flush/idle without pts=… (held=0)`. Fix: `decoder.reset()`/`recreate` on invalidate; `decoder_reset` flag when the slot is empty; cancel checks inside the decode pump; one reset+retry on flush/idle miss.
- **Media clock** — wall-clock until audio track exists; then video slaves to `AudioContext` (`MediaClock` + `AudioOutput`). Hosts must call `WebPlayer::media_time()` — do not keep a parallel wall timeline in `VideoRef` once audio is attached. Do not open `AudioContext` only as a clock: `resume()` + early `close()` throws `Closed before resume completed`. **Resume on Play before `prime_playback`** — a long prime after the click leaves the context `suspended` (silent). (`mediabunny: examples/media-player getPlaybackTime` / `runAudioIterator`)
- **Playback rate** — forward only (`0.25…8`). Negative rates normalize to `1.0`. Fixed-pitch audio through [`PLAYBACK_RATE_AUDIO_MAX`] (`4×`); above that → wall clock + mute (`is_wall_clock_rate`). Shuttle **present** policy still `≥2` (`is_shuttle_rate`, underrun hold off) and is independent of mute. Audio path: `AudioContext` + WSOLA (`time_stretch::TimeStretcher`; Vanilagy gist ≈ Chromium `preservesPitch`; `mediabunny: no equivalent` rate API). Exact 1× bypasses stretcher. Demo: review slider; **JKL** ladder `1/2/4/8` (J/L step, K → 1× + play/pause); **Space** play/pause without rate reset. On rate **decrease** snap clock to displayed/committed playhead (`set_playback_rate` / `rate_decrease_snap_time`) or J keeps skip-catching. **K while playing:** `pause` then `set_playback_rate(1)` — rate→1× while playing spawns audio; pause after leaves music on a stopped picture. Spawn re-checks `player.playing` + non-wall rate.
- **Loop seam at review rates** — `prepare_loop_if_needed` only after EOF is queued; Hd default cap=4 leaves ~0.1s wall drain at 1.75× while `fill_loop_prime` needs ~0.3s → `NeedsLoop` waits / cold restart (visible pause). Near EOF raise cap to ≈ `budget * fps * rate` (`PlayPolicy::loop_prime_budget_secs`, Hd 0.4 / Uhd 1.0; clamp floor…64). Uhd also expands non-loop near-EOF (`eof_lead_secs=1`) to pull the remaining tail. Do **not** set `restarting` during prepare (prefetch gated by `preparing_loop` only) so the tail keeps presenting. `mediabunny: no equivalent` (no loop/rate API).
- **Future: reverse (deferred)** — WebCodecs has no reverse iterator. Practical approach (`mediabunny: #350`): short forward `decode_gop` / key passes, retain (prefer canvas/`ImageBitmap`, not large live VF queues — Chromium `decodeQ≈41` pool starve), present descending, snap clock on key-hop. Dense −1× realtime needs buffer ≥ next-pass wall time (~2 Short GOPs on 1080p); Long GOPs stay slideshow. Do not hold clock on thin trail during fill (freezes last frames for whole decode). Empty/gap hold only if clock would run away. Key-hop (`|rate|≥2` style): present older sync + snap — refusing `front < target` wedges at reverse entry.
- **Audio schedule** — decode → `AudioData` → `f32-planar` → (`TimeStretcher` when rate ≠ 1) → `AudioBuffer` → `AudioBufferSourceNode` at rounded sample-boundary start time with **node rate 1.0** (never vinyl-pitch via `playbackRate`). Stop all nodes + bump epoch + reset stretcher on pause/seek. Ahead throttle ~1s like mediabunny (media timeline at 1×; wall schedule when stretching). AAC: rebuild ASC from esds profile/freq/chan when `re_mp4` drops raw description (`mp4a.40.{profile}`). Skip schedule when grain offset ≥ buffer duration. Borrow rules: [RefCell / WASM re-entry](#refcell--wasm-re-entry).
- **Yields** — prefer `Promise.resolve` microtask for decode pump turns. `setTimeout`+`Closure::once` hung open on `Decoding frame 0` after key sample read. Still use Closure for `requestAnimationFrame` (do not pass bare Promise `resolve` into rAF). Never await WebCodecs `output` alone — poll + feed-ahead / flush-with-timeout.

### Resource wrappers

Types that must `close()` (or equivalent) or whose browser `clone` is fallible get an RAII owner + borrow trait (`OwnedVideoFrame` / `AsVideoFrame`). Move into cache/queue; borrow to draw. `try_clone` → `Result` (never `Option`). Same pattern later for `AudioData` / similar — not for DOM/`Blob`/`window`.

### RefCell / WASM re-entry

Browser work (`VideoFrame`/`AudioData` drop, decoder `output`/`reset`/`close`, tachys events) can **re-enter Rust on the same stack**. A live `RefMut` across that work → `RefCell already borrowed`.

**Forbidden:** `while let` / `if let` scrutinee that calls `borrow_mut()` (or `borrow()`) when the body does more than bind — the guard spans the body.

```rust
// BAD — RefMut held through reset / schedule / await
while let Some(x) = cell.borrow_mut().pop_front() { /* … */ }
if let Some(mut d) = cell.borrow_mut().take() { d.reset(); }
```

**Required:**

```rust
let taken = cell.borrow_mut().take();
if let Some(mut d) = taken { let _ = d.reset(); }

loop {
    let next = cell.borrow_mut().pop_front();
    let Some(item) = next else { break };
    // cell free
}
```

Also: no `Ref`/`RefMut` across `.await`; clone `Rc`, await, put back. Teardown / nested paths: `try_borrow_mut` and skip (or drop) instead of panicking — `park_presentation_into_cache`, scrub `put_chunk` eviction, `clear_scrub_store`, audio `stop`.

Host UI: `skills/leptos.md` (no borrow across await; defer scrub off `on:pointer*`).

### Scrub cache

Model: [Frame and decode caches](#frame-and-decode-caches).

- Visited Exact frames via `present_and_cache` → densify marker + blit (`put_chunk` replace). Play presents use `present_play_frame` (draw only; densify stays off). `present_only` is for uncommitted scrub preview only.
- `seek_frame` must **not** wipe scrub store / invalidate on every call.
- **Long key→N / play mid-GOP `flush/idle`** — `optimizeForLatency: false` for VOD; park `PTS >= target` in seek `held`; after take, drain decodeQ into held; keep `PRESENTATION_QUEUE_CAP` small (4); never `schedule_scrub_cache_fill` while `CacheFillMode::Play`. After flush clear warm state. **GOP-end:** pump may finish with `q>0` (`fed=N,q>0` at GOP end); `finish_gop_for_target` short drain (~80ms) then flush if needed — a 400ms pre-flush drain underran Play every GOP on 1080p. Soft sequential next-key on Play (no recreate). `e2e/perf.scrub.settle.spec.ts`, baseline `slow-large`.
- **Periodic Play freeze ~0.5s every GOP** — was Long-GOP recreate on key change + long `finish_gop` drain → underrun hold. Soft sequential next-key when idle; scrub non-adjacent still hard. **Do not** hard-recreate on `busy` alone when soft-OK; await idle then soft switch. **Packet pump must cross GOPs** (`samples_after_decode` + `DECODE_WARM_AHEAD`) — in-GOP-only stop → finish_gop every key → ~1 hold/GOP (~0.93× realtime). Early `take_held` respawns **warm_only** pump (saw_target=true) — otherwise feeds to EOF (held≈3200). `mediabunny: src/media-sink.ts` mediaSamplesInRange continuous.
- **Play after scrub:** host must not `seek_frame` from `VideoRef::play` while settle targets the same playhead (restarts key→N). Settle success arms densify via `arm_scrub_densify_after_exact` only when no pending PlayPrime; cancelled settle must not densify/invalidate. `prime_playback` re-decodes into the presentation queue (no VF lift from scrub store).
- Timeline drag: async `scrub_to` — canvas via warm `ImageBitmap` only (`present_from_cache` = blit LRU). Store markers are strip/geometry only — **never** canvas. Miss → soft preview (key/neighbor); **soft-drag** (Short/AllIntra) stops there — no `goto_frame` until settle. **Exact-drag** (Long) may decode VF. Never `commit_frame` **or densify** the uncommitted preview alone without present path.
- **Predictive densify:** drag updates `scrub_dir` (`note_scrub_direction`); `next_gap` hard-prefers along `dir` (`nearer_gap_with_dir`); settle/play `clear_scrub_direction` (neutral). Do **not** cancel an in-flight GOP densify when aim jumps (was: ≥8 frames / other key → constant cancel on rapid scrub, dense stayed empty). Aim only picks the next gap after the GOP completes; budget eviction still drops farthest-from-aim.
- **False Long on short-IP:** scene-cut GOPs shrink `min` → large spread while `max ≤ 30`. Classify Short on **max only** (not spread ≤ 2). scene-cut proxy was `Long ~27 (max 30)` under the old gate. `mediabunny: no equivalent`.
- Pointer-up → `prepare_scrub_settle` (abort scrub epoch, reset scrub decoder, invalidate main decoder, set hold, clear dir) then **always** `goto_frame_with_interim(target)` — do **not** commit/blit target (or key/preview) via soft still. Canvas keeps last drag frame until decode VF. Prune salvages PTS `< target` into scrub store only (no canvas blit). Exact commits when ready; densify after is cheap on that GOP.
- **Settle / seek VideoFrame pool** — salvaging every pruned key→N `VideoFrame` into densify can pressure Chromium's pool. Symptom → close prune salvage farther than `INTERIM_SALVAGE_MAX_BEHIND_US`. Do **not** wipe densify markers on settle.
- **Do not** blit interim (fake play). **Do not** drop salvage to “clean” the strip (forces a second full-GOP decode — warn user per `AGENTS.md` Suboptimal proposals). Do **not** `fill_gop` restart after key on settle — re-feeding the same key wedged at `decodeQ≈11` on long-GOP settle. Do **not** `note_scrub_frame` on settle.
- Fill task must **return** on hold (clear `scrub_busy`) and **must not respawn** while hold is set. Sync seek also `abort_scrub_fill` + salvage. Do **not** compact/wipe other runs on aim change — eviction is farthest-from-aim only.
- `decode_gop` cancel hard-resets (`abort_gop_partial`); already-streamed frames stay in scrub store. Late empty-`input_timestamps` outputs are dropped (not hard-error). At capacity, insert refuses farther-than-resident candidates.
- **Fill progress ≠ `frame_count` growth** — at capacity, evict+insert keeps len stable; count newly resident ids (or gap closed). After `idle_passes≥3`, set `fill_stalled_aim` so the task does not respawn until aim moves / `enter_scrub_cache_pipeline`. Hold optimistic scrub preview until `video.frame` matches.
- PREV miss = one key→prev cascade with interim → step behind ring (capped); present target via `goto_frame_fill_behind`. Hit = take from `step_behind` (no key→N). Next hit = presentation ahead or warm blit / `decode_frame`. **Do not** `invalidate_decoder` / `arm_scrub_densify_after_exact` after sequential step — use `arm_scrub_densify_after_step` (densify only). Seek/settle still invalidate. `pause()` must not bump `scrub_epoch` when already paused. `mediabunny: samples()` forward; reverse restarts from key unless samples retained (`mediaSamplesAtTimestamps` flush on rewind). CanvasSink-style pool present.
- **Blit miss on hold:** `step_soft_goto` — warm `goto_frame` / `goto_frame_fill_behind` **without** `abort_scrub_fill` / `hold_densify_for_exact` so densify keeps refilling blit ahead of aim (`note_scrub_direction` on every step). Was: abort+hold → densify starved → periodic ~3s key→N on 4K hold through “green” markers.
- **Pause densify after Play:** `play()` keeps markers and clears blit; marker `no_gaps` used to disarm fill. `next_gap_target` prefers `ScrubBlitLru::next_gap_frame`; `fill_one_gap` skips only when GOP has markers **and** blit; ingest refreshes bitmap when marker already present. Blit solid-at-cap uses the same `margin_left`/`margin_right` arming as markers (from `ScrubPolicy`) so the window slides before the cursor hits the last warm frame — left wider than right (GOP densify key→forward). Do **not** arm from a pinned timeline edge (`start==0` / `end==last`). **Exact thrash:** `ScrubBlitLru::put` must refuse newcomers that would only evict a closer resident (parity with `insert_chunk`); `next_gap` returns `None` when the only gap would_evict — was expand→evict flicker + 100% CPU at origin/EOF on short clips. **Covered vs Exact width:** on Exact refuse, **keep** the Covered marker (do not roll back) so dim coverage can fill the short clip while Exact stays aim-windowed.
- **Step VF ring** — `displayed` + `step_behind` (8) + presentation ahead (4). Survive Pause; clear on seek / scrub settle / teardown. **Loop seam:** clear presentation + step rings only — **keep** Covered + Exact (same clip). **While playing:** drop aged VF — do **not** `ingest_scrub_frame`. Exact on Play: trim farthest for VF VRAM (not full wipe). Cap total live VF — do not keep whole Long GOP.
- **All-Intra:** soft key switch when idle (`begin_key_cascade`); **skip** gap-fill densify (1-packet seek).

### Playback and host

Engine-side pacing notes; UI queue / signals: `skills/leptos.md`.

- **Underrun clock hold** — when the presentation queue is empty mid-play, **freeze** the media clock (`hold_media_clock_for_underrun`) and let prefetch refill; resume with `resume_media_clock` (warm). Audio uses **soft hold** (`AudioOutput::hold_underrun` — silence + cancel pump, **keep** `AudioDecoder` configured) then `restart_warm` (prime ~80 ms before return). Do **not** hard `stop`+`reset`+reconfigure on underrun/loop seam — that sounded like “audio-only lag” while video stayed on the last/loop-prime frame. Pause/seek still use hard `stop_media_clock` / `start_media_clock`. Do **not** prefer `skip_behind` on underrun — Chrome long-GOP play raced audio → `skipped≈242` then Ended before EOF. EOF: do not `Ended` while presentation still holds frames. Probe: `e2e/player.play.long-gop.spec.ts`.
- **Loop seam audio** — after `apply_loop_prime`, host calls `resume_media_clock(0)` not cold `start_media_clock` (`mediabunny: media-player` keeps audio path; full recreate is for seeks).
- **4K residual loop/EOF seam (parked)** — 2160p / Uhd loop e2e (parked; no committed 4K plate yet): mid-play ≈1.0×, underrun holds **0 ms**, wrap wall ≈60 ms, post-wrap ≈0.98×; max pipeline lag ≈30 ms. Subjective “slight lag” after wrap = brief fps dip (~29→22) + `skipped` 1–2 while first GOP fills — **not** audio hard-stop or clock hold. Next lever if revisited: first-GOP / `loop_prime` warm denser, not soft-hold audio. EOF/loop probe (1080p): `e2e/player.play.eof-loop.spec.ts`. Uhd residual still parked.
- **Play VideoFrame pool** — densify stays **off** while playing (`abort_scrub_fill(false)` on Play — no rearm). **Play behind cap 0** (drop aged VF in `install_displayed`; clear step ring on `play()`) — keeping `STEP_BEHIND_CAP`=8 during Play pinned ~21 VFs with held/ahead and starved Chromium. Pause/scrub still uses behind=8 for Prev. Ready strip = Covered+Exact gradient overlays from `coalesce_frame_runs` (ahead/behind not painted). Exact: on Play **trim** via `scrub_blit_cap_during_play` / `exact_frames_to_free_for_play_vram` (displayed + presentation + slack in YUV → RGBA slots), evict farthest — do **not** `blit.clear()` (was cold Exact every Pause). Pause restores full blit budget + densify. Probes: `e2e/player.play.long-gop.spec.ts`.
- **Play warm-ahead / pump stop** — after `take_held` in dual-async `decode_frame`, do **not** `stop` the packet pump before returning the frame (that raced backpressure → empty `held` → cold pump every 1–2 frames → ~0.75× realtime underrun holds on long-GOP play). Return immediately; background task parks ≥4 ahead then stops. Bump `pump_epoch` before each new cold range so only one feeder runs. `mediabunny: src/media-sink.ts mediaSamplesInRange` keeps decoding while the consumer pulls.
- **Densify blit** — OffscreenCanvas `transferToImageBitmap` (sync) at viewport buffer capped by source display + monitor (`screen`×DPR). Blit cap = aim-centered MiB→frames (`scrub_blit_frame_budget`; recompute on `relayout`). Eviction = farthest from playhead. Seek / Next / Prev / settle paint decode VF or warm blit — never a compressed still on canvas (`mediabunny:` CanvasSink VF + `poolSize`). **GPU flash:** after each progressive densify GOP (`fill_one_gap` made progress), `wait_animation_frame` before the next gap — `sleep_ms(0)` was too short for Chrome GPU reclaim (whole-window black). Do not half-res Exact or shrink the blit window for review hot-drag.
- **Freeze cause** — filling the whole prefetch queue *after* each present blocks the deadline (feels like short freezes on long/heavy clips). Prefetch must use wall-clock spare time instead.
- **Playback tasks** — wall-clock media timeline + rAF present task and a **separate** prefetch task. `present_due` / `prefetch_one` take `&self` with interior mutability so decode awaits never block presents.
- **Commit position after decode** — `window_at` + `commit_frame` so a failed decode does not advance the frame counter.
- **`web_sys_unstable_apis`** — required. Set in `.cargo/config.toml`.
- **`VideoDecoderInit::new` argument order** in current `web-sys` is `(error, output)`.
- **Timestamps in web-sys** — `EncodedVideoChunkInit` uses `i32` µs / `u32` duration; clamp long timelines.
- **Workspace isolation** — do not import `video` / `app` / Leptos into `media/`.
- **Mobile / LAN HTTP** — WebCodecs needs a [secure context](https://developer.mozilla.org/en-US/docs/Web/Security/Secure_Contexts). `http://localhost` OK; LAN IP HTTP is not.
- **Playback canvas size** — blit at CSS contain size × DPR, capped by **source display** and **monitor** (`screen` × DPR; removed fixed edge-1280). Measured from the **parent viewport** box (object-fit: contain). **Lock** buffer size + CSS `aspect-ratio` on open / `WebPlayer::relayout` — resizing `canvas.width/height` every present changes intrinsic aspect and jitters layout ±1px. `video`: `leptos_use::use_resize_observer` on `.wm-viewport` → `relayout`. **Fullscreen:** size change clears canvas pixels — `repaint_after_buffer_resize` from `displayed` (else warm blit); do **not** clear scrub markers/blit (legacy WebP wipe). Soft upscale until densify refreshes at the new size is OK.
- **Throttle play UI** — do not `sync_ui` / update fps signals every frame; ~10 Hz is enough. Reactive status updates every present compete with decode/draw on the main thread.
- **Prefetch at EOF** — if `prefetch_one` returns `false` (end of track / full), the background pump **must** `await` rAF (or sleep). `continue` without yield spins the main thread and freezes the tab.
