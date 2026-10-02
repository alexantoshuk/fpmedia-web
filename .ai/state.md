# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-10-02) — scrubber strips on canvas

**Landed:** gray/Covered/Exact strips = 1 px `<canvas>` + integer `fillRect` (CSS gradients removed). Soft-merge + paint latch kept as run source. Covered `#1f5742`. User OK on BBB-class long clips.

**Still dirty (not this commit):** Play-clock / `playback_clock.rs` / demux / decode / e2e baselines / product.md / ChG probe.

**Also open — ChG Play judder:** full ~30 min FAIL series (`skip/s`≈0.22). 20 min OK. Do not upload ChG. Probe: `e2e/_chg_play_probe.mjs`.

---

## Next up

### Play lag

1. Dig remaining ChG full gap — **cite before edit**.

### Also open

1. Drop `re_mp4` fork when upstream has #35 + #37.
2. Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc`.

### Later

- Tab-hidden `AudioContinue`; pure `AudioPlayer`; thumb Worker; 4K loop residual; dual-source; publish crates; production chrome.
