# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## Handoff (2026-10-02) — ChG Play judder (not holds)

**Symptom (user):** local ChG full ~30 min VO still judders / micro-lags. **Do not upload ChG.**

**Ladder:** ChG 20 min ≈OK (skip=0); full still FAIL; synth OK after prime; mb full VO smooth (prior).

**Series 45 s progression (ChG full):**

| step | skip/s | fps_min | holds |
|------|--------|---------|-------|
| pre (512 MiB + bg quantum) | 1.51 | 8 | 4 |
| `LOCAL_CACHE_BUDGET` 8 MiB | 0.51 | ~17 | 0 |
| + want-only (no bg/margin/`prefetch`) | **0.22** | ~20 | 0 |

Still FAIL (`skip/s`≥0.15, `held_min` 0). Most full skips in first ~2 s after probe discard; mid-play quieter.

**In tree (uncommitted):** Blob local cache port (`LOCAL_CACHE_BUDGET` + want-only) + prior dirty Play-clock/`Bytes`/`sync_source` — **split commits**. Probe: `e2e/_chg_series_probe.mjs`.

**Cite:** `mediabunny: src/source.ts BlobSource` (`maxCacheSize ?? 8MiB`, JS orch `Uint8Array`).

---

## Next up

### Play lag

1. Dig remaining full gap (prime skip / thin held / `warm_sample_bytes` 0.5 MiB WASM tiles) — **cite before edit**; or commit cache port now as partial.
2. Optional mb same-wall series.

### Also open

1. Drop `re_mp4` fork when upstream has #35 + #37.
2. Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc`.

### Later

- Tab-hidden `AudioContinue`; pure `AudioPlayer`; thumb Worker; 4K loop residual; dual-source; publish crates; production chrome.
