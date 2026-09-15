# Scrub perf baselines

JSON baselines for BBB + Wing It scrub mechanics. Capture **before** a decode
refactor, re-run with `compare` after.

## Clips (local, not in repo)

| Id | Default path |
| -- | ------------ |
| `bbb` | `%USERPROFILE%/Downloads/big_buck_bunny_1080p_h264.mov` |
| `wing-it` | `%USERPROFILE%/Downloads/WING IT - Blender Open Movie.mp4` |

Override with `WEBMEDIA_PERF_CLIP_BBB` / `WEBMEDIA_PERF_CLIP_WING`.

## Scenarios (fixed)

1. `fast-large` — 8 quick moves across ~0.12→0.78
2. `slow-small` — 36 moves with 45 ms delay across ~0.42→0.50
3. `slow-large` — 48 moves with 35 ms delay across ~0.10→0.72

Each scenario records drag time, settle-to-exact (`eng == playhead`) time, and sparse UI samples.

**Known pre-refactor miss:** Wing It `slow-large` may time out with `eng` stuck behind playhead (long GOP densify vs exact). Save mode still writes the JSON; compare only flags a new exact failure if the baseline had `exactOk: true`.

## Commands

App must be up on `:3000` (`cargo leptos watch`).

```sh
# Save under e2e/baselines/pre-decode-sink/{bbb,wing-it}.json
npm run e2e:baseline:save

# Re-run identical scenarios; fail on exact miss or settle/drag regression
npm run e2e:baseline:compare
```

Tag: `WEBMEDIA_BASELINE_TAG` (default `pre-decode-sink`).
Budgets: `WEBMEDIA_SETTLE_BUDGET` (1.75×), `WEBMEDIA_DRAG_BUDGET` (2.0×),
plus absolute slack env vars in `helpers/scrub-baseline.ts`.
