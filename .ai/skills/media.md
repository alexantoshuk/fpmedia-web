# Media engine (`webmedia`)

Isolated crate: MP4 demux + presentation-order frame index + player API.
Optional WebCodecs decode/render on `wasm32` (`feature = "webcodecs"`).
No Leptos/`app` dependency. Workspace layout: `.ai/architecture.md`.

## Triage

| Symptom | Look first | Notes |
| ------- | ---------- | ----- |
| Hang / stuck on open, Play disabled | [WebCodecs decode](#webcodecs-decode), Chromium first AVC packet | `flush` hang, `busy`/`!ready`, lying sync |
| `decoder busy` / `needs a key chunk` | Decoder lifecycle, Op cancel, Yields | macrotask `take_decoder`; clear warm state after `flush` |
| `backpressure timed out` | `decodeQueueSize` backpressure | wake on **new** output / take; dual-async pump waits at `q==0`; single-task (`decode_gop` / key cascade) still escapes |
| Scrub exact slow / tab freeze on settle | [Scrub cache](#scrub-cache), backpressure | one key→N cascade + salvage; probes below |
| Settle eng stuck / `q=41` or `flush/idle` | Scrub cache + `begin_key_cascade` | windowed salvage; recreate on key change; one cascade |
| Dense empty while scrubbing | Predictive densify, Scrub cache | finish in-flight GOP; don’t cancel mid-GOP on aim move |
| Play freezes / low fps / mid-GOP `flush/idle` | `present_only`, presentation cap, warm held drain | do not pin play frames into dense; park PTS `>= target` |
| Pause→Seek same / Prev→Next ≠ pause frame | Scrub cache `put_presented_frame` | canvas must replace dense resident |
| Play lag right after scrub (long GOP) | Scrub cache + `skills/leptos.md` Play after scrub | seek restart / densify before PlayPrime |
| A/V desync / silent play with audio track | [Audio + media clock](#audio--media-clock) | video must use `WebPlayer::media_time()` (AudioContext), not host wall-clock |
| `RefCell already borrowed` (decode / scrub / open / Drop) | [RefCell / WASM re-entry](#refcell--wasm-re-entry) | `if let`/`while let` + `borrow_mut`; JS re-entry |
| QT / large `.mov` open freeze | Demux `re_mp4` sample ranges | size-0 `skip_box`; no `CachedSource` during verify |
| UI stuck / controls dead (not decode) | `skills/leptos.md` triage | OpQueue, `playing`, `spawn_local` |

**Probes:** `e2e/player.smoke.spec.ts`; scrub settle `e2e/perf.scrub.settle.spec.ts`; drag→settle `e2e/perf.scrub.drag-settle.spec.ts`; general scrub `e2e/perf.scrub.probe.spec.ts`. **Decode-sink gate:** `e2e/perf.scrub.baseline.spec.ts` / `npm run e2e:baseline:save|compare` (BBB + Wing It; `e2e/baselines/`).

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
    ├── frame_cache.rs     ← DenseFrameCache + FrameRunIndex + ThumbCache shell
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

Decoded `VideoFrame` residency (`wasm` + `webcodecs`). Dual `VideoDecoder` so scrub fill does not steal play.

```text
  Play path                          Scrub / pause path
 ─────────────                       ──────────────────
  main VideoDecoder                  scrub VideoDecoder
       │                                    │
       ▼                                    ▼
  presentation queue                 DenseFrameCache
  (ahead, ~16 frames)                (visited + gap-fill)
       │                                    │
       └──────────► canvas ◄────────────────┘
```

| Store | Role | Ownership |
| ----- | ---- | --------- |
| **Presentation queue** | Play-ahead for deadline present | `WebPlayer` deque; `PRESENTATION_QUEUE_CAP` (16) |
| **`DenseFrameCache`** | Exact-frame hits for scrub / step / seek | `BTreeMap` residency + aim/budget; `FrameRunIndex` for gaps |
| **`ThumbCache`** | Filmstrip bitmaps (shell; not wired) | Separate LRU / budget from dense |

| Constant | Default | Meaning |
| -------- | ------- | ------- |
| `DEFAULT_SCRUB_CACHE_MIB` | 1280 | Dense-cache RAM budget (decoded-frame estimate; clamped by `MAX_SCRUB_CACHE_FRAMES`) |
| `MIN_SCRUB_CACHE_FRAMES` / `MAX_SCRUB_CACHE_FRAMES` | 8 / 512 | Floor / ceiling after MiB→frames |
| `PRESENTATION_QUEUE_CAP` | 16 | Play prefetch depth (independent of dense) |
| `SCRUB_CACHE_EDGE_MARGIN` | 24 | After `no_gaps`, re-arm fill only within N of either cache edge |

**Byte → frames:** `scrub_cache_frame_budget(count, w, h, mib)` ≈ `mib×1024² / (w×h×1.5)` (YUV 4:2:0), then clamp to `[MIN, MAX]` and `frame_count`. Same budget for every `GopProfile`; short clips that fit stay fully resident. Wire via `VideoMetadata::scrub_cache_frame_budget`.

Eviction (dense): drop farthest from **aim**; protect the aim frame (victim = endpoint of **evictable** keys). Gap geometry lives in **`FrameRunIndex`**: O(1) when `no_gaps` / playhead missing / adjacent hole / cached `playhead_run`; else one BTree walk of the local run. Gap pick uses **predictive densify** (below). **Never** `(playhead > 0).then_some(playhead - 1)` — `then_some` evaluates the subtrahend eagerly and overflows at frame 0; use `checked_sub` / `filter`. Ownership: move into cache; borrow to draw (`with_frame` / `AsVideoFrame`) — see [Resource wrappers](#resource-wrappers).

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

**One entry:** `WebPlayer::enter_scrub_cache_pipeline(focus)` for pause, scrub drag/hold/release, and post-seek/step: scrub mode → park presentation queue into dense → aim at `focus` → `scrub_cache_fill_task` densifies the **GOP** of the next gap (predictive along scrub `dir` when set; else nearest, tie→behind) until budget-saturated. `play()` switches to Play mode and cancels scrub fill. **In-flight GOP densify is not cancelled when aim moves** (rapid drag used to abort mid-GOP and leave dense empty); aim only retargets the *next* gap after the current GOP finishes. Abort densify only on settle hold / play / scrub-epoch. **Scrub hybrid:** drag (`scrub_to(..., settle=false)`) keeps densify on; pointer-up settle calls `prepare_scrub_settle` (abort scrub epoch + reset scrub decoder + invalidate main + hold) **before** the async op — never `note_scrub_frame` on settle. Exact: **one** key→N cascade (`goto_frame_with_interim`, same as sync `seek_frame`). Cache-hit key may `commit_frame` early; do **not** `goto_frame(key)` then warm-continue (take key from `held` → `backpressure timed out (q=41, held=0)` on Wing It). Re-present `scrub_preview_choice` while exact runs. Salvage pruned PTS `< target` into dense **without blit** (`PruneInterim`) — do **not** drop salvage for strip aesthetics. Never blit interim. Fill task **exits** on hold. PREV still uses mid-GOP `fill_gop_into_cache` present.

**Contiguous arming:** `DenseFrameCache::no_gaps` when the extent is a single run. While `no_gaps` **and under frame budget**, keep expanding (`lo−1` / `hi+1`). Edge-idle (`SCRUB_CACHE_EDGE_MARGIN`) applies only when **at capacity** (shift window when playhead nears an edge). Visited scrub frames stay until budget eviction (farthest from aim) — do **not** wipe other runs on aim change.

Demo strip: dense exact (green) + presentation ahead (blue). While paused, `video` keeps rAF ticking so marks update during fill. Gap-fill streams each decoded frame into dense immediately (not a post-GOP batch).

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
- **Decoder lifecycle** — warm decode-order pump (`decode_frame`): feed next packets in the GOP until the target PTS is in `held`. Prefer feeding ahead over `flush()`. Do **not** `flush()` between playback frames in the same GOP. **After `flush()`, clear `key_order`/`fed_through`** — Chromium then requires a key chunk (`VideoDecoder needs a key chunk`); Play prime after open seek-0 failed when we flushed frame 0 but left warm state. Prefer one coax delta feed to emit the keyframe instead of flush. **`take_decoder` must await macrotasks (`setTimeout(0)`)** — microtask-only waits can return `decoder busy` before a cancelled peer puts the slot back. **All-Intra:** never early-exit the key poll while `input_timestamps` remain; avoid multi-second flush (pins the slot) — hard re-feed once, short flush only as last resort. Soft `begin_key_cascade` only when queue empty and no pending remap.
- **B-frame held buffer + PTS remap** — park not-yet-presented frames in `held` (`PTS >= target` during seek prune — never drop post-target or play/prefetch loses warm ahead). Prefer matching `VideoFrame.timestamp()` when present in `input_timestamps`, else FIFO. After taking the target, briefly drain already-decoded outputs into `held` before stopping the pump. **`optimizeForLatency: false`** for VOD (true → `backpressure q≈41` / mid-GOP wedge on DRYP play). Do **not** require `prefer-hardware` in `isConfigSupported`.
- **`decodeQueueSize` backpressure** — mediabunny: `sampleQueue.length + decodeQueueSize > computeMaxQueueSize(…)` then `await queueDequeue` resolved on **new** sample output **or consumer take**. Port: `held.len() + decodeQueueSize` vs `compute_max_queue_size(held.len())`; wait on `ondequeue` / new `output` / `setTimeout(0)`. Wake dequeue waiters on output, `take_held`, prune-close, and GOP consumer drain (mediabunny `onQueueDequeue`). **Never** treat already-buffered `frames` as progress (instant re-wake while queue stuck → false `backpressure timed out` after ~2–3s). Do not count `held` toward the encoded cap beyond empty/non-empty max pick — held is B-frame reorder. **Dual-async `decode_frame`** (`DecoderCore` + `spawn_local` pump + consumer pull; `mediabunny: src/media-sink.ts mediaSamplesInRange`): pump sets `wait_for_consumer` and awaits at `q==0` until take/prune (starved-spin safety if the needed frame is still unfed). **Single-task paths** (`decode_gop`, key cascade) still escape at `q==0` — no concurrent taker. Wake consumer via `not_empty_waiters` when `held` gains samples. **While cascading key→N, close PTS `< target` in the same remap pass** (`collect_and_prune_held`) — park `>= target` so play/prefetch warm-continue works. Use mediabunny empty/non-empty depths (40/8) only. After `flush()`, clear `key_order`/`fed_through` even on miss (else next delta → `key frame required`). **Play:** `present_only` (do not pin every frame into dense); keep `PRESENTATION_QUEUE_CAP` small (6). **After settle / sync exact:** `invalidate_decoder` before densify so leftover main `decodeQ` does not compete with the scrub decoder. **`decode_gop` (scrub fill):** drain remapped frames to `on_frame` / dense `insert_frame` as they emerge (`flush_held_into_gop` / `feed_sample_gop`) so `held` stays empty and the ready strip paints per tick — do **not** batch `insert_gop` after the whole GOP. Yield with **`setTimeout(0)`** (macrotask) between feeds so paused rAF can refresh the strip; microtask-only `Promise.resolve` chains starve painting. **Drag densify → settle:** `await_decode_capacity` / `feed_sample(_gop)` must honor cancel every spin (hold / scrub_epoch); do **not** run the 2s `flush_with_timeout` after cancel — abort hard-reset instead. Fill task on `Cancelled` + `hold_densify_for_exact` must **return immediately** (no sleep/continue) so `scrub_busy` clears and exact owns GPU.
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

Also: no `Ref`/`RefMut` across `.await`; clone `Rc`, await, put back. Teardown / nested paths: `try_borrow_mut` and skip (or drop) instead of panicking — `park_presentation_into_cache`, dense `insert_frame` eviction, `clear_dense_cache`, audio `stop`.

Host UI: `skills/leptos.md` (no borrow across await; defer scrub off `on:pointer*`).

### Scrub cache

Model: [Frame and decode caches](#frame-and-decode-caches).

- Visited frames via `present_and_cache` (**move**, not silent clone) — including the **play** present path (not `present_only`; that left dense stuck on the open/seek island so pause gap-fill saw holes around the real playhead). `present_only` is for uncommitted scrub preview only. **Must `put_presented_frame` (replace + force budget):** plain `insert_frame` skips existing ids / refuses far inserts → canvas shows A while dense keeps stale/missing A → Pause→Seek same frame or Prev→Next paints the wrong bitmap.
- `seek_frame` must **not** `clear_dense_cache` / invalidate on every call.
- Timeline drag: async `scrub_to` — interim canvas via `scrub_preview_choice`: **cached neighbor if `|c−t| ≤ |key−t|`**, else key (tie → cache; avoids key-flicker across holes). Never `commit_frame` **or densify** the preview (`present_only` / cache draw).
- **Predictive densify:** drag updates `scrub_dir` (`note_scrub_direction`); `next_gap` hard-prefers along `dir` (`nearer_gap_with_dir`); settle/play `clear_scrub_direction` (neutral). Do **not** cancel an in-flight GOP densify when aim jumps (was: ≥8 frames / other key → constant cancel on rapid scrub, dense stayed empty). Aim only picks the next gap after the GOP completes; budget eviction still drops farthest-from-aim.
- Pointer-up → `prepare_scrub_settle` (abort scrub epoch, reset scrub decoder, invalidate main decoder, set hold, clear dir) then **one** `goto_frame_with_interim(target)` (cache-hit key may commit first). Prune salvages PTS `< target` into dense only (no canvas blit). Exact commits when ready; densify after is cheap on that GOP.
- **Settle / seek VideoFrame pool** — salvaging every pruned key→N `VideoFrame` into dense can pressure Chromium's pool. Symptom → close prune salvage farther than `INTERIM_SALVAGE_MAX_BEHIND_US`. Do **not** `trim_to_near` the whole dense strip on settle (pointer-up looked like cache vanishing on a warm region).
- **Long key→N / play mid-GOP `flush/idle`** — `optimizeForLatency: false` for VOD; park `PTS >= target` in seek `held`; after take, drain decodeQ into held; play uses `present_only` + small presentation cap (dense pin + large queue starved the VideoFrame pool on DRYP). After flush clear warm state. `e2e/perf.scrub.settle.spec.ts`, baseline `slow-large`.
- **Play after scrub:** host must not `seek_frame` from `VideoRef::play` while settle targets the same playhead (restarts key→N). Settle success arms densify via `arm_scrub_densify_after_exact` only when no pending PlayPrime; cancelled settle must not densify/invalidate. `prime_playback` **moves** contiguous dense frames ahead of playhead into presentation (`take_frame`) before decoding.
- **Do not** blit interim (fake play). **Do not** drop salvage to “clean” the strip (forces a second full-GOP decode — warn user per `AGENTS.md` Suboptimal proposals). Do **not** `fill_gop` restart after key on settle — re-feeding the same key wedged at `decodeQ≈11` on Wing It. Do **not** `note_scrub_frame` on settle.
- Fill task must **return** on hold (clear `scrub_busy`) and **must not respawn** while hold is set. Sync seek also `abort_scrub_fill` + salvage. Do **not** compact/wipe other runs on aim change — eviction is farthest-from-aim only.
- `decode_gop` cancel hard-resets (`abort_gop_partial`); already-streamed frames stay in dense. Late empty-`input_timestamps` outputs are dropped (not hard-error). At capacity, `insert_frame` refuses farther-than-resident candidates.
- **Fill progress ≠ `frame_count` growth** — at capacity, evict+insert keeps len stable; count newly resident ids (or gap closed). After `idle_passes≥3`, set `fill_stalled_aim` so the task does not respawn until aim moves / `enter_scrub_cache_pipeline`. Hold optimistic scrub preview until `video.frame` matches.
- PREV miss = whole GOP into dense (streamed); present target mid-GOP via `fill_gop_into_cache(..., Some(prev))`. `pause()` must not bump `scrub_epoch` when already paused; cache-hit PREV/NEXT must not `begin_op`.
- **All-Intra:** soft key switch when idle (`begin_key_cascade`); **skip** gap-fill densify (1-packet seek).

### Playback and host

Engine-side pacing notes; UI queue / signals: `skills/leptos.md`.

- **Behind-deadline skip** — `video` loop calls `skip_behind(lag)` when wall clock is past the next present deadline: jump to the frame at the catch-up media time (nearest keyframe if lag ≥ ~0.5s), then snap the deadline to now. Do not try to catch up by decoding every skipped present.
- **Freeze cause** — filling the whole prefetch queue *after* each present blocks the deadline (feels like short freezes on long/heavy clips). Prefetch must use wall-clock spare time instead.
- **Playback tasks** — wall-clock media timeline + rAF present task and a **separate** prefetch task. `present_due` / `prefetch_one` take `&self` with interior mutability so decode awaits never block presents.
- **Commit position after decode** — `window_at` + `commit_frame` so a failed decode does not advance the frame counter.
- **`web_sys_unstable_apis`** — required. Set in `.cargo/config.toml`.
- **`VideoDecoderInit::new` argument order** in current `web-sys` is `(error, output)`.
- **Timestamps in web-sys** — `EncodedVideoChunkInit` uses `i32` µs / `u32` duration; clamp long timelines.
- **Workspace isolation** — do not import `video` / `app` / Leptos into `media/`.
- **Mobile / LAN HTTP** — WebCodecs needs a [secure context](https://developer.mozilla.org/en-US/docs/Web/Security/Secure_Contexts). `http://localhost` OK; LAN IP HTTP is not.
- **Playback canvas size** — blit at CSS contain size × DPR (cap edge 1280), measured from the **parent viewport** box (object-fit: contain), not full coded res. **Lock** buffer size + CSS `aspect-ratio` on open / `WebPlayer::relayout` — resizing `canvas.width/height` every present changes intrinsic aspect and jitters layout ±1px. `video`: `leptos_use::use_resize_observer` on `.wm-viewport` → `relayout`.
- **Throttle play UI** — do not `sync_ui` / update fps signals every frame; ~10 Hz is enough. Reactive status updates every present compete with decode/draw on the main thread.
- **Prefetch at EOF** — if `prefetch_one` returns `false` (end of track / full), the background pump **must** `await` rAF (or sleep). `continue` without yield spins the main thread and freezes the tab.
