# Test fixtures

Committed MP4s stay **tiny** (~20–50 KB lavfi clips) for Rust unit/integration and
Playwright smoke. Do **not** add camera dumps or multi‑MB media here.

Burn-in on each frame (top-left): **`f N`** (0-based presentation index) +
**SMPTE timecode** `HH:MM:SS:FF` @ 24 fps — for play/scrub EOF diagnosis.

**FullHD A/V sync plates** live in **[testvideo](https://github.com/alexantoshuk/testvideo)**
Releases (`e2e/helpers/testvideo.ts`). Prefer those for browser play/scrub/JKL
probes — not Downloads / third-party sample hosts.

| File | Description |
| ---- | ----------- |
| `short_gop.mp4` | 320×240, 24 fps, 48 frames (`f 0`…`f 47`), H.264, GOP 12, **no B-frames**, no audio |
| `with_b_frames.mp4` | same as above but `-bf 2` (B-frames / reordering) |
| `with_audio.mp4` | same plate + AAC sine, ~2s — audio demux / A/V clock fixture |
| `fragmented_tiny.mp4` | **gitignored** — optional local fMP4; prefer URL test below |

### fMP4 via URL (testvideo)

Default: AVC 1080p 24 fps fMP4 GOP12 bf0 from Release v0.1.0
(`avc_1080p_24_fmp4_gop12_bf0.mp4`).

```sh
cargo test -p fpmedia-web --test demux_mp4 progressive_fmp4_from_url -- --ignored --nocapture
# optional override:
# FPMEDIA_WEB_FMP4_URL=https://… cargo test -p fpmedia-web --test demux_mp4 progressive_fmp4_from_url -- --ignored
```

Also copy video fixtures into `public/` so the Leptos demo / smoke e2e can open them
(`public/*.mp4` is gitignored — copy after clone):

```sh
cp engine/tests/fixtures/short_gop.mp4 public/short_gop.mp4
cp engine/tests/fixtures/with_b_frames.mp4 public/with_b_frames.mp4
cp engine/tests/fixtures/with_audio.mp4 public/with_audio.mp4
```

### Regenerate

Needs ffmpeg with `libfreetype` + a TTF (Windows: `C:/Windows/Fonts/arial.ttf`;
Linux: e.g. `/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf`).

Write a one-line filter script (avoids shell escaping of `%{n}` / timecode):

```sh
# FONT=C\\:/Windows/Fonts/arial.ttf   # Windows
# FONT=/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf
FONT="${FONT:-C\\\\:/Windows/Fonts/arial.ttf}"
cat > /tmp/burnin.fffilter <<EOF
drawtext=fontfile=${FONT}:fontsize=22:fontcolor=white:borderw=2:bordercolor=black:x=8:y=8:text=f %{n},drawtext=fontfile=${FONT}:fontsize=22:fontcolor=white:borderw=2:bordercolor=black:x=8:y=40:timecode=00\\\\:00\\\\:00\\\\:00:r=24
EOF

ffmpeg -y -f lavfi -i testsrc=duration=2:size=320x240:rate=24 \
  -filter_script:v /tmp/burnin.fffilter \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 -bf 0 -an \
  engine/tests/fixtures/short_gop.mp4

ffmpeg -y -f lavfi -i testsrc=duration=2:size=320x240:rate=24 \
  -filter_script:v /tmp/burnin.fffilter \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 -bf 2 -an \
  engine/tests/fixtures/with_b_frames.mp4

ffmpeg -y -f lavfi -i "sine=frequency=440:duration=2" \
  -f lavfi -i testsrc=duration=2:size=320x240:rate=24 \
  -filter_script:v /tmp/burnin.fffilter \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 \
  -c:a aac -b:a 64k -shortest \
  engine/tests/fixtures/with_audio.mp4

ffmpeg -y -f lavfi -i "color=c=black:s=64x64:d=1:r=5" \
  -c:v libx264 -pix_fmt yuv420p \
  -movflags frag_keyframe+empty_moov+default_base_moof \
  engine/tests/fixtures/fragmented_tiny.mp4

cp engine/tests/fixtures/short_gop.mp4 public/short_gop.mp4
cp engine/tests/fixtures/with_b_frames.mp4 public/with_b_frames.mp4
cp engine/tests/fixtures/with_audio.mp4 public/with_audio.mp4
```

Last frame of the 48-frame plates: **`f 47`** / **`00:00:01:23`**.
