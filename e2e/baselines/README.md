# Scrub perf baselines

JSON baselines for **testvideo** Release clips (AVC 1080p sync plates). Capture
with `save`, re-run with `compare` after decode/scrub changes.

## Clips (canonical)

| Id | Meta | Asset |
| -- | ---- | ----- |
| `avc_1080p_25_gop12_bf0` | AVC 1080p 25 fps GOP12 bf0 ~60 s AAC | [testvideo v0.3.0](https://github.com/alexantoshuk/testvideo/releases/tag/v0.3.0) |
| `avc_1080p_25_gop48_bf0` | AVC 1080p 25 fps GOP48 bf0 ~60 s AAC | same Release |

Opened via demo URL field as same-origin `/media/<stem>.mp4` (static files from
`npm run media:fetch` into `demo-player/public/media/`). Catalog: `e2e/helpers/testvideo.ts`.
Override download base with `TESTVIDEO_BASE`.

**Do not** point baselines at Downloads / ad-hoc local movies. In-repo tiny
fixtures (`demo-player/public/`, `engine/tests/fixtures/`) are for smoke + Rust unit only.

## Scenarios (fixed)

1. `fast-large` — 8 quick moves across ~0.12→0.78
2. `slow-small` — 36 moves with 45 ms delay across ~0.42→0.50
3. `slow-large` — 48 moves with 35 ms delay across ~0.10→0.72

Each scenario records drag time, settle-to-exact (`eng == playhead`) time, and sparse UI samples.

After changing clip ids or scenario defs, run **save** again (old JSON under
other names is obsolete).

## Commands

App must be up on `:3000` (`cd demo-player && trunk serve`).

```sh
# Save under e2e/baselines/pre-decode-sink/{clipId}.json
npm run e2e:baseline:save

# Re-run identical scenarios; fail on exact miss or settle/drag regression
npm run e2e:baseline:compare
```
