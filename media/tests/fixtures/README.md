# Test fixtures

Committed MP4s stay **tiny** (~20 KB lavfi clips). Do **not** add camera dumps,
long clips, or multi‑MB media. Prefer URL probes or generate tiny clips locally.
Tests that need an optional local file `skip` when it is missing.

| File | Description |
| ---- | ----------- |
| `short_gop.mp4` | 320×240, 24 fps, 48 frames, H.264, GOP 12, **no B-frames**, no audio |
| `with_b_frames.mp4` | same as above but `-bf 2` (B-frames / reordering) |
| `with_audio.mp4` | 320×240 + AAC sine, ~2s — audio demux / A/V clock fixture |
| `fragmented_tiny.mp4` | **gitignored** — optional local fMP4; prefer URL test below |

### fMP4 via URL (not committed)

Default probe (Pexels vertical, fragmented):

`https://videos.pexels.com/video-files/39355913/16751924_1080_1920_30fps.mp4`

```sh
cargo test -p webmedia --test demux_mp4 progressive_fmp4_from_url -- --ignored --nocapture
# optional override:
# WEBMEDIA_FMP4_URL=https://… cargo test -p webmedia --test demux_mp4 progressive_fmp4_from_url -- --ignored
```


Also copy video fixtures into `public/` so the Leptos demo can fetch them:

```sh
cp media/tests/fixtures/short_gop.mp4 public/short_gop.mp4
cp media/tests/fixtures/with_b_frames.mp4 public/with_b_frames.mp4
cp media/tests/fixtures/with_audio.mp4 public/with_audio.mp4
```

Regenerate:

```sh
ffmpeg -y -f lavfi -i testsrc=duration=2:size=320x240:rate=24 \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 -bf 0 -an \
  media/tests/fixtures/short_gop.mp4

ffmpeg -y -f lavfi -i testsrc=duration=2:size=320x240:rate=24 \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 -bf 2 -an \
  media/tests/fixtures/with_b_frames.mp4

ffmpeg -y -f lavfi -i "sine=frequency=440:duration=2" \
  -f lavfi -i "color=c=blue:s=320x240:d=2:r=24" \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 \
  -c:a aac -b:a 64k -shortest \
  media/tests/fixtures/with_audio.mp4

ffmpeg -y -f lavfi -i "color=c=black:s=64x64:d=1:r=5" \
  -c:v libx264 -pix_fmt yuv420p \
  -movflags frag_keyframe+empty_moov+default_base_moof \
  media/tests/fixtures/fragmented_tiny.mp4

cp media/tests/fixtures/short_gop.mp4 public/short_gop.mp4
cp media/tests/fixtures/with_b_frames.mp4 public/with_b_frames.mp4
cp media/tests/fixtures/with_audio.mp4 public/with_audio.mp4
```
