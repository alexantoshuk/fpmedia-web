# Test fixtures

| File | Description |
| ---- | ----------- |
| `short_gop.mp4` | 320×240, 24 fps, 48 frames, H.264, GOP 12, **no B-frames**, no audio |
| `with_b_frames.mp4` | same as above but `-bf 2` (B-frames / reordering) |

Also copy `short_gop.mp4` to `public/` so the Leptos demo can fetch `/short_gop.mp4`.

Regenerate:

```sh
ffmpeg -y -f lavfi -i testsrc=duration=2:size=320x240:rate=24 \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 -bf 0 -an \
  media/tests/fixtures/short_gop.mp4

ffmpeg -y -f lavfi -i testsrc=duration=2:size=320x240:rate=24 \
  -c:v libx264 -pix_fmt yuv420p -g 12 -keyint_min 12 -sc_threshold 0 -bf 2 -an \
  media/tests/fixtures/with_b_frames.mp4

cp media/tests/fixtures/short_gop.mp4 public/short_gop.mp4
```
