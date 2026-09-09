# State

## In progress

- Confirm play/pause/seek on a real Resolve export (fixtures `short_gop` / `with_b_frames` play ~24 fps with deadline pacing + B-frame held buffer).

---

## Next up

### Media engine

- Promise-driven decode pump (mediabunny-style `decodeQueueSize` backpressure) instead of busy-poll `wait_for_timestamp`.
- `verifyKeyPackets`-style bitstream keyframe check for lying containers.
- Audio track + catch-up/skip when decode falls far behind deadline.

### Later

- Packet / sample / canvas sink layering (mediabunny mental model).
- Sparse `samplesAtTimestamps` for filmstrip thumbnails.
- Dual-source compare layout hooks.
- Publish `webmedia` crate for reuse by host apps.
