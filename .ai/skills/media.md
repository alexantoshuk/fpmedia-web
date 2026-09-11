# Media engine (`webmedia`)

Isolated crate: MP4 demux + presentation-order frame index + player API.
Optional WebCodecs decode/render on `wasm32` (`feature = "webcodecs"`).
No Leptos/`app` dependency.

## Layout

```
media/src/                 ← package name: webmedia
├── lib.rs
├── types.rs               ← FrameId, Timestamp, EncodedSample, VideoMetadata
├── bitstream/             ← AVC packet type (verifyKeyPackets-style)
├── source/                ← MediaSource + MemorySource + CachedSource (+ wasm BlobSource)
├── frame_index.rs         ← presentation order, nearest_keyframe, decode_window
├── demux/
│   ├── boxes.rs           ← top-level ISOBMFF scan + ftyp/moov stub
│   └── mp4.rs             ← re_mp4 → Mp4VideoTrack; open_mp4_bytes / open_mp4_source
├── player.rs              ← play/pause/seek_frame/step → DecodeWindow
└── decode/                ← cfg(wasm32 + webcodecs)
    ├── webcodecs.rs       ← VideoDecoder cascade + decode_gop
    ├── frame_cache.rs     ← DenseFrameCache + ThumbCache shell
    ├── canvas.rs          ← draw VideoFrame
    └── web_player.rs      ← WebPlayer façade (open_blob)

video/                     ← Leptos <Video> + VideoRef (HTML <video> analog)
app/                       ← demo host UI (uses video::Video)
public/                    ← sample MP4 fixtures
```

## Frame vs time

- **Primary position** = presentation `FrameId` (0-based), sorted by composition timestamp.
- Timestamps come from the sample table — do **not** use `frame = time * fps` as truth (VFR).

## Seek cost

Exact frame N does **not** require all-intra video. Cascade:

```text
nearest sync (by decode timestamp) → decode … → target
```

`Player::seek_frame` returns a `DecodeWindow`; `WebPlayer` feeds it to WebCodecs, keeps the target `VideoFrame`, draws to canvas.

## Mediabunny reference (required)

**Rule (also in `AGENTS.md`):** for every demux / decode / seek / playback / source change, open [Vanilagy/mediabunny](https://github.com/Vanilagy/mediabunny) first, port the pattern into Rust, and only invent when there is no equivalent. Cite `src/…` symbols in gotchas. Do **not** depend on the TS package.

Useful areas there: packet/sample/canvas sinks, decode-order vs presentation-order, `decodeQueueSize` backpressure (`media-sink.ts` `queueDequeue` / `onQueueDequeue` on **output**), keyframe cascade / flush rules, playback clock pacing, `BlobSource` reads.

Rust/WASM friction (Closures, `JsFuture`, single-threaded `RefCell`) is adaptation of the same control flow — not a license to redesign waits. Recent failures (`decoder busy`, `needs a key chunk`, `backpressure timed out`) were divergences from mediabunny, not Rust impossibility.

### Patterns already / still to adapt

- Wall-clock deadline pacing (sleep remainder after decode) — done in `video` play loop.
- Decode-order pump + held buffer + PTS remap + output-callback wait (`decode_frame`) — done; prefer feed-ahead over flush.
- Bounded presentation queue (decode ~6 frames ahead) + `decodeQueueSize` backpressure + skip-when-behind — done on `WebPlayer` / `video` loop.
- Bitstream keyframe verification (`verifyKeyPackets`) — AVC at demux: demote lying `stss` sync flags via IDR NAL check; HEVC / Recovery Point SEI still open. **Blob/progressive open must use async verify** (`verify_avc_sync: true`); sync `block_on` cannot drive `BlobSource`. Lying syncs (e.g. Wing It frames 202/239) make skip-behind jump to a non-IDR → decode/prime fails mid-play.
- Seek/pause cancel of in-flight decode — `WebPlayer` `op_epoch` (mediabunny dispose / asyncId); UI host `OpQueue` `async_id` for open/play-prime/prefetch. Video-only clock uses **wall time** until an audio track exists (avoid `AudioContext.resume`/`close` races: `Closed before resume completed`). **Async scrub** (`HostOp::Scrub` / `scrub_to`): cancel in-flight + latest wins, no `busy`; key preview then exact (mediabunny `seekToKey`→`seekTo`). **Sync** Prev/Next/Seek latch `busy` and wait their turn (preempt scrub only).
- **Open from file input must use `wasm_bindgen_futures::spawn_local`**, not `leptos::task::spawn_local`. The app file picker runs outside a Leptos reactive owner; leptos spawn can leave the future unscheduled (console stops at `open_blob start`, UI stuck on the default status).
- Hold `WebPlayer` as `Rc` and **never** `RefCell::borrow` across `.await` in the UI host — that blocks `pause()`/`cancel_ops`. Serialize open/seek/play-prime on `OpQueue` (do not `if busy { return }`). Pause bumps `async_id` and must not force `busy=false` while open/seek still drain.
- **Quick Pause→Play** — cancelled prefetch may still hold the decoder slot; `decode_frame` must wait/yield for it (not fail `decoder busy`). Stale play tasks must not stop the new session unless `async_id` still matches.
- **Transport `playing` is UI-owned** — never `playing.set(engine.playing)` in `sync_ui`. After Pause, that clobber left `playing=true` with no session → Play/Next/Prev all `disabled` / silent no-op. Recover stale `playing` when session is `None`.
- Blob reads: mediabunny WebKit path — `blob.slice(start,end).arrayBuffer()` (avoid `blob.stream()` on Safari/iOS).
- Do not pass Promise `resolve` directly to `requestAnimationFrame` or `setTimeout` (hang risk); use a `Closure` or `setTimeout` + `Closure::once`.
- **Chromium first AVC packet** — strip NALs before AUD and reserved types 20–31 (mediabunny `VideoDecoderWrapper` / Chromium issues 470109459, #396). Without this, `decode()` accepts the key but never emits → open hangs on `flush()` / seek 0; Play stays disabled (`busy`/`!ready`). Race `flush()` with a timeout as a safety net.
- Audio track + shared A/V media clock.
- Later: packet / sample / canvas sink layering; sparse samples-at-timestamps for filmstrip.

## Gotchas

1. **`re_mp4` sample ranges** are **file-absolute**. Progressive open loads full `ftyp`+`moov` plus **headers** of skipped top-level boxes (`mdat`/`free`/…): `re_mp4` reads those headers before `skip_box` seeks. Zero-filled gaps look like `size=0` and abort. **Do not** wrap the source in `CachedSource` during demux — an 8 MiB prefetch on an `mdat` header pulls media into WASM and freezes open on large files; cache only after parse for sample I/O. Header-sized reads (≤16 B) never prefetch. Demo uses `open_blob` with **async** AVC sync verify after moov.
1b. **QT / `.mov` AAC (`mp4a` version 2)** — crates.io `0.5.1` fails open (`mp4a box contains a box with a larger size than it`). Workspace pins [alexantoshuk/re_mp4@a6dceaa](https://github.com/alexantoshuk/re_mp4/commit/a6dceaa656469bd17be77c7d7e2e2b696835d0e6) (PR [rerun-io/re_mp4#35](https://github.com/rerun-io/re_mp4/pull/35)) until upstream merges.
2. **AVCC vs Annex-B** — MP4 `avc1` samples are length-prefixed; WebCodecs wants that plus `avcC` in `VideoDecoderConfig.description`. Length size = `(avcC[4] & 0b11) + 1`.
2b. **Lying sync samples** — on AVC open, demote `is_sync` when the AU is not an IDR (NAL type 5). Mediabunny `verifyKeyPackets`. Does not yet handle Recovery Point SEI or HEVC IRAP.
3. **B-frames** — index is presentation order; `nearest_keyframe` / `decode_window` use **`decode_order`** (not DTS range). Fixture `with_b_frames.mp4` covers this.
4. **WebCodecs target match** — pick the sample with `frame == target_frame`. Never use `samples.last()`.
5. **Decoder lifecycle** — warm decode-order pump (`decode_frame`): feed next packets in the GOP until the target PTS is in `held`. Prefer feeding ahead over `flush()`. Do **not** `flush()` between playback frames in the same GOP. **After `flush()`, clear `key_order`/`fed_through`** — Chromium then requires a key chunk (`VideoDecoder needs a key chunk`); Play prime after open seek-0 failed when we flushed frame 0 but left warm state. Prefer one coax delta feed to emit the keyframe instead of flush.
5b. **B-frame held buffer + PTS remap** — park not-yet-presented frames in `held`. Assign PTS from sorted `input_timestamps` as frames emerge (mediabunny `VideoDecoderWrapper`). After each feed: if `decodeQueueSize == 0` and no output, **feed more** (reorder). If queue > 0, wait for output **raced with `Promise.resolve` microtask** — never await output alone (hangs open on B-frames). Wake waiters via `spawn_local` (not raw `queueMicrotask(resolve)` — that can fail to run and freeze open with `busy=true`). Open uses `optimizeForLatency: true`. Do **not** require `prefer-hardware` in `isConfigSupported`.
5c. **`decodeQueueSize` backpressure** — mediabunny: `sampleQueue.length + decodeQueueSize > computeMaxQueueSize(…)` then `await queueDequeue` resolved on **new** sample output **or consumer take**. Port: collect `frames`→`held`, wait on `ondequeue` / new `output` / `setTimeout(0)`. **Never** treat already-buffered `frames` as progress (instant re-wake while queue stuck → false `backpressure timed out` after ~2–3s). Do not count `held` toward the encoded cap beyond empty/non-empty max pick — held is B-frame reorder. **While cascading key→N (PREV / deep seek), prune `held` frames with PTS `< target` on every collect and inside `await_decode_capacity`** — otherwise past VideoFrames fill `held` (e.g. 16), max stays 8, browser queue wedges at q≈9 → `backpressure timed out` on rapid PREV (mediabunny: consumer drain of `sampleQueue`). **Prune `held` before the seek target while pumping** (`collect_and_prune_held`) — mediabunny frees samples as the sink consumer pulls; keeping every early frame open exhausts the VideoFrame pool so `decodeQueueSize` never drains (`q=9, held=16` on rapid PREV key→N).
5c2. **WebCodecs Closure reentrancy** — never `resolve.call0()` synchronously inside `output`/`dequeue` callbacks. Defer with **`spawn_local`**. On teardown: bump generation, `set_ondequeue(None)`, `close()`, **forget** old Closures.
5c3. **Op cancel** — `WebPlayer::op_epoch` / `cancel_ops` / `begin_op`. After every decode await, if epoch changed → `VideoFrame.close()` + `MediaError::Cancelled` (UI treats as soft). `pause()` bumps epoch. Prefetch returns `Ok(false)` on cancel. Host: open/play epoch guards so late async cannot install session/player. **Rapid PREV:** soft `invalidate` (clear timestamps/held only) while chunks are still in the browser decoder desyncs PTS remap → rare `flush/idle without pts=… (held=0)`. Fix: `decoder.reset()`/`recreate` on invalidate; `decoder_reset` flag when the slot is empty; cancel checks inside the decode pump; one reset+retry on flush/idle miss.
5c4. **Media clock** — wall-clock until audio track exists. Do not open `AudioContext` only as a clock: `resume()` + early `close()` throws `Closed before resume completed`.
5c5. **Yields** — prefer `Promise.resolve` microtask for decode pump turns. `setTimeout`+`Closure::once` hung open on `Decoding frame 0` after key sample read. Still use Closure for `requestAnimationFrame` (do not pass bare Promise `resolve` into rAF). Never await WebCodecs `output` alone — poll + feed-ahead / flush-with-timeout.
5d. **Presentation queue** — `WebPlayer` keeps a bounded decode-ahead queue (~16 `VideoFrame`s). **Present path must not refill**. `video`: background `prefetch_one` to CAP. **Play** full-primes before the media clock. **Loop (one decoder):** once the queue holds EOF, `prepare_loop_if_needed` reuses the main decoder to fill `loop_prime` while the tail still presents; `loop_to_start` swaps buffers (cold `goto_frame(0)` only if prepare missed). **Next step:** pop the queued front when it matches; do not `clear_presentation` first (that closes the only copy of already-emitted frames). **Dense GOP cache (not step ring):** after each present (including play), **move** the `VideoFrame` into `DenseFrameCache` (GOP chunks, `lru`, `max_chunks`+`max_frames`) — never `VideoFrame.clone` on the play path; clone only when re-showing a cache hit. A **scrub** `VideoDecoder` fills missing GOPs around the playhead **only while paused** (`ensure_window`); `play()` bumps `scrub_epoch` to cancel in-flight fill. Never schedule scrub during play — WASM is single-threaded and full-GOP scrub steal caused lag + main `flush/idle without pts` under skip-behind. `ThumbCache` is a separate keyframe-keyed LRU for caller-sized bitmaps (filmstrip later). No lossless compression of frames.
5d2. **Seek must not clear the dense cache** — `seek_frame` used to `clear_dense_cache` + `invalidate_decoder` on every call, so scrub pointermoves always did a cold key→N cascade (multi-second on Wing It / long GOPs). Port: cache-hit present like PREV/NEXT; keep chunks across seeks; let `decode_frame` recreate only when the GOP changes or the target was consumed. Timeline drag uses async `scrub_to` (key **draw** then exact commit, cancelable — never `commit_frame` the key preview or the scrubber jumps key→exact on release); frame input / Seek button stay sync `seek_frame`. Hold optimistic scrub preview until `video.frame` matches.
5d3. **PREV vs NEXT asymmetry** — NEXT reuses a warm decoder / presentation queue. PREV must restart from the key. Do **not** use `decode_frame` on PREV miss (it prunes every held frame before the target, so the next PREV cascades again). Fill the **whole GOP** into `DenseFrameCache` (`decode_gop` / `fill_gop_into_cache`), then present from cache. A single-frame `insert_owned` must not mark the GOP complete — `gop_complete` / `incomplete_chunk_keys` gate scrub fill. `pause()` must not bump `scrub_epoch` when already paused (UI steps call pause every time and were aborting background GOP fill). Cache-hit PREV/NEXT must not `begin_op` (same reason).
5e. **Behind-deadline skip** — `video` loop calls `skip_behind(lag)` when wall clock is past the next present deadline: jump to the frame at the catch-up media time (nearest keyframe if lag ≥ ~0.5s), then snap the deadline to now. Do not try to catch up by decoding every skipped present.
5f. **Freeze cause** — filling the whole prefetch queue *after* each present blocks the deadline (feels like short freezes on long/heavy clips). Prefetch must use wall-clock spare time instead.
6. **Playback** — `video`: **AudioContext clock + rAF present task** and a **separate prefetch task**. `present_due` / `prefetch_one` take `&self` with interior mutability so decode awaits never block presents. Track is `Rc` so decode can hold it across await without locking the playhead.
7. **Commit position after decode** — `window_at` + `commit_frame` so a failed decode does not advance the frame counter.
8. **`web_sys_unstable_apis`** — required. Set in `.cargo/config.toml`.
9. **`VideoDecoderInit::new` argument order** in current `web-sys` is `(error, output)`.
10. **Timestamps in web-sys** — `EncodedVideoChunkInit` uses `i32` µs / `u32` duration; clamp long timelines.
11. **Workspace isolation** — do not import `video` / `app` / Leptos into `media/`.
12. **Mobile / LAN HTTP** — WebCodecs needs a [secure context](https://developer.mozilla.org/en-US/docs/Web/Security/Secure_Contexts). `http://localhost` OK; LAN IP HTTP is not.
14. **Playback canvas size** — blit at CSS contain size × DPR (cap edge 1280), measured from the **parent viewport** box (object-fit: contain), not full coded res. **Lock** buffer size + CSS `aspect-ratio` on open / `WebPlayer::relayout` — resizing `canvas.width/height` every present changes intrinsic aspect and jitters layout ±1px. `video`: `leptos_use::use_resize_observer` on `.wm-viewport` → `relayout`.
15. **Throttle play UI** — do not `sync_ui` / update fps signals every frame; ~10 Hz is enough. Reactive status updates every present compete with decode/draw on the main thread.
16. **Prefetch at EOF** — if `prefetch_one` returns `false` (end of track / full), the background pump **must** `await` rAF (or sleep). `continue` without yield spins the main thread and freezes the tab.

## Commands

```sh
cargo test -p webmedia

# Full Leptos demo (Chrome/Edge recommended)
cargo leptos watch
# → http://127.0.0.1:3000
```
