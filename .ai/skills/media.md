# Media engine (`webmedia`)

Isolated crate: MP4 demux + presentation-order frame index + player API.
Optional WebCodecs decode/render on `wasm32` (`feature = "webcodecs"`).
No Leptos/`app` dependency. Workspace layout: `.ai/architecture.md`.

## Triage

| Symptom | Look first | Notes |
| ------- | ---------- | ----- |
| Hang / stuck on open, Play disabled | [WebCodecs decode](#webcodecs-decode), Chromium first AVC packet | `flush` hang, `busy`/`!ready`, lying sync |
| `decoder busy` / `needs a key chunk` | Decoder lifecycle, Op cancel, Yields | `take_decoder` waits ~5s wall-clock (Chrome clamps `setTimeout(0)`); clear warm state after `flush` |
| `backpressure timed out` | `decodeQueueSize` backpressure | wake on **new** output / take; `q≈41,held=0` → reset+retry; wire `skip_behind` on underrun |
| Scrub exact slow / tab freeze on settle | [Scrub cache](#scrub-cache), backpressure | one key→N cascade + salvage; probes below |
| Settle eng stuck / `q=41` or `flush/idle` | Scrub cache + `begin_key_cascade` | windowed salvage; recreate on key change; one cascade |
| Dense empty while scrubbing | Predictive densify, Scrub cache | finish in-flight GOP; don’t cancel mid-GOP on aim move |
| Play freezes / low fps / mid-GOP `flush/idle` | presentation cap, warm held drain | small `PRESENTATION_QUEUE_CAP`; park PTS `>= target` |
| Periodic ~0.5s Play freeze every N frames (BBB) | GOP boundary cascade / `finish_gop` | soft sequential next-key (no recreate); short pre-flush drain |
| Chrome `flush/idle` at GOP end (`fed=N,q>0`) | `finish_gop_for_target` | wall-clock drain before flush; DRYP frame 49 |
| Play underrun / huge `skipped`+`lag` then `q=41` | Underrun clock hold | empty present → freeze media clock; resume when buffered (not skip spiral) |
| Chrome play stutter / strip wipe / `flush/idle` mid-GOP (FF OK) | Play VideoFrame pool + underrun hold | keep densify trail (no play trim); DRYP |
| Dense wiped on Play | `present_play_frame` / prime | do **not** trim Exact trail — budget/aim eviction only |
| Scrub blit `source image could not be decoded` | WebP `atob` bytes | `char_code_at`, not `into_bytes`; soft miss → decode |
| Cached scrub / Next-Prev lag on large viewport | Scrub WebP encode | encode = viewport ≤ source×monitor; q=0.5; WebP = drag only |
| Next/Prev hold slow / key→N every step | Step VF ring | `STEP_BEHIND_CAP`+`PRESENTATION_QUEUE_CAP`; no invalidate after step |
| Settle / Seek / Next / Prev look soft | present path | must decode VF → canvas; WebP blit is scrub-drag only |
| Play stutter with growing Exact strip | densify must stay off | `abort_scrub_fill(false)` on Play; no fill rearm; Play ingest ≠ densify |
| Play Exact trail empty after play | `present_play_frame` ingest | gated WebP (`÷DPR`, every-Nth, spare-time); probes: smoke grows Exact |
| Play jank / fullscreen scrub drag lag | WebP Worker | densify+Play use JS `convertToBlob`; sync blit only |
| Dense stuck empty after Pause | `pause` densify kick + hold latch | clear `hold_densify_for_exact`; deferred kick if `scrub_busy` race |
| Regress play/seek after a “local” decode knob tweak | `AGENTS.md` Mediabunny-first pre-edit gate | cite mediabunny path first — no latency/prune/queue invent |
| Pause→Seek same / Prev→Next ≠ pause frame | Scrub cache `put_chunk` / present_and_cache | canvas must replace scrub resident |
| Play lag right after scrub (long GOP) | Scrub cache + `skills/leptos.md` Play after scrub | seek restart / densify before PlayPrime |
| Play after scrub no-op (esp. after EOF) | `skills/leptos.md` Play after scrub | UI `playing` before prime + stale clock → Ended kills PlayPrime |
| A/V desync / silent play with audio track | [Audio + media clock](#audio--media-clock) | video must use `WebPlayer::media_time()` (AudioContext), not host wall-clock |
| `RefCell already borrowed` (decode / scrub / open / Drop) | [RefCell / WASM re-entry](#refcell--wasm-re-entry) | `if let`/`while let` + `borrow_mut`; JS re-entry |
| QT / large `.mov` open freeze | Demux `re_mp4` sample ranges | size-0 `skip_box`; no `CachedSource` during verify |
| UI stuck / controls dead (not decode) | `skills/leptos.md` triage | OpQueue, `playing`, `spawn_local` |

**Probes:** `e2e/player.smoke.spec.ts`; **Chrome DRYP play** `e2e/player.play.dryp.spec.ts`; scrub settle `e2e/perf.scrub.settle.spec.ts`; drag→settle `e2e/perf.scrub.drag-settle.spec.ts`; general scrub `e2e/perf.scrub.probe.spec.ts`. **Decode-sink gate:** `e2e/perf.scrub.baseline.spec.ts` / `npm run e2e:baseline:save|compare` (BBB + Wing It; `e2e/baselines/`).

## Module map (`media/src`)

```
media/src/                 ← package name: webmedia
├── lib.rs
├── types.rs               ← FrameId, Timestamp, EncodedSample, Video/AudioMetadata
├── clock.rs               ← MediaClock (wall until audio; then AudioContext driver)
├── bitstream/             ← AVC packet type (IDR + Recovery Point SEI)
├── source/                ← MediaSource + MemorySource + CachedSource (+ wasm BlobSource)
├── frame_index.rs         ← presentation order; sync_by_decode O(log k) + by_decode GOP map
├── demux/
│   ├── boxes.rs           ← top-level ISOBMFF scan + ftyp/moov stub
│   └── mp4.rs             ← re_mp4 → Mp4VideoTrack + Mp4AudioTrack; OpenedMp4
├── player.rs              ← play/pause/seek_frame/step → DecodeWindow
└── decode/                ← cfg(wasm32 + webcodecs)
    ├── webcodecs.rs       ← DecoderCore + dual-async decode_frame; decode_gop
    ├── audio_output.rs    ← AudioDecoder → AudioBuffer → AudioContext schedule
    ├── owned_audio_data.rs← OwnedAudioData (close on drop)
    ├── frame_cache.rs     ← ScrubChunkStore + FrameRunIndex + ThumbCache shell
    ├── scrub_encode.rs    ← VideoFrame → WebP + ImageBitmap blit
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

**mediabunny:** `examples/media-player/media-player.ts` (`getPlaybackTime` = audio context clock even without audio track when context exists; `runAudioIterator` schedules `BufferSource` at `audioContextStartTime + timestamp - playbackTimeAtStart`); `AudioBufferSink` / `AudioSampleSink` (`src/media-sink.ts`). Do not open `AudioContext` only as a disposable clock (`resume` + early `close` → `Closed before resume completed`).

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

Decoded Exact residency is **WebP chunks** (not long-lived `VideoFrame`s). Dual `VideoDecoder` so scrub fill does not steal play.

```text
  Play path                          Scrub / pause path
 ─────────────                       ──────────────────
  main VideoDecoder                  scrub VideoDecoder
       │                                    │
       ▼                                    ▼
  presentation queue                 densify → ScrubChunkStore (WebP)
  (ahead ≤4 + behind ≤8 live VF)     + ImageBitmap blit LRU
       │                                    ▲
       └──────────► canvas ── present ingest┘
                    (Play: Exact when VF ages out of behind; densify off)
```

| Store | Role | Ownership |
| ----- | ---- | --------- |
| **Presentation queue** | Play-ahead + Next hits | `WebPlayer` deque; `PRESENTATION_QUEUE_CAP` (4) |
| **Step behind ring** | Prev hits (live VF) | `displayed` + `step_behind`; `STEP_BEHIND_CAP` (8) |
| **`ScrubChunkStore`** | Exact scrub/step/seek + Play presented trail | WebP bytes in RAM + `FrameRunIndex`; densify **or** play present encodes then drops VF |
| **`ScrubBlitLru`** | Live scrub blit | `createImageBitmap` LRU (`SCRUB_BLIT_LRU` = 4) |
| **`ThumbCache`** | Filmstrip bitmaps (shell; not wired) | Separate from Exact chunks |

| Constant | Default | Meaning |
| -------- | ------- | ------- |
| `DEFAULT_SCRUB_CACHE_MIB` | 1280 | Scrub-store frame budget (decoded-frame estimate; clamped by `MAX_SCRUB_CACHE_FRAMES`) |
| `MIN_SCRUB_CACHE_FRAMES` / `MAX_SCRUB_CACHE_FRAMES` | 8 / 512 | Floor / ceiling after MiB→frames |
| `PRESENTATION_QUEUE_CAP` | 4 | Play/Next ahead depth (with `STEP_BEHIND_CAP` ≈ 1+8+4 live VF) |
| `STEP_BEHIND_CAP` | 8 | Live VF behind playhead for Prev without key→N |
| `SCRUB_BLIT_LRU` | 96 | Live ImageBitmap handles for scrub present (warm after encode) |
| `SCRUB_CACHE_EDGE_MARGIN` | 24 | After `no_gaps`, re-arm fill only within N of either cache edge |

**Byte → frames:** `scrub_cache_frame_budget(count, w, h, mib)` ≈ `mib×1024² / (w×h×1.5)` (YUV 4:2:0), then clamp to `[MIN, MAX]` and `frame_count`. Same budget for every `GopProfile`; short clips that fit stay fully resident. Wire via `VideoMetadata::scrub_cache_frame_budget`. (Compressed byte-budget eviction is a follow-up.)

Eviction (scrub store): drop farthest from **aim**; protect the aim frame. Gap geometry lives in **`FrameRunIndex`**. Gap pick uses **predictive densify** (below). **Never** `(playhead > 0).then_some(playhead - 1)` — use `checked_sub` / `filter`. Encode: canvas `toDataURL('image/webp', SCRUB_WEBP_QUALITY=0.5)` at **playback buffer size** (viewport contain × DPR, ≤ source display **and** monitor `screen`×DPR) sync in densify `on_frame` so VF closes immediately (Chrome pool). **Do not use `1.0`** — Chromium switches to lossless WebP and densify stalls. Store quality is for drag / strip; **settle** paints decode VF then re-encodes. Blit: `createImageBitmap(Blob)` → draw. `relayout` size change clears scrub store + blit. Play does **not** inflate WebP→VideoFrame for the presentation queue (re-decode / prefetch). `mediabunny:` CanvasSink small canvas pool — no WebP store equivalent.

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

**GOP profile** (`GopProfile` on `VideoMetadata`, from `FrameIndex::gop_profile` after open / AVC sync verify):

| Profile | When | Gap-fill |
| ------- | ---- | -------- |
| `AllIntra` | every sample sync | **off** (seek = 1 packet; soft key switch, no per-frame `recreate`; visited frames still cached) |
| `ShortFixed { len }` | max GOP ≤ 30 and spread ≤ 2 | on |
| `Long { median, max }` | otherwise | on |

Local invention (mediabunny has no equivalent) — fill policy + budget sizing; decode path stays key→cascade.

**All-Intra gotcha:** every frame is a sync, so a naïve `key_order` change must **not** hard-`recreate` the `VideoDecoder` (was: scrub felt uncached + inexact). Soft-clear when idle via `begin_key_cascade`. Do **not** run gap-fill densify — single-frame decode is enough; densify only wastes CPU. Filename may say “H265/4K” while demux picks an AVC 1080p track — trust `metadata`, not the name.

| Mode | Dense fill | Main decoder |
| ---- | ---------- | ------------ |
| **Play** | Off (scrub epoch cancelled) | Present + prefetch into presentation queue |
| **Scrub / paused** | On via scrub decoder (skipped for All-Intra) | Seeks / steps / PREV GOP fill as needed |

**One entry:** `WebPlayer::enter_scrub_cache_pipeline(focus)` for pause, scrub drag/hold/release, and post-seek/step: scrub mode → park presentation into **ScrubChunkStore** (WebP encode) → aim at `focus` → `scrub_cache_fill_task` densifies the **GOP** of the next gap until budget-saturated. `play()` switches to Play mode and cancels scrub fill. **In-flight GOP densify is not cancelled when aim moves**. Abort densify only on settle hold / play / scrub-epoch. **Scrub hybrid:** drag keeps densify on; pointer-up settle calls `prepare_scrub_settle` **before** the async op. Exact: **one** key→N cascade (`goto_frame_with_interim`). Salvage pruned PTS `< target` into scrub store **without blit**. Never blit interim. Fill task **exits** on hold. PREV uses mid-GOP `fill_gop_into_cache` present.

**Contiguous arming:** `ScrubChunkStore::no_gaps` when the extent is a single run. While `no_gaps` **and under frame budget**, keep expanding. Edge-idle (`SCRUB_CACHE_EDGE_MARGIN`) applies only when **at capacity**. Visited scrub frames stay until budget eviction (farthest from aim).

Demo strip: Exact WebP marks (green) + presentation ahead (blue). While paused, `video` keeps rAF ticking so marks update during fill. Gap-fill streams each decoded frame into the scrub store immediately (encode + drop VF).

## Gotchas

### Demux and bitstream

- **`re_mp4` sample ranges** are **file-absolute**. Progressive open loads full `ftyp`+`moov` plus **headers** of all other top-level boxes (`mdat`/`free`/`wide`/…): `re_mp4` reads those headers before `skip_box` seeks. Zero-filled gaps look like `size=0` and abort **or** — with QT padding — `skip_box(0)` **infinite-loops** (seek back to the size-0 header) and freezes the tab on large files (BBB `.mov`). Mitigations: scan headers past `moov`; trim size-0 padding at the end of `moov` children; `SparseBoxReader` refuses size-0 `skip_box` rewinds. **Do not** wrap the source in `CachedSource` during demux or during full-file AVC sync verify — an 8 MiB prefetch per scattered keyframe pulls multi‑GB into WASM (BBB ≈600 syncs); attach cache only after parse+verify for sample I/O. Header-sized reads (≤16 B) never prefetch. Demo uses `open_blob` with **async** AVC sync verify after moov.
- **QT / `.mov` AAC (`mp4a` version 2)** — crates.io `0.5.1` fails open (`mp4a box contains a box with a larger size than it`). Workspace pins [alexantoshuk/re_mp4@cd63eca](https://github.com/alexantoshuk/re_mp4/commit/cd63ecac0269668e09435cc5b9d5f3b06692c09e) (PR [rerun-io/re_mp4#35](https://github.com/rerun-io/re_mp4/pull/35)) until upstream merges.
- **AVCC vs Annex-B** — MP4 `avc1` samples are length-prefixed; WebCodecs wants that plus `avcC` in `VideoDecoderConfig.description`. Length size = `(avcC[4] & 0b11) + 1`.
- **Lying sync samples** — on AVC open, demote `is_sync` when the AU is neither IDR (NAL type 5) nor exact Recovery Point SEI (`sei_nal_is_exact_recovery_point`; mediabunny `determineVideoPacketType` / w3c/webcodecs#650). Chromium &lt;145: SEI keys demoted (`accept_avc_recovery_point_sei`); ≥145 / non-Chromium: kept. Does not yet handle HEVC IRAP.
- **B-frames** — index is presentation order; `nearest_keyframe` / `decode_window` use **`decode_order`** (not DTS range). Fixture `with_b_frames.mp4` covers this.
- **Sync index** — `FrameIndex` keeps `sync_by_decode` (sorted) + dense `by_decode[decode_order]→FrameId`. `nearest_keyframe` / `next_keyframe_after` are O(log k); `gop_frame_ids` / `decode_window` slice the decode map (O(gop)), not a full-frame scan. `set_sync` updates the sync table in place. Local invention (mediabunny has packet cursors, not this index).

### WebCodecs decode

- **Target match** — pick the sample with `frame == target_frame`. Never use `samples.last()`.
- **Decoder lifecycle** — warm decode-order pump (`decode_frame`): feed next packets in the GOP until the target PTS is in `held`. Prefer feeding ahead over `flush()`. Do **not** `flush()` between playback frames in the same GOP. **Play across GOP:** soft `begin_key_cascade(..., sequential_next_key=true)` when idle — feed the next IDR without `VideoDecoder` recreate (mediabunny range pump). Non-adjacent scrub jumps still hard-recreate. **After `flush()`, clear `key_order`/`fed_through`** — Chromium then requires a key chunk (`VideoDecoder needs a key chunk`); Play prime after open seek-0 failed when we flushed frame 0 but left warm state. Prefer one coax delta feed to emit the keyframe instead of flush. **`take_decoder` waits up to ~5s wall-clock** with macrotask yields — a fixed ~120×`setTimeout(0)` was only ~0.5s on Chromium (4ms clamp) and false-fired `decoder busy` during play while Firefox was fine. Cancel only on `op_epoch` change. **All-Intra:** never early-exit the key poll while `input_timestamps` remain; avoid multi-second flush (pins the slot) — hard re-feed once, short flush only as last resort. Soft `begin_key_cascade` only when queue empty and no pending remap.
- **B-frame held buffer + PTS remap** — park not-yet-presented frames in `held` (`PTS >= target` during seek prune — never drop post-target or play/prefetch loses warm ahead). Prefer matching `VideoFrame.timestamp()` when present in `input_timestamps`, else FIFO. After taking the target, briefly drain already-decoded outputs into `held` before stopping the pump. **`optimizeForLatency: false`** for VOD (true → `backpressure q≈41` / mid-GOP wedge on DRYP play). Do **not** require `prefer-hardware` in `isConfigSupported`.
- **`decodeQueueSize` backpressure** — mediabunny: `sampleQueue.length + decodeQueueSize > computeMaxQueueSize(…)` then `await queueDequeue` resolved on **new** sample output **or consumer take**. Port: `held.len() + decodeQueueSize` vs `compute_max_queue_size(held.len())`; wait on `ondequeue` / new `output` / `setTimeout(0)`. Wake dequeue waiters on output, `take_held`, prune-close, and GOP consumer drain (mediabunny `onQueueDequeue`). **Never** treat already-buffered `frames` as progress (instant re-wake while queue stuck → false `backpressure timed out` after ~2–3s). Do not count `held` toward the encoded cap beyond empty/non-empty max pick — held is B-frame reorder. **Dual-async `decode_frame`** (`DecoderCore` + `spawn_local` pump + consumer pull; `mediabunny: src/media-sink.ts mediaSamplesInRange`): pump sets `wait_for_consumer` and awaits at `q==0` until take/prune (starved-spin safety if the needed frame is still unfed). **Single-task paths** (`decode_gop`, key cascade) still escape at `q==0` — no concurrent taker. Wake consumer via `not_empty_waiters` when `held` gains samples. **While cascading key→N, close PTS `< target` in the same remap pass** (`collect_and_prune_held`) — park `>= target` so play/prefetch warm-continue works. Use mediabunny empty/non-empty depths (40/8) only. After `flush()`, clear `key_order`/`fed_through` even on miss (else next delta → `key frame required`). **Play:** `present_and_cache` (not `present_only`); keep `PRESENTATION_QUEUE_CAP` small (4) + `STEP_BEHIND_CAP` (8). **`backpressure timed out (q≈41, held=0)`:** one hard `reset`+retry (same as flush/idle) — wedged Chromium after underrun catch-up. **After settle / sync exact:** `invalidate_decoder` before densify so leftover main `decodeQ` does not compete with the scrub decoder. **`decode_gop` (scrub fill):** drain remapped frames to `on_frame` / dense `insert_frame` as they emerge (`flush_held_into_gop` / `feed_sample_gop`) so `held` stays empty and the ready strip paints per tick — do **not** batch `insert_gop` after the whole GOP. Yield with **`setTimeout(0)`** (macrotask) between feeds so paused rAF can refresh the strip; microtask-only `Promise.resolve` chains starve painting. **Drag densify → settle:** `await_decode_capacity` / `feed_sample(_gop)` must honor cancel every spin (hold / scrub_epoch); do **not** run the 2s `flush_with_timeout` after cancel — abort hard-reset instead. Fill task on `Cancelled` + `hold_densify_for_exact` must **return immediately** (no sleep/continue) so `scrub_busy` clears and exact owns GPU.
- **Closure reentrancy** — never `resolve.call0()` synchronously inside `output`/`dequeue` callbacks. Defer with **`spawn_local`**. On teardown: bump generation, `set_ondequeue(None)`, `close()`, **forget** old Closures.
- **Op cancel** — `WebPlayer::op_epoch` / `cancel_ops` / `begin_op`. After every decode await, if epoch changed → drop owned frame (`OwnedVideoFrame` closes on drop) + `MediaError::Cancelled` (UI treats as soft). `pause()` bumps epoch. Prefetch returns `Ok(false)` on cancel. Host: open/play epoch guards so late async cannot install session/player. **Rapid PREV:** soft `invalidate` (clear timestamps/held only) while chunks are still in the browser decoder desyncs PTS remap → rare `flush/idle without pts=… (held=0)`. Fix: `decoder.reset()`/`recreate` on invalidate; `decoder_reset` flag when the slot is empty; cancel checks inside the decode pump; one reset+retry on flush/idle miss.
- **Media clock** — wall-clock until audio track exists; then video slaves to `AudioContext` (`MediaClock` + `AudioOutput`). Hosts must call `WebPlayer::media_time()` — do not keep a parallel wall timeline in `VideoRef` once audio is attached. Do not open `AudioContext` only as a clock: `resume()` + early `close()` throws `Closed before resume completed`. **Resume on Play before `prime_playback`** — a long prime after the click leaves the context `suspended` (silent). (`mediabunny: examples/media-player getPlaybackTime` / `runAudioIterator`)
- **Audio schedule** — decode → `AudioData` → `f32-planar` → `AudioBuffer` → `AudioBufferSourceNode` at rounded sample-boundary start time; stop all nodes + bump epoch on pause/seek. Ahead throttle ~1s like mediabunny. AAC: rebuild ASC from esds profile/freq/chan when `re_mp4` drops raw description (`mp4a.40.{profile}`). Skip schedule when grain offset ≥ buffer duration. Borrow rules: [RefCell / WASM re-entry](#refcell--wasm-re-entry).
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

- Visited Exact frames via `present_and_cache` → WebP `put_chunk` (replace). Play presents use `present_play_frame` → same ingest after canvas draw (presented-only trail; **no** densify). `present_only` is for uncommitted scrub preview only.
- `seek_frame` must **not** wipe scrub store / invalidate on every call.
- **Long key→N / play mid-GOP `flush/idle`** — `optimizeForLatency: false` for VOD; park `PTS >= target` in seek `held`; after take, drain decodeQ into held; keep `PRESENTATION_QUEUE_CAP` small (4). Play Exact trail = VF age-out from step behind (`present_play_frame` → trim → `ingest_play_exact_frame`); never `schedule_scrub_cache_fill` while `CacheFillMode::Play`. After flush clear warm state. **GOP-end:** pump may finish with `q>0` (DRYP frame 49 `fed=49,q=5`); `finish_gop_for_target` short drain (~80ms) then flush if needed — a 400ms pre-flush drain underran Play every GOP on 1080p BBB. Soft sequential next-key on Play (no recreate). `e2e/perf.scrub.settle.spec.ts`, baseline `slow-large`.
- **Periodic Play freeze ~0.5s every GOP** — was Long-GOP recreate on key change + long `finish_gop` drain → underrun hold. Soft sequential next-key when idle; scrub non-adjacent still hard. `mediabunny: src/media-sink.ts` continuous range.
- **Play after scrub:** host must not `seek_frame` from `VideoRef::play` while settle targets the same playhead (restarts key→N). Settle success arms densify via `arm_scrub_densify_after_exact` only when no pending PlayPrime; cancelled settle must not densify/invalidate. `prime_playback` re-decodes into the presentation queue (no VF lift from scrub store).
- Timeline drag: async `scrub_to` — interim canvas via `scrub_preview_choice`: **cached neighbor if `|c−t| ≤ |key−t|`**, else key (tie → cache; avoids key-flicker across holes). Never `commit_frame` **or densify** the preview (`present_only` / cache draw).
- **Predictive densify:** drag updates `scrub_dir` (`note_scrub_direction`); `next_gap` hard-prefers along `dir` (`nearer_gap_with_dir`); settle/play `clear_scrub_direction` (neutral). Do **not** cancel an in-flight GOP densify when aim jumps (was: ≥8 frames / other key → constant cancel on rapid scrub, dense stayed empty). Aim only picks the next gap after the GOP completes; budget eviction still drops farthest-from-aim.
- Pointer-up → `prepare_scrub_settle` (abort scrub epoch, reset scrub decoder, invalidate main decoder, set hold, clear dir) then **always** `goto_frame_with_interim(target)` — do **not** commit/blit target (or key/preview) via `present_from_cache` (store is low-q; key blit flashed wrong frame). Canvas keeps last drag frame until decode VF. Prune salvages PTS `< target` into scrub store only (no canvas blit). Exact commits when ready; densify after is cheap on that GOP.
- **Settle / seek VideoFrame pool** — salvaging every pruned key→N `VideoFrame` into the scrub encode path can pressure Chromium's pool. Symptom → close prune salvage farther than `INTERIM_SALVAGE_MAX_BEHIND_US`. Do **not** wipe the Exact trail on settle.
- **Do not** blit interim (fake play). **Do not** drop salvage to “clean” the strip (forces a second full-GOP decode — warn user per `AGENTS.md` Suboptimal proposals). Do **not** `fill_gop` restart after key on settle — re-feeding the same key wedged at `decodeQ≈11` on Wing It. Do **not** `note_scrub_frame` on settle.
- Fill task must **return** on hold (clear `scrub_busy`) and **must not respawn** while hold is set. Sync seek also `abort_scrub_fill` + salvage. Do **not** compact/wipe other runs on aim change — eviction is farthest-from-aim only.
- `decode_gop` cancel hard-resets (`abort_gop_partial`); already-streamed frames stay in scrub store. Late empty-`input_timestamps` outputs are dropped (not hard-error). At capacity, insert refuses farther-than-resident candidates.
- **Fill progress ≠ `frame_count` growth** — at capacity, evict+insert keeps len stable; count newly resident ids (or gap closed). After `idle_passes≥3`, set `fill_stalled_aim` so the task does not respawn until aim moves / `enter_scrub_cache_pipeline`. Hold optimistic scrub preview until `video.frame` matches.
- PREV miss = one key→prev cascade with interim → step behind ring (capped); present target via `goto_frame_fill_behind`. Hit = take from `step_behind` (no key→N). Next hit = presentation ahead or warm `decode_frame`. **Do not** `invalidate_decoder` / `arm_scrub_densify_after_exact` after sequential step — use `arm_scrub_densify_after_step` (densify only). Seek/settle still invalidate. `pause()` must not bump `scrub_epoch` when already paused. `mediabunny: samples()` forward; reverse restarts from key unless samples retained (`mediaSamplesAtTimestamps` flush on rewind).
- **Step VF ring** — `displayed` + `step_behind` (8) + presentation ahead (4). Survive Pause; clear on seek / scrub settle / loop / teardown. Play Exact WebP when VF ages out of behind (not on every present). Do **not** park/drop ahead on Pause (was forcing key→N each Next). Cap total live VF — do not keep whole Long GOP.
- **All-Intra:** soft key switch when idle (`begin_key_cascade`); **skip** gap-fill densify (1-packet seek).

### Playback and host

Engine-side pacing notes; UI queue / signals: `skills/leptos.md`.

- **Underrun clock hold** — when the presentation queue is empty mid-play, **freeze** the media clock (`hold_media_clock_for_underrun` / stop audio) and let prefetch refill; resume with `start_media_clock` at the snapped time. Do **not** prefer `skip_behind` here — Chrome DRYP raced audio → `skipped≈242` then Ended before EOF. `skip_behind` remains for explicit catch-up APIs; same-GOP soft (no dense wipe). EOF: do not `Ended` while presentation still holds frames. Probe: `e2e/player.play.dryp.spec.ts`.
- **Play VideoFrame pool** — densify stays **off** while playing (`abort_scrub_fill(false)` on Play — no rearm). Exact marks: Pause densify **and** Play presented-only ingest when VF ages out of the step behind ring (`present_play_frame` → `install_displayed` → trim); **must not** clear/trim Exact on Play — only budget / farthest-from-aim eviction. Play does not lift chunks into the presentation queue (re-decode). Presentation ahead stays small (`PRESENTATION_QUEUE_CAP`=4) + behind (`STEP_BEHIND_CAP`=8). **Pause** clears densify hold and kicks fill; keeps step VF ring. Drag: `clear_densify_hold`. Probes: `e2e/player.smoke.spec.ts` (Pause densifies; Play preserves + grows Exact along playhead); `e2e/player.play.dryp.spec.ts`. `mediabunny:` no Exact WebP trail — local invention.
- **Play Exact / densify WebP Worker** — sync `toDataURL` on main starves rAF. Play + densify: blit LRU at **device pixels** (scrub-drag sharpness); Worker WebP at **CSS size (`÷ DPR`)** for store. Densify inserts `PENDING_WEBP_MIME` placeholder so gaps close. **Canvas present:** scrub-drag may blit WebP; **Seek / Next / Prev / settle** always decode VF (`goto_frame*`) — never `present_from_cache` for those ops (`mediabunny:` CanvasSink VF). Pending queue cap 4. Sync fallback if Worker unavailable.
- **Scrub WebP encode** — densify: OffscreenCanvas `transferToImageBitmap` (sync blit) + `toDataURL('image/webp', SCRUB_WEBP_QUALITY=0.5)` at **viewport buffer** capped by source display + monitor (`screen`×DPR). Quality **`1.0` → Chromium lossless** (stalls); store is **drag/strip only** — Seek/Next/Prev/settle paint decode VF then re-ingest. Blit eviction = farthest from playhead (classic LRU wiped Prev). **Never** `atob`→`into_bytes`. Pause parks current frame only. Large viewport → slower densify (abort fill on exact step/seek).
- **Freeze cause** — filling the whole prefetch queue *after* each present blocks the deadline (feels like short freezes on long/heavy clips). Prefetch must use wall-clock spare time instead.
- **Playback tasks** — wall-clock media timeline + rAF present task and a **separate** prefetch task. `present_due` / `prefetch_one` take `&self` with interior mutability so decode awaits never block presents.
- **Commit position after decode** — `window_at` + `commit_frame` so a failed decode does not advance the frame counter.
- **`web_sys_unstable_apis`** — required. Set in `.cargo/config.toml`.
- **`VideoDecoderInit::new` argument order** in current `web-sys` is `(error, output)`.
- **Timestamps in web-sys** — `EncodedVideoChunkInit` uses `i32` µs / `u32` duration; clamp long timelines.
- **Workspace isolation** — do not import `video` / `app` / Leptos into `media/`.
- **Mobile / LAN HTTP** — WebCodecs needs a [secure context](https://developer.mozilla.org/en-US/docs/Web/Security/Secure_Contexts). `http://localhost` OK; LAN IP HTTP is not.
- **Playback canvas size** — blit at CSS contain size × DPR, capped by **source display** and **monitor** (`screen` × DPR; removed fixed edge-1280). Measured from the **parent viewport** box (object-fit: contain). **Lock** buffer size + CSS `aspect-ratio` on open / `WebPlayer::relayout` — resizing `canvas.width/height` every present changes intrinsic aspect and jitters layout ±1px. `video`: `leptos_use::use_resize_observer` on `.wm-viewport` → `relayout`.
- **Throttle play UI** — do not `sync_ui` / update fps signals every frame; ~10 Hz is enough. Reactive status updates every present compete with decode/draw on the main thread.
- **Prefetch at EOF** — if `prefetch_one` returns `false` (end of track / full), the background pump **must** `await` rAF (or sleep). `continue` without yield spins the main thread and freezes the tab.
