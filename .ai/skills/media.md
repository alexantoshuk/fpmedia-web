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
├── frame_index.rs         ← presentation order, nearest_keyframe, decode_window
├── demux/mp4.rs           ← re_mp4 → Mp4VideoTrack (+ sync-flag verify for AVC)
├── player.rs              ← play/pause/seek_frame/step → DecodeWindow
└── decode/                ← cfg(wasm32 + webcodecs)
    ├── webcodecs.rs       ← VideoDecoder cascade
    ├── canvas.rs          ← draw VideoFrame
    └── web_player.rs      ← WebPlayer façade

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

For **new media features** and **demux / decode / seek / playback bug fixes**, always consult [Vanilagy/mediabunny](https://github.com/Vanilagy/mediabunny) (guide + `src/`) before inventing an approach. Port patterns into Rust; do **not** depend on the TS package.

Useful areas there: packet/sample/canvas sinks, decode-order vs presentation-order, `decodeQueueSize` backpressure, keyframe cascade / flush rules, playback clock pacing.

### Patterns already / still to adapt

- Wall-clock deadline pacing (sleep remainder after decode) — done in `video` play loop.
- Decode-order pump + held buffer + PTS remap + output-callback wait (`decode_frame`) — done; prefer feed-ahead over flush.
- Bounded presentation queue (decode ~6 frames ahead) + `decodeQueueSize` backpressure + skip-when-behind — done on `WebPlayer` / `video` loop.
- Bitstream keyframe verification (`verifyKeyPackets`) — AVC at demux: demote lying `stss` sync flags via IDR NAL check; HEVC / Recovery Point SEI still open.
- Seek/pause cancel of in-flight decode — `WebPlayer` `op_epoch` (mediabunny dispose / asyncId); UI open/play epochs + `AudioContext.close`; rAF yield without `Closure::forget`.
- Audio track + shared A/V media clock.
- Later: packet / sample / canvas sink layering; sparse samples-at-timestamps for filmstrip.

## Gotchas

1. **`re_mp4` sample ranges** refer to the **same** byte buffer passed to `open_mp4_bytes`. `Mp4VideoTrack` owns that buffer. `open_mp4_path` is native-only (`cfg(not(wasm32))`).
2. **AVCC vs Annex-B** — MP4 `avc1` samples are length-prefixed; WebCodecs wants that plus `avcC` in `VideoDecoderConfig.description`. Length size = `(avcC[4] & 0b11) + 1`.
2b. **Lying sync samples** — on AVC open, demote `is_sync` when the AU is not an IDR (NAL type 5). Mediabunny `verifyKeyPackets`. Does not yet handle Recovery Point SEI or HEVC IRAP.
3. **B-frames** — index is presentation order; `nearest_keyframe` / `decode_window` use **`decode_order`** (not DTS range). Fixture `with_b_frames.mp4` covers this.
4. **WebCodecs target match** — pick the sample with `frame == target_frame`. Never use `samples.last()`.
5. **Decoder lifecycle** — warm decode-order pump (`decode_frame`): feed next packets in the GOP until the target PTS is in `held`. Prefer feeding ahead over `flush()`. Do **not** `flush()` between playback frames in the same GOP. **New keyframe / GOP:** `recreate()` + feed key (soft-flush `begin_new_key` hung open on some Resolve clips — revisit later). **Long GOPs:** pump cap is `frame_count + 32` (not a fixed 64) — Telegram/Resolve clips often have ~250-frame GOPs; cold PREV/seek mid-GOP needs that many feeds in one call. **Consumed target:** if `fed_through` is past the target and its PTS is gone from `input_timestamps`/`held` (presentation queue was cleared), restart from the key — WebCodecs cannot re-emit. Never hold `RefCell`/`borrow_mut` on the decoder across `.await` — **take** the decoder out of `Option` for the decode, then put it back.
5b. **B-frame held buffer + PTS remap** — park not-yet-presented frames in `held`. Assign PTS from sorted `input_timestamps` as frames emerge (mediabunny `VideoDecoderWrapper`). After each feed: if `decodeQueueSize == 0` and no output, **feed more** (reorder). If queue > 0, wait for output **raced with `Promise.resolve` microtask** — never await output alone (hangs open on B-frames). Wake waiters via `spawn_local` (not raw `queueMicrotask(resolve)` — that can fail to run and freeze open with `busy=true`). Open uses `optimizeForLatency: true`. Do **not** require `prefer-hardware` in `isConfigSupported`.
5c. **`decodeQueueSize` backpressure** — before `decode()`, wait while `decodeQueueSize` alone exceeds caps (8 with buffered frames, 40 when empty), raced with a microtask. **Do not** count `held` toward the wait condition — held-full + idle queue deadlocks (no `dequeue` ever fires).
5c2. **WebCodecs Closure reentrancy** — never `resolve.call0()` synchronously inside `output`/`dequeue` callbacks. Defer with **`spawn_local`**. On teardown: bump generation, `set_ondequeue(None)`, `close()`, **forget** old Closures.
5c3. **Op cancel** — `WebPlayer::op_epoch` / `cancel_ops` / `begin_op`. After every decode await, if epoch changed → `VideoFrame.close()` + `MediaError::Cancelled` (UI treats as soft). `pause()` bumps epoch. Prefetch returns `Ok(false)` on cancel. Host: open/play epoch guards so late async cannot install session/player.
5c4. **AudioContext** — `close()` when clearing the play session (pause / stop / superseded play). Do not leave contexts for GC only.
5c5. **rAF yield** — pass Promise `resolve` into `requestAnimationFrame` (no per-frame `Closure::forget`).
5d. **Presentation queue** — `WebPlayer` keeps a bounded decode-ahead queue (~16 `VideoFrame`s). **Present path must not refill**. `video`: background `prefetch_one` to CAP. **Play** full-primes before the media clock. **Loop (one decoder):** once the queue holds EOF, `prepare_loop_if_needed` reuses the main decoder to fill `loop_prime` while the tail still presents; `loop_to_start` swaps buffers (cold `goto_frame(0)` only if prepare missed). **Next step:** pop the queued front when it matches; do not `clear_presentation` first (that closes the only copy of already-emitted frames).
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
