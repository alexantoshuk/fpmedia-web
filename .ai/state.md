# State

## In progress

- Smoke-test play/pause/seek on a real Resolve export (1080p+, longer GOPs, B-frames) — fixtures alone are not production-proof.

---

## Next up

### Media engine

- Audio track + shared A/V media clock (video slaves to audio once audio exists).
- HEVC bitstream key verify + AVC Recovery Point SEI (IDR-only verify is in place).

### Later

- Packet / sample / canvas sink layering (mediabunny mental model).
- Sparse `samplesAtTimestamps` for filmstrip thumbnails.
- Dual-source compare layout hooks.
- Publish `webmedia` / `video` crates for reuse by host apps.
- Production player chrome (demo controls in `app` stay for now).
