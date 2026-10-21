# Glossary

> Shared vocabulary for fpmedia-web. **English only.** Term definitions live here.
> **Agents:** do **not** preload this file every session — open on demand (unfamiliar term, or add/edit/delete vocabulary). Maintenance: `AGENTS.md` § Glossary hygiene.
> UX decisions → `product.md`. Design / FSM / caches / decode flows → `architecture.md`.
> Traps → `skills/*`.

When a term’s meaning changes in code, **edit or delete** the entry here in the same change (do not leave stale definitions).

---

## Position and frames

### FrameId

Primary timeline position: a **presentation** frame index, 0-based. Not `time × fps`. Seek, step, scrub aim, strip paint, and densify gaps all speak FrameId. Demux may also track decode order; presentation order is what the user means by “frame N”.

### Aim

The FrameId the engine is currently focused on for eviction, gap pick, and Exact settle — usually the playhead under the finger, after seek, or after Pause. Residency windows (`AimWindow`) evict **farthest from aim**.

### Key / sync / keyframe

A sync sample (IDR / equivalent). Exact decode to frame N is a **keyframe cascade**: nearest sync at or before N → feed decode-order packets → emit until presentation N is held. B-frames and reorder windows are part of that cascade for seek / settle / step / Pause — not a tax on continuous Play smoothness.

### GOP

Group of pictures from one sync key through the frames before the next key. **Densify’s unit of work** is one GOP (`fill_one_gap`). `GopProfile` (AllIntra / ShortFixed / Long) is derived from the frame index and gates whether densify runs.

### Key→N

Shorthand for the Exact cascade from the GOP key through the aimed presentation frame (and any needed reorder slack). Used on settle, seek, step Exact, and densify **salvage** for a single missing id.

---

## Timeline strips and residency

Demo colors are host-owned; meanings are product invariants (`product.md` § Play vs scrub residency).

### Fetch (gray)

Bytes that have ever been fetched into the source cache plane. Backed by **FetchMap** (accumulate ever-fetched ranges — not a live LRU mirror of the byte cache). May soft-merge for paint; unrelated to decoded coverage.

### Covered (dim green)

“We have decoded coverage for this FrameId” as **gap geometry** — a **marker** in `ScrubChunkStore`, not necessarily a live bitmap. Used so densify / gap-pick know what is already solid. Soft-drag **preview must not** use Covered alone (no pixels).

### Exact (bright green)

A live densify **ImageBitmap** in `ScrubBlitLru` for that FrameId. Means step-ready near the playhead and is the only plane **soft-drag preview** reads (`scrub_preview_frame`). Pause/seek/settle canvas Exactness is the key→N decode to aim — the strip is the residency map, not a substitute for that decode.

### Marker

A Covered store entry without pixels. Cheap residency for strip geometry. Insert may **refuse** at capacity when the new id would only evict something closer to aim.

### Blit

(1) The Exact bitmap cache (`ScrubBlitLru`), or (2) putting/drawing an `ImageBitmap`. “Wipe Exact” / `blit.clear` on settle destroys soft-drag warm trail — forbidden pattern (`skills/media.md`).

### Pin-hole

A one-frame (or few-frame) gap in Covered and/or Exact after Pause densify — often PreferSoftware **omit** and/or marker **insert refuse**, then densify **soft-ban**. On constrained machines these may remain as **accepted strip cosmetics** (`product.md`); Pause/seek Exact canvas should still be correct. Do not “fix” by wiping Exact.

### Skip-bridge

After a GOP decode that produced some outputs but left marker holes: insert Covered markers for missing GOP ids (without Exact bitmaps) so gap-pick can advance instead of hammering swiss-cheese forever. Local densify plane (no mediabunny Covered store).

---

## Scrub phases and engine modes

Phase table: `architecture.md` § Scrub phases. Exactness policy: `product.md` § Exact vs Play.

### Play

Continuous playback. Prioritizes cadence and A/V sync over painting every presentation frame. **Densify off** (epoch cancel). Pictures come from the **PlayIter** on the main decoder, not the presentation queue.

### PlayIter / nextFrame

Play-only decode iterator on the main decoder (port of mediabunny `mediaSamplesInRange`): feeds decode-order packets from the key and parks while decoded + in-flight ≥ 4. The rAF consumer draws overdue samples and parks the first future one as **nextFrame** in a 2-canvas pool; it is painted when `media_time()` reaches it. Flow: `architecture.md` § Decode flows.

### Pause / idle scrub

Transport paused / ready. Densify **on** when `ScrubPolicy::densify` (Pause button uses densify; soft-drag does not). Fills Covered + Exact around aim until gaps are exhausted or soft-banned.

### Soft-drag

Pointer down/move on the scrubber: update aim + paint preview from Exact blit only. **No densify under the finger** (PreferSoftware fill saturates the main thread). Drag aborts in-flight densify for the gesture.

### Settle

Pointer-up (or equivalent release): host `Settling`, engine `ExactHold`, Exact key→N to the aimed frame. Densify **yields** (defers) so Exact can own the scrub decoder/HW session. **Do not** full-clear Exact blit here.

### Seek

Explicit goto FrameId — same Exact cascade family as settle. Cold/fMP4 paths may warm index + GOP bytes first.

### Step

Next/Prev by one presentation frame. Hits presentation/behind ring or Exact blit when resident; otherwise Exact decode. Should not abort useful densify unnecessarily.

### ExactHold

Engine mode while settle/seek Exact owns decode — densify must not compete (yield / latch pending).

### PlayPrime / ScrubRelease

Leaving scrub toward play (or staying paused): prime index + bytes and start the PlayIter (or seed the paused picture), clear scrub gesture, re-arm densify per mode. Host transport: `ScrubRelease { then_play }`, etc.

### Cold scrub

Scrub/settle while FetchMap + frame index for the aim GOP are not warm yet. Soft settle may commit an indexed aim and defer Exact (`pending_cold_exact`) instead of freezing the tab waiting on bytes.

---

## Densify and decode path

Design: `architecture.md` § Predictive densify. Traps: `skills/media.md`.

### Densify

Background fill of Covered markers + Exact bitmaps around aim on the **scrub** `VideoDecoder` (PreferSoftware), separate from the main play/seek decoder. Predictive: nearest gap to aim first; one GOP per `fill_one_gap`; rAF pacing between GOPs.

### fill_one_gap

One densify iteration for a gap’s GOP: (1) range `decode_gop` → stream ingest, (2) Exact **salvage** for missing blit ids, (3) Covered **skip-bridge** for remaining marker holes (only if the GOP produced some outputs).

### Salvage

For presentation ids still missing Exact after PreferSoftware `decode_gop`: scrub `decode_frame` key→N (mediabunny-shaped **`samplesAtTimestamps`**, vs range **`samples()`** / `decode_gop`).

### Soft-ban

Temporarily skip a GOP key in the densify walk after no progress (Exact complete, cap refuse-all, or idle retries). Prevents infinite PreferSoftware re-decode of the same island. Banned set dies when the densify task ends; a later Pause/pipeline can try again.

### PreferSoftware

WebCodecs `hardwareAcceleration: prefer-software` for scrub and thumb decoders so they do not steal the main HW session. Hint only — under load or weak CPU/I/O, software decode may **omit** frames more often.

### Omit

Decoder accepted packets for a GOP but emitted fewer presentation frames than the index lists (classic `N−1` of `N`). Consumer sees a pin-hole unless salvage / skip-bridge closes it.

### Insert refuse

At marker or blit capacity, refuse a new FrameId when inserting it would only evict a resident **closer to aim**. Protects the warm window; can leave edge/swiss-cheese holes if skip-bridge also uses refusing insert.

### AimWindow

Shared farthest-from-aim residency window under Covered markers and Exact blit. One eviction policy for both planes.

### Dual decoder (+ thumb)

Main decoder = Play / Exact seek-settle (prefer-hardware when supported). Scrub decoder = densify PreferSoftware (must not dual-HW poison the main session — especially HEVC). When feature `thumbs` is on, a **third** PreferSoftware decoder serves hover only — never the densify scrub slot. Overview: `architecture.md` § Decode flows.

### ThumbCache

Residency store for hover-thumb bitmaps (separate from Exact blit / Covered markers). Eviction policy is thumb-specific today (GOP-oriented); aim-distance `AimWindow` for thumbs is open work (`state.md` Later).

### ThumbPreview

Engine hover decode/present path on the thumb decoder: refine the aimed hover FrameId into `ThumbCache`, paint via host `<HoverThumb>`. Isolated from densify; Play avoids sparse key_fill; far refine is throttled. Close enough ≈ same sync key (`hover_thumb_close_enough`).

---

## Elements and host

Surfaces: `product.md`. Host traps: `skills/leptos.md`.

### `<Video>` / VideoRef

Sized stage + element-like API (open, play, pause, seek, rate, volume). Owns canvas present; does not ship player chrome.

### `<Scrubber>`

Headless gesture + track context: pointer → playhead / scrub phases. Host supplies children and CSS (progress, ticks, strip layout).

### `<TimelineMeta>`

One meta strip for a kind (`Fetch` / `Keyframes` / `Covered` / `Exact`): pipeline runs → continuous `f/n` spans → 1px canvas paint (same grid as tip / zebra). Host owns color and DOM placement. `Keyframes` = sync sample ticks from the frame index (not densify residency). Mount acquires a refcounted `PipelineInterest` plane (`Fetch` or `Ready`); debug meters use a separate lease.

### `<HoverThumb>`

Hover filmstrip preview component: canvas **inside** the element; chrome via `children` / `class`. Wired under `<Scrubber>` or manual API. **Play and Pause only — not while scrubbing.** Must not drive densify or buffer strips. Engine: `ThumbPreview` / `ThumbCache`.

### Pipeline (debug)

`video.pipeline` / live decode stats for the demo panel. Scrubber UI must not require reading pipeline to paint strips.

### OpQueue

Host operation queue (coalesced open/seek/scrub/play commands) so UI does not stampede the engine.

---

## Sources and open

Progressive open: `architecture.md` § Progressive open.

### MediaSource / CachedSource

Abstract byte source + WASM-facing cache (quantum fills; do not `array_buffer()` whole files). Open is progressive (`moov` / moofs first).

### FetchMap

Accumulate map of fetched byte ranges → Fetch (gray) strip. Gap-fill is its own SingleFlight — never share densify’s flight.

### ReadOrchestrator

Coordinates ranged reads for `UrlSource` and `BlobSource` (port of mediabunny `ReadOrchestrator`): a few workers each keep one open stream (HTTP Range body / `blob.stream()`) and serve reads from it, with a small window cache. Keeps open and play from stampeding the network or reopening a file per sample.

---

## Audio and clock

Design diagram: `architecture.md` § Audio + media clock. UX rates: `product.md`.

### MediaClock

Engine clock behind `VideoPlayer::media_time()`. Drives Play present/due decisions. Sourced from the audio graph’s `AudioContext` when AAC/PCM is scheduled, or from a **silent PlaybackClock** when there is no usable audible output. Hosts must not substitute wall clock in UI for A/V sync.

### PlaybackClock

Silent / no-audio-track (or muted-under-load) media clock that advances on **wall time**. Needed because Chrome `AudioContext.currentTime` can lag when there is little or no audible output under WebCodecs load (intentional diverge from mediabunny `getPlaybackTime` in that case). AAC with a live graph still prefers the context clock.

### AudioOutput

Engine sink: demuxed audio → schedule into `AudioContext` (volume / mute / rate). Owns the path shared by AAC and PCM; `VideoRef` only exposes volume/mute, not the clock.

### AAC path

`mp4a` via WebCodecs `AudioDecoder` → `AudioData` → `AudioContext`. Unsupported audio sample entries are ignored; video still opens (`product.md`).

### PCM path

Common QT/ISO PCM FourCCs (`in24` / `sowt` / `twos` / …): raw sample bytes from moov sound description (incl. nested `wave`/`enda` endianness) → f32 planar schedule. **No** `AudioDecoder`. Same volume/mute/rate surface as AAC.

### WSOLA / TimeStretcher

Fixed-pitch rate change through **4×** (`audio::TimeStretcher`). Above that (demo shuttle / API up to 8×): wall-clock video advance and muted audio (`product.md`). Local invention — mediabunny has no playback-rate API equivalent.

---

## Related product words (short)

| Term | Meaning |
| ---- | ------- |
| **Exact vs Play** | Frame-accurate = settle / seek / step / Pause canvas; Play prefers smoothness (`product.md`). |
| **Step-ready** | Exact blit (or ring) has the neighbor — Next/Prev should feel instant. |
| **Shuttle** | High playback rate (≥2×); may drop overdue frames; audio muted above fixed-pitch limit. |
| **Mediabunny-first** | Port patterns from Vanilagy/mediabunny before inventing Rust-only decode/sink behaviour (`AGENTS.md`). |
