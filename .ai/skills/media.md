# Media engine (`webmedia`)

Isolated crate: MP4 demux + presentation-order frame index + player API.
Optional WebCodecs decode/render on `wasm32` (`feature = "webcodecs"`).
No Leptos/`app` dependency.

## Layout

```
media/src/                 ← package name: webmedia
├── lib.rs
├── types.rs               ← FrameId, Timestamp, EncodedSample, VideoMetadata
├── frame_index.rs         ← presentation order, nearest_keyframe, decode_window
├── demux/mp4.rs           ← re_mp4 → Mp4VideoTrack
├── player.rs              ← play/pause/seek_frame/step → DecodeWindow
└── decode/                ← cfg(wasm32 + webcodecs)
    ├── webcodecs.rs       ← VideoDecoder cascade
    ├── canvas.rs          ← draw VideoFrame
    └── web_player.rs      ← WebPlayer façade

app/                       ← Leptos CSR demo UI (uses WebPlayer)
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

## Mediabunny adaptations (planned)

Ideas from [Vanilagy/mediabunny](https://github.com/Vanilagy/mediabunny) — port patterns, not TS code:

- Promise-driven decode pump + `decodeQueueSize` backpressure (replace busy-poll `wait_for_timestamp`).
- Flush on GOP change / backward seek; warm continue within GOP.
- Bitstream keyframe verification (`verifyKeyPackets`).
- Later: packet / sample / canvas sink layering; sparse samples-at-timestamps for filmstrip.

## Gotchas

1. **`re_mp4` sample ranges** refer to the **same** byte buffer passed to `open_mp4_bytes`. `Mp4VideoTrack` owns that buffer. `open_mp4_path` is native-only (`cfg(not(wasm32))`).
2. **AVCC vs Annex-B** — MP4 `avc1` samples are length-prefixed; WebCodecs wants that plus `avcC` in `VideoDecoderConfig.description`.
3. **B-frames** — index is presentation order; `nearest_keyframe` / `decode_window` use **`decode_order`** (not DTS range). Fixture `with_b_frames.mp4` covers this.
4. **WebCodecs target match** — pick the sample with `frame == target_frame`. Never use `samples.last()`.
5. **Decoder lifecycle** — warm decoder for forward steps in the same GOP (`fed_through_order`). Do **not** `flush()` between playback frames. Flush only as last resort (then invalidate).
6. **Playback** — `WebPlayer::play` / `pause` / `step_playback`; UI paces with `frame_duration_seconds()` (`Result`, no fps/24 fallback). No audio yet.
7. **Commit position after decode** — `window_at` + `commit_frame` so a failed decode does not advance the frame counter.
8. **`web_sys_unstable_apis`** — required. Set in `.cargo/config.toml`.
9. **`VideoDecoderInit::new` argument order** in current `web-sys` is `(error, output)`.
10. **Timestamps in web-sys** — `EncodedVideoChunkInit` uses `i32` µs / `u32` duration; clamp long timelines.
11. **Workspace isolation** — do not import `app` / Leptos into `media/`.
12. **Mobile / LAN HTTP** — WebCodecs needs a [secure context](https://developer.mozilla.org/en-US/docs/Web/Security/Secure_Contexts). `http://localhost` OK; LAN IP HTTP is not.
13. **Leptos + async WebPlayer** — while an async `&mut` decode is in flight, do not call other methods on the same player from another task (Rust aliasing / wasm re-entrancy). Gate UI with a busy flag; pause only after the in-flight future settles.

## Commands

```sh
cargo test -p webmedia

# Full Leptos demo (Chrome/Edge recommended)
cargo leptos watch
# → http://127.0.0.1:3000
```
