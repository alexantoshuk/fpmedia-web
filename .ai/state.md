# State

> In progress + Next up (+ Later) only. No Done list — finished items are deleted; durable lessons go to skills / product / architecture (`AGENTS.md` Documentation).

---

## In progress

### Handoff

- **HEAD:** `TimelineRef` shipped (`feat(components): add TimelineRef…`) — Scrubber/HoverThumb shared scrub handle; Sequence backend next. Prior: soft-drag main-HW key chase; ThumbSink; scrub→play.
- **In progress:** Sequence playlist **implementation** starting at **model** (duration map + `SequencePos` / `[i]` helpers). UX/design locked — `product.md` / `architecture.md` § Sequence / `glossary.md` § SequencePos · TimelineRef.
- **Do not land:** `b8206d1` Exact wipe-on-settle (seek Exact-follow ≠ wipe).
- **Parked:** Pause densify pin-holes — accepted strip cosmetics (`product.md`); Later if revisited.

---

## Next up

### Sequence playlist

**Now:** (1) model + `[i]` helpers. Then 2–6. Shared scrub (`TimelineRef`) done.

1. **Model** — clip list + duration offsets + `t ↔ (clip_index, local FrameId)`; unit tests; `SequencePos` / `[i]` format helpers (readout + go-to parse).
2. **`<Sequence>` + 2× `<Video>`** — open blobs/URLs; metadata probe → durations; live + idle assign; one visible stage; `TimelineRef` backend.
3. **Play seam** — EOF mid-list → swap + play next; last → `ended` / loop; idle = next (forward).
4. **Seek / scrub / step** — duration scrubber; map to local; rebind idle; facade transport.
5. **Demo** — multi file + multi URL; position `[i] now / end` (Frames/Timecode); go-to `[i] …` or bare = current clip.
6. **Docs trim** — drop this block when shipped; mark Sequence surface shipped in product/architecture.

**Out of v1:** clip name in readout/go-to; cut markers on bar; gapless audio; app-wide shared `ThumbSink`; compare dual-visible.

### Publish crates (`0.1.0-alpha`)

Layout/packages: `architecture.md` § Layout. Repo: `alexantoshuk/fpmedia-web`. Demo `demo-player` stays `publish = false`.

**DoD:** external Leptos CSR host `cargo add`s `fpmedia-web-components`, opens MP4 via Blob (and CORS-ok URL), play/pause/seek/scrub settle — no our Axum / Trunk.

**Hard gate:** do **not** publish until `re_mp4` is on crates.io with #35+#38 (no fork). Until then: `cargo publish` blocked. Keep git `re_mp4` pin until then.

**Hygiene done (still `publish = false`):** package versions `0.1.0-alpha.1`; path deps carry matching `version =`; README badges point at in-repo guides (not live crates.io/docs.rs).

1. **Deps unblock** — switch workspace `re_mp4` from git pin to crates.io when #35+#38 ship; `cargo package` dry-run.
2. **Ship** — flip `publish`; CONTRIBUTING gates + e2e; lockstep publish order timecode → engine → components; GH Release notes; restore crates.io/docs.rs badges; drop this block from state.

### Online demo (free static)

Implementation in tree (Trunk `demo-player`, `.github/workflows/pages.yml`).

- **Do:** enable Settings → Pages → Source: GitHub Actions; push/merge; confirm site loads WASM + Browse open.
- **Out of scope:** public CORS / media proxy; sample buttons; large masters.
- **Note:** private repo — Pages may need Pro; published URL is often world-readable. Local e2e URL probes: `npm run media:fetch` (testvideo v0.3.0 → `demo-player/public/media/`).

---

## Later

- Optional: harden Pause densify pin-holes on slow PreferSoftware (salvage + skip-bridge `put_chunk`; no Exact wipe) — only if strip cosmetics stop being acceptable (`product.md`).
- Revisit seek Exact follow **without** wiping Exact on settle (densify yield / unbanned walk only — prove scrub still paints).
- Optional: unthrottle play `sync_ui` for truer e2e playhead sampling (host only; not an engine judder fix).
- Classify browser codec faults by DOMException `name` (`EncodingError` / `InvalidStateError` / `DataError`) instead of message text in `MediaError::from_codec_message` — behavior change (per-browser wording).
- Native `FileSource`; no-Range `ReadableStreamSource`; optional `hevc` defaulting.
- Sequence: clip **name** in `[i]` readout / go-to; cut markers on scrubber; gapless / tighter A/V seam; shared ThumbSink across sources.
- Compare dual-visible (reuse 2-slot pool); pure `<Audio>` / `AudioPlayer`; thumb Worker; 4K loop residual; reverse playback (if revisited); production player UI.
