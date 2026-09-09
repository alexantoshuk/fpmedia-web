# State

## In progress

- **Smoke-test Leptos demo** in Chrome (`cargo leptos watch` → Load sample / seek / step).

---

## Next up

### Media engine

- Confirm play/pause/seek on `short_gop.mp4` and a real Resolve export.
- Promise-driven decode pump (mediabunny-style backpressure) instead of busy-poll `wait_for_timestamp`.
- `verifyKeyPackets`-style bitstream keyframe check for lying containers.
- Continuous play pacing polish + audio track.

### Later

- Packet / sample / canvas sink layering (mediabunny mental model).
- Sparse `samplesAtTimestamps` for filmstrip thumbnails.
- Dual-source compare layout hooks.
- Publish `webmedia` crate for reuse by host apps.
