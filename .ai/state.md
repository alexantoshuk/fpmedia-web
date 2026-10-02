# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-10-02) — Play lag (ChG)

**Focus:** plain **Play judder** (ChG full VO). **Hover deferred.**

**Landed:** silent `PlaybackClock` + live pump; `finish_silent_play_prime` + skip-snap; host `pending_clock_start` (paint→arm); **`samples_after_decode` → `by_decode` O(limit)**; bg sync park 1 s on Play.

**Series ChG full ~45 s (after by_decode):** skip/s **0.09**, skip@t2 **1**, fps_min **23.5**, held_min **1**, holds **0**. Probe exit **0** (`skip/s`<0.15 ∧ holds=0). Strict VERDICT still FAIL (`held_min≥2` / `fps_min≥24` — `pres` healthy).

**Cite:** `FrameIndex::by_decode` (local); `mediabunny: verifyKeyPackets` on retrieve. Local probe: `e2e/_chg_series_probe.mjs` (untracked).

---

## Next up

### Play lag

1. Optional: relax series `held_min` → `pres_min`, or chase residual fps_min 23.5.
2. Optional mb same-wall series.

### Deferred

1. Play hover thumb: if 800 ms floor + 150 ms UI defer still janks → try 1000 ms or Play cache-only (no aim decode).

### Also open

1. Drop `re_mp4` fork when upstream has #35 + #37.
2. Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc`.

### Later

- Tab-hidden `AudioContinue`; pure `AudioPlayer`; thumb Worker; 4K loop residual; dual-source; publish crates; production chrome.
