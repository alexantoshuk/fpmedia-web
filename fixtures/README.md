# Test fixtures

Committed MP4s stay **tiny** (~20–50 KB lavfi clips) for Rust unit/integration and
Playwright smoke. Do **not** add camera dumps or multi‑MB media here.

**Naming / git:** only `fixture_*.mp4` in this directory is trackable (see root `.gitignore`).
Rust tests and Playwright `setInputFiles` both read these files. Other `*.mp4` stay ignored
(including optional local `fragmented_tiny.mp4`).

Burn-in on each frame (top-left), three lines: **`f N`** (0-based presentation index),
**SMPTE timecode** `HH:MM:SS:FF` @ 24 fps, and a constant **clip label** (`short` / `bf` / `audio`)
so a playlist can tell the plates apart. The picture underneath is the same `testsrc`.

**FullHD A/V sync plates** live in **[testvideo](https://github.com/alexantoshuk/testvideo)**
Releases (`e2e/helpers/testvideo.ts`). Prefer those for browser play/scrub/JKL
probes — not Downloads / third-party sample hosts.

| File | Description |
| ---- | ----------- |
| `fixture_short_gop.mp4` | Label **`short`**. 320×240, 24 fps, 48 frames (`f 0`…`f 47`), H.264, GOP 12, **no B-frames**, no audio |
| `fixture_with_b_frames.mp4` | Label **`bf`**. Same plate, `-bf 2` (B-frames / reordering) |
| `fixture_with_audio.mp4` | Label **`audio`**. Same plate + AAC sine, ~2s — audio demux / A/V clock fixture |
| `fragmented_tiny.mp4` | **gitignored** (no `fixture_` prefix) — optional local fMP4; prefer URL test below |

### fMP4 via URL (testvideo)

Default: AVC 1080p 25 fps fMP4 GOP12 bf0 from Release v0.3.0
(`avc_1080p_25_fmp4_gop12_bf0.mp4`).

```sh
cargo test -p fpmedia-web --test demux_mp4 progressive_fmp4_from_url -- --ignored --nocapture
# optional override:
# FPMEDIA_WEB_FMP4_URL=https://… cargo test -p fpmedia-web --test demux_mp4 progressive_fmp4_from_url -- --ignored
```

### Regenerate

Needs ffmpeg with `libfreetype` + a TTF (Windows: `C:/Windows/Fonts/arial.ttf`;
Linux: e.g. `/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf`).

Write a one-line filter script (avoids shell escaping of `%{n}` / timecode):

```sh
# FONT=C\\:/Windows/Fonts/arial.ttf   # Windows
# FONT=/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf
FONT="${FONT:-C\\\\:/Windows/Fonts/arial.ttf}"
burnin() {
  cat > /tmp/burnin.fffilter <<EOF
drawtext=fontfile=${FONT}:fontsize=22:fontcolor=white:borderw=2:bordercolor=black:x=8:y=8:text=f %{n},drawtext=fontfile=${FONT}:fontsize=22:fontcolor=white:borderw=2:bordercolor=black:x=8:y=40:timecode=00\\\\:00\\\\:00\\\\:00:r=24,drawtext=fontfile=${FONT}:fontsize=22:fontcolor=white:borderw=2:bordercolor=black:x=8:y=72:text=${1}
EOF
}

burnin short
ffmpeg -y -f lavfi -i testsrc=duration=2:size=320x240:rate=24 \
  -filter_script:v /tmp/burnin.fffilter \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 -bf 0 -an \
  fixtures/fixture_short_gop.mp4

burnin bf
ffmpeg -y -f lavfi -i testsrc=duration=2:size=320x240:rate=24 \
  -filter_script:v /tmp/burnin.fffilter \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 -bf 2 -an \
  fixtures/fixture_with_b_frames.mp4

burnin audio
ffmpeg -y -f lavfi -i "sine=frequency=440:duration=2" \
  -f lavfi -i testsrc=duration=2:size=320x240:rate=24 \
  -filter_script:v /tmp/burnin.fffilter \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 \
  -c:a aac -b:a 64k -shortest \
  fixtures/fixture_with_audio.mp4

ffmpeg -y -f lavfi -i "color=c=black:s=64x64:d=1:r=5" \
  -c:v libx264 -pix_fmt yuv420p \
  -movflags frag_keyframe+empty_moov+default_base_moof \
  fixtures/fragmented_tiny.mp4
```

Last frame of the 48-frame plates: **`f 47`** / **`00:00:01:23`**. Labels stay **`short`**, **`bf`**, **`audio`** on every frame.
