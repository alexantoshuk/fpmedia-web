//! Packet pump + async wait/drain helpers around [`DecoderCore`].
//!
//! These are the awaiting half of the decoder: every helper takes
//! `&Rc<RefCell<DecoderCore>>` and re-borrows per turn, so no borrow is ever
//! held across `.await` (`.ai/skills/media.md`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsValue;
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::demux::Mp4VideoTrack;
use crate::error::{DecodeStall, MediaError};
use crate::types::{EncodedSample, FrameId};

use super::core::{DecoderCore, GopSink, REORDER_FEED_SLACK, compute_max_queue_size};

/// Pump safety valve: `decodeQueueSize == 0` means no `ondequeue` can ever fire,
/// so only a consumer take / prune can shrink `held`. With the dual-async split a
/// take is expected, but the consumer may be waiting for a frame that is still
/// unfed (deep B-pyramid) — feed anyway after this many idle turns instead of
/// deadlocking.
const PUMP_STARVED_SPINS: u32 = 240;

/// Cross-task flags between the packet pump and the frame consumer
/// (mediabunny keeps these as closure-scoped `promiseWithResolvers` pairs).
#[derive(Default)]
pub(super) struct PumpSignals {
    /// Consumer finished (frame, cancel, or error) — pump must stop feeding.
    pub(super) stop: Cell<bool>,
    /// Pump has no more packets to feed (GOP end / reorder slack / error).
    pub(super) done: Cell<bool>,
    /// Incremented per fed packet so the consumer can tell "slow" from "stuck".
    pub(super) beats: Cell<u64>,
    pub(super) error: RefCell<Option<MediaError>>,
}

/// Playhead the live packet pump aims past (updated on each consumer take).
///
/// mediabunny `mediaSamplesInRange` keeps one feeder while the iterator pulls;
/// we retarget this cursor instead of bumping `pump_epoch` on every thin-ahead
/// take (ChG B-frame: respawn thrash → ~20 paint fps).
#[derive(Default)]
pub(super) struct PumpPlayCursor {
    pub(super) target_us: Cell<i64>,
    pub(super) target_do: Cell<usize>,
}

/// Active feeder handle — `epoch` must still match [`super::WebCodecsDecoder`]'s
/// `pump_epoch` or the task is a zombie exiting after a hard bump.
pub(super) struct LivePump {
    pub(super) signals: Rc<PumpSignals>,
    pub(super) epoch: u64,
}

/// Stops the pump on every consumer exit path (`?`, cancel, success).
pub(super) struct PumpStopGuard(pub(super) Rc<PumpSignals>);

impl Drop for PumpStopGuard {
    fn drop(&mut self) {
        self.0.stop.set(true);
    }
}

/// Packet pump for [`super::WebCodecsDecoder::decode_frame`] (mediabunny:
/// `src/media-sink.ts` — the `(async () => { … })()` that keeps decoding packets
/// while the iterator consumer pulls samples).
pub(super) struct FramePump {
    pub(super) core: Rc<RefCell<DecoderCore>>,
    pub(super) pump_epoch: Rc<Cell<u64>>,
    pub(super) epoch: u64,
    pub(super) track: Rc<Mp4VideoTrack>,
    pub(super) key_do: usize,
    /// Shared play cursor — reread each turn so consumer takes retarget without
    /// respawning (`PumpPlayCursor`).
    pub(super) cursor: Rc<PumpPlayCursor>,
    pub(super) signals: Rc<PumpSignals>,
    /// Consumer already took the aim PTS — only refill ahead (do not wait for
    /// target in `held`, or we feed to EOF and pin thousands of frames).
    pub(super) warm_only: bool,
}

impl FramePump {
    pub(super) async fn run(self) {
        let stopped = || self.signals.stop.get() || self.pump_epoch.get() != self.epoch;
        // Feeding a little past the target (B/P reorder) is intentional; do not
        // drain the entire GOP — that amplified PTS desync on rapid PREV.
        // Cross-GOP warm: keep going until `warm_ahead` presentation frames
        // sit ahead of the target (Play seam). mediabunny mediaSamplesInRange
        // does not stop at the next IDR.
        let max_feeds = (self.track.frame_count() as usize)
            .saturating_add(32)
            .max(64);
        let mut feeds = 0usize;
        // `warm_only`: aim was already taken at spawn — do not wait for that PTS.
        // When Play retargets the live cursor to the next frame, clear this so we
        // feed until the *new* aim appears (B-frame: ahead of old aim can park
        // before do=N for the next PTS is fed → flush/idle held≈N-2).
        let mut saw_target = self.warm_only;
        let spawn_aim_us = self.cursor.target_us.get();
        let mut saw_for_us = spawn_aim_us;
        let warm_ahead = self.core.borrow().warm_ahead;
        loop {
            if stopped() {
                break;
            }
            let target_us = self.cursor.target_us.get();
            let target_do = self.cursor.target_do.get();
            if target_us != saw_for_us {
                // Live retarget (ChG continuous feeder) — require the new aim in
                // `held` before parking again. mediabunny range keeps one loop
                // but still emits every pulled timestamp.
                saw_target = false;
                saw_for_us = target_us;
            }
            let (fed, ahead, has_target) = {
                let mut c = self.core.borrow_mut();
                // Consumer ranges (!warm_only): prune at spawn aim so a live
                // cursor jump cannot wipe the waiting PTS (Chrome held=225
                // near=83333 aim=41666). Warm refill uses the live cursor.
                let prune_us = if self.warm_only {
                    target_us
                } else {
                    spawn_aim_us
                };
                let _ = c.collect_and_prune_held(prune_us);
                let fed = c.fed_through_order.unwrap_or(self.key_do);
                let ahead = c.held.iter().filter(|h| h.pts_us > target_us).count();
                let has_target = c
                    .held
                    .iter()
                    .any(|h| h.pts_us == target_us || (h.pts_us - target_us).abs() <= 1);
                (fed, ahead, has_target)
            };
            if has_target {
                saw_target = true;
            }
            // After the target has appeared in `held` (or was taken), keep
            // `warm_ahead` frames past it — may cross the next GOP.
            // Requiring `has_target` forever made the pump run to EOF once the
            // consumer took the frame (1080p held≈3200). Stopping before
            // `saw_target` would break seek (B-frames with pts>target first).
            //
            // When ahead is full: **park** (do not exit). Exiting forced
            // `ensure_warm_pump` respawns and broke mediabunny's continuous
            // `mediaSamplesInRange` feeder on long-GOP B-frame play (~20fps).
            // ChG Play lag depends on this park — do not revert to `break`.
            if saw_target && ahead >= warm_ahead {
                self.core.borrow().wake_not_empty_waiters();
                // Wake on consumer `take_held` (dequeue), not only outputs.
                wait_for_capacity_progress(&self.core).await;
                continue;
            }
            if saw_target && fed >= target_do.saturating_add(REORDER_FEED_SLACK + warm_ahead) {
                wait_for_capacity_progress(&self.core).await;
                continue;
            }
            // Aim PTS missing while later frames already sit in `held` and the
            // decoder is idle — further feeds only run to EOF (short B-frame
            // Resolve ~227 → `held=225` + `flush/idle` on pts=41666). B-frames
            // that emit pts>target first still have the aim stamp pending, so
            // they are not idle.
            // mediabunny: seekToTime → iterator.return() before a new range.
            let hole = pts_aim_hole(&self.core.borrow(), target_us, target_do);
            if hole && !self.warm_only {
                break;
            }
            // Warm feeder after live retarget: spawn aim was taken (not a hole);
            // a *new* aim that is missing amid later PTS must not EOF-fill.
            if hole && self.warm_only && target_us != spawn_aim_us {
                break;
            }
            if feeds >= max_feeds {
                break;
            }
            let next = match self.track.samples_after_decode(fed, 1).await {
                Ok(next) => next,
                Err(e) => {
                    *self.signals.error.borrow_mut() = Some(e);
                    break;
                }
            };
            let Some(sample) = next.into_iter().next() else {
                break; // End of track — consumer flushes.
            };
            if stopped() {
                break;
            }
            // Prune before/during backpressure so a long key→target cascade (PREV)
            // does not pin dozens of VideoFrames and wedge decodeQueueSize.
            let prune_us = if self.warm_only {
                target_us
            } else {
                spawn_aim_us
            };
            match await_decode_capacity(&self.core, Some(prune_us), None, Some(&stopped), true)
                .await
            {
                Ok(()) => {}
                Err(MediaError::Cancelled) => break,
                Err(e) => {
                    *self.signals.error.borrow_mut() = Some(e);
                    break;
                }
            }
            if stopped() {
                break;
            }
            let is_key = sample.is_keyframe;
            let sample_do = sample.decode_order;
            let fed = self.core.borrow_mut().decode_encoded_sample(&sample, false);
            if let Err(e) = fed {
                *self.signals.error.borrow_mut() = Some(e);
                break;
            }
            feeds = feeds.saturating_add(1);
            if is_key {
                self.core.borrow_mut().key_order = Some(sample_do);
            }
            self.signals
                .beats
                .set(self.signals.beats.get().wrapping_add(1));
            // Give WebCodecs a turn to emit (B-frame reorder) before the next
            // feed — same as the pre-dual-async pump loop.
            if let Err(e) = await_outputs_if_pending(&self.core).await {
                *self.signals.error.borrow_mut() = Some(e);
                break;
            }
        }
        self.signals.done.set(true);
        // Consumer may be parked on `not_empty` — let it run the flush path.
        self.core.borrow().wake_not_empty_waiters();
    }
}

/// Observable decode progress — used to tell a slow pump from a wedged one.
pub(super) fn progress_signature(
    core: &Rc<RefCell<DecoderCore>>,
    signals: &Rc<PumpSignals>,
) -> (Option<usize>, usize, usize, u32, usize, u64) {
    let c = core.borrow();
    (
        c.fed_through_order,
        c.input_timestamps.len(),
        c.held.len(),
        c.decode_queue_size(),
        c.frames.borrow().len(),
        signals.beats.get(),
    )
}

pub(super) fn holds_target(core: &Rc<RefCell<DecoderCore>>, target_us: i64) -> bool {
    core.borrow()
        .held
        .iter()
        .any(|h| h.pts_us == target_us || (h.pts_us - target_us).abs() <= 1)
}

/// Aim PTS is absent from `held`, but later PTS are parked and nothing is
/// in flight — the aim was consumed/dropped; feeding ahead cannot recreate it.
///
/// B-frames often emit pts>aim **before** the aim's decode-order packet is fed
/// (`do=1` may be pts 166666 while aim is `do=3` / 41666). That is **not** a
/// hole while `fed < target_do` — keep feeding. Once `fed >= target_do` and we
/// are idle without the aim PTS, the sample was skipped/dropped; further feeds
/// only EOF-fill (Resolve short clip → `held≈N-2`). `fed=None` + later trail =
/// stale wipe after continuous warm.
/// mediabunny: `seekToTime` → `videoFrameIterator.return()` before a new range.
pub(super) fn pts_aim_hole(core: &DecoderCore, target_us: i64, target_do: usize) -> bool {
    if core
        .held
        .iter()
        .any(|h| h.pts_us == target_us || (h.pts_us - target_us).abs() <= 1)
    {
        return false;
    }
    if !core.held.iter().any(|h| h.pts_us > target_us) {
        return false;
    }
    let idle = core.decode_queue_size() == 0
        && !core.raw_outputs_pending()
        && core.input_timestamps.is_empty();
    if !idle {
        return false;
    }
    match core.fed_through_order {
        None => true,
        Some(fed) => fed >= target_do,
    }
}

/// True when `new_key_do` is the sync that immediately follows `prev_key_do`
/// (Play crossing a GOP boundary). Scrub jumps to a non-adjacent key are false.
pub(super) fn is_sequential_next_key(
    track: &Mp4VideoTrack,
    prev_key_do: usize,
    new_key_do: usize,
) -> bool {
    track
        .index()
        .is_sequential_next_sync_do(prev_key_do, new_key_do)
}

/// Drain outputs for up to `turns` macrotasks; `true` once the target is held.
pub(super) async fn poll_for_target(
    core: &Rc<RefCell<DecoderCore>>,
    target_us: i64,
    turns: u32,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<bool, MediaError> {
    for _ in 0..turns {
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        await_outputs_if_pending(core).await?;
        let idle = {
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if c.held
                .iter()
                .any(|h| h.pts_us == target_us || (h.pts_us - target_us).abs() <= 1)
            {
                return Ok(true);
            }
            // Only stop early when nothing is in-flight — empty queue alone can
            // race the output callback (All-Intra then fell into 2s flush).
            c.decode_queue_size() == 0 && !c.raw_outputs_pending() && c.input_timestamps.is_empty()
        };
        if idle {
            break;
        }
    }
    Ok(false)
}

/// End-of-input / reorder-slack: drain outputs, then flush once.
///
/// Chrome HW often still has `decodeQueueSize > 0` when the packet pump hits
/// GOP end (long-GOP frame 49: `fed=49, q=5, stamps=9`). A short fixed spin then
/// `flush()` raced to timeout and reported `flush/idle` while work remained.
/// mediabunny: `await decoder.flush()` after packets exhaust — wait for real
/// drain/emit first (`src/media-sink.ts` mediaSamplesInRange).
pub(super) async fn finish_gop_for_target(
    core: &Rc<RefCell<DecoderCore>>,
    target_us: i64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<OwnedVideoFrame>, MediaError> {
    /// Brief drain for in-flight decodeQ at GOP end. Was 400ms — blocked
    /// prefetch long enough for presentation (cap 6 ≈250ms) to underrun →
    /// ~0.5s Play freeze every GOP on 1080p. mediabunny awaits emit without
    /// a long pre-flush sleep (`src/media-sink.ts` mediaSamplesInRange).
    const DRAIN_BEFORE_FLUSH_MS: f64 = 80.0;
    /// Extra wait after a timed-out flush while q/stamps remain.
    const DRAIN_AFTER_FLUSH_MS: f64 = 800.0;

    if let Some(ready) =
        drain_for_target(core, target_us, DRAIN_BEFORE_FLUSH_MS, is_cancelled).await?
    {
        return Ok(Some(ready));
    }

    let pending = {
        let c = core.borrow();
        !c.input_timestamps.is_empty() || c.decode_queue_size() > 0
    };
    if pending {
        // mediabunny awaits flush to completion; Chromium HW may need a few
        // seconds for the last GOP frames — but do **not** spin 5s *before*
        // flush (that starved play vs the audio clock on long-GOP: skipped≈242).
        let flushed = flush_with_timeout_ms(core, 3_000).await?;
        {
            let mut c = core.borrow_mut();
            c.check_error()?;
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                c.fed_through_order = None;
                c.key_order = None;
                c.input_timestamps.clear();
                return Ok(Some(ready));
            }
        }
        if !flushed {
            // Timer won while q still full — keep draining; do not clear warm yet.
            if let Some(ready) =
                drain_for_target(core, target_us, DRAIN_AFTER_FLUSH_MS, is_cancelled).await?
            {
                let mut c = core.borrow_mut();
                c.fed_through_order = None;
                c.key_order = None;
                c.input_timestamps.clear();
                return Ok(Some(ready));
            }
        }
        {
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                c.fed_through_order = None;
                c.key_order = None;
                c.input_timestamps.clear();
                return Ok(Some(ready));
            }
            // Chromium requires a key chunk after flush — drop warm state even on
            // miss so the next cascade recreates/feeds a key.
            if flushed || c.decode_queue_size() == 0 {
                c.fed_through_order = None;
                c.key_order = None;
                c.input_timestamps.clear();
                c.packets_fed = 0;
                c.rasl_skipped = false;
            }
        }
    }
    Ok(None)
}

/// Wait up to `budget_ms` for `target_us` to appear in `held`, or for the
/// decoder to go fully idle.
async fn drain_for_target(
    core: &Rc<RefCell<DecoderCore>>,
    target_us: i64,
    budget_ms: f64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<OwnedVideoFrame>, MediaError> {
    let deadline = now_ms() + budget_ms;
    loop {
        if is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        await_outputs_if_pending(core).await?;
        {
            let mut c = core.borrow_mut();
            c.collect_and_prune_held(target_us)?;
            if let Some(ready) = c.take_held(target_us) {
                return Ok(Some(ready));
            }
            let idle = c.decode_queue_size() == 0
                && !c.raw_outputs_pending()
                && c.input_timestamps.is_empty();
            if idle || now_ms() >= deadline {
                break;
            }
        }
        wait_for_frame_progress(core).await;
    }
    let mut c = core.borrow_mut();
    c.collect_and_prune_held(target_us)?;
    Ok(c.take_held(target_us))
}

pub(super) async fn feed_sample(
    core: &Rc<RefCell<DecoderCore>>,
    sample: &EncodedSample,
    force_key: bool,
    prune_before_us: i64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), MediaError> {
    // Prune before/during backpressure so a long key→target cascade (PREV)
    // does not pin dozens of VideoFrames and wedge decodeQueueSize.
    await_decode_capacity(core, Some(prune_before_us), None, Some(is_cancelled), false).await?;
    if is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    core.borrow_mut().decode_encoded_sample(sample, force_key)
}

/// Like [`feed_sample`] but drains remapped frames to `on_frame` during
/// backpressure waits so `held` stays empty (GOP densify / scrub fill).
pub(super) async fn feed_sample_gop(
    core: &Rc<RefCell<DecoderCore>>,
    sample: &EncodedSample,
    force_key: bool,
    pts_to_id: &mut HashMap<i64, FrameId>,
    on_frame: &mut dyn FnMut(FrameId, OwnedVideoFrame),
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), MediaError> {
    await_decode_capacity(
        core,
        None,
        Some((pts_to_id, on_frame)),
        Some(is_cancelled),
        false,
    )
    .await?;
    if is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    core.borrow_mut().decode_encoded_sample(sample, force_key)
}

pub(super) async fn flush_with_timeout(
    core: &Rc<RefCell<DecoderCore>>,
) -> Result<bool, MediaError> {
    flush_with_timeout_ms(core, 2_000).await
}

/// `Ok(false)` = the race timer won (wedged Chromium decoder); callers keep
/// draining instead of hard-failing. Real `flush()` rejections still error.
pub(super) async fn flush_with_timeout_ms(
    core: &Rc<RefCell<DecoderCore>>,
    ms: i32,
) -> Result<bool, MediaError> {
    // Build the race promise under a short borrow — never await while borrowed.
    let race = core.borrow().flush_race_promise(ms);
    match JsFuture::from(race).await {
        Ok(v) if v.as_string().as_deref() == Some("timeout") => Ok(false),
        Ok(_) => Ok(true),
        Err(e) => Err(MediaError::from(e)),
    }
}

/// Backpressure before `decode()` — mediabunny `media-sink.ts`:
/// `sampleQueue.length + decodeQueueSize > computeMaxQueueSize(sampleQueue.length)`
/// then `await queueDequeue` (resolved on each output **or** consumer take).
///
/// `drop_before_us`: close held frames strictly before the seek target so
/// `held` does not stay non-empty forever during a long key→N cascade (that
/// kept max=8 and stalled the browser queue — rapid PREV timeout).
///
/// `gop`: when densifying a full GOP, drain remapped frames to the consumer
/// each spin so `held` stays empty (consumer take → max queue 40, not 8).
///
/// `wait_for_consumer`: set by the dual-async packet pump, which has a real
/// concurrent consumer. Single-task callers (key cascade, `decode_gop`) must
/// keep the `decodeQueueSize == 0` escape — with no other task able to take
/// from `held`, waiting there is a guaranteed deadlock
/// (`backpressure timed out (q=0, held=N)` on 1080p B-frames).
///
/// Do **not** treat already-buffered `frames` alone as capacity progress
/// (that spun to timeout while `decodeQueueSize` never dropped).
async fn await_decode_capacity(
    core: &Rc<RefCell<DecoderCore>>,
    drop_before_us: Option<i64>,
    mut gop: Option<GopSink<'_>>,
    is_cancelled: Option<&dyn Fn() -> bool>,
    wait_for_consumer: bool,
) -> Result<(), MediaError> {
    let mut spins = 0u32;
    let mut starved = 0u32;
    loop {
        if is_cancelled.is_some_and(|f| f()) {
            return Err(MediaError::Cancelled);
        }
        let (has_capacity, q, held) = {
            let mut c = core.borrow_mut();
            c.check_error()?;
            // Remap + prune before measuring queue pressure.
            if let Some(us) = drop_before_us {
                c.collect_and_prune_held(us)?;
            } else {
                c.collect_outputs_into_held()?;
            }
            if let Some((pts_to_id, on_frame)) = gop.as_mut() {
                c.flush_held_into_gop(*on_frame, pts_to_id);
            }
            let decoded = c.held.len() as u32;
            let max = compute_max_queue_size(decoded);
            let q = c.decode_queue_size();
            // mediabunny: sampleQueue.length + decodeQueueSize > maxQueueSize
            (decoded + q <= max, q, decoded)
        };
        if has_capacity {
            break;
        }
        if q == 0 {
            if !wait_for_consumer {
                // No concurrent taker: only take/prune/gop-drain can shrink
                // `held`, and both already ran above.
                break;
            }
            // Dual-async helps only when the consumer can actually take soon
            // (target already remapped into `held`). Otherwise waiting is the
            // old single-thread deadlock — feed ahead like mediabunny does when
            // the iterator has not yet pulled the needed sample.
            let target_ready =
                drop_before_us.is_some_and(|us| core.borrow().held.iter().any(|h| h.pts_us == us));
            if !target_ready {
                break;
            }
            // Target is buffered — yield so the consumer `take_held` can run and
            // `onQueueDequeue` us. Bail only if it never does (deep stall).
            starved += 1;
            if starved > PUMP_STARVED_SPINS {
                break;
            }
            // Nudge the consumer in case it is parked on `not_empty`.
            core.borrow().wake_not_empty_waiters();
        } else {
            starved = 0;
        }
        spins += 1;
        if spins > 600 {
            return Err(MediaError::DecodeStall {
                kind: DecodeStall::Backpressure,
                detail: format!("decodeQueueSize backpressure timed out (q={q}, held={held})"),
            });
        }
        wait_for_capacity_progress(core).await;
    }
    core.borrow().check_error()
}

/// After a feed: give WebCodecs a macrotask to emit; never park forever.
pub(super) async fn await_outputs_if_pending(
    core: &Rc<RefCell<DecoderCore>>,
) -> Result<(), MediaError> {
    {
        let c = core.borrow();
        c.check_error()?;
        if c.raw_outputs_pending() || c.input_timestamps.is_empty() || c.decode_queue_size() == 0 {
            return Ok(());
        }
    }
    poll_output_or_macrotask(core).await;
    core.borrow().check_error()
}

/// mediabunny: await progress until an output arrives (or a short timer).
async fn poll_output_or_macrotask(core: &Rc<RefCell<DecoderCore>>) {
    let (frames, output_waiters) = {
        let c = core.borrow();
        (Rc::clone(&c.frames), Rc::clone(&c.output_waiters))
    };
    let output = js_sys::Promise::new(&mut |resolve, _reject| {
        if !frames.borrow().is_empty() {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        }
        output_waiters.borrow_mut().push(resolve);
    });
    // Macrotask — HW decode often will not finish in a microtask-only spin
    // (that path forced `flush` on long-GOP frame 0 and broke Play).
    let tick = sleep_promise_ms(0);
    let _ = JsFuture::from(promise_race(output, tick)).await;
}

/// Wait for `decodeQueueSize` to drop: `ondequeue`, new `output`, or macrotask.
/// Do **not** resolve early when `frames` is already non-empty — that is not
/// queue progress and caused false backpressure timeouts (mediabunny waits on
/// real `onQueueDequeue` from a new sample **or** a consumer take).
async fn wait_for_capacity_progress(core: &Rc<RefCell<DecoderCore>>) {
    let (output_waiters, dequeue_waiters) = {
        let c = core.borrow();
        (Rc::clone(&c.output_waiters), Rc::clone(&c.dequeue_waiters))
    };
    let output = js_sys::Promise::new(&mut |resolve, _reject| {
        output_waiters.borrow_mut().push(resolve);
    });
    let dequeue = js_sys::Promise::new(&mut |resolve, _reject| {
        dequeue_waiters.borrow_mut().push(resolve);
    });
    let tick = sleep_promise_ms(0);
    let _ = JsFuture::from(js_sys::Promise::race(&js_sys::Array::of3(
        &output, &dequeue, &tick,
    )))
    .await;
}

/// Consumer side of the dual async: wake on a new remapped `held` frame
/// (mediabunny `queueNotEmpty`), a raw decoder output, or a macrotask.
pub(super) async fn wait_for_frame_progress(core: &Rc<RefCell<DecoderCore>>) {
    let (frames, output_waiters, not_empty_waiters) = {
        let c = core.borrow();
        (
            Rc::clone(&c.frames),
            Rc::clone(&c.output_waiters),
            Rc::clone(&c.not_empty_waiters),
        )
    };
    let output = js_sys::Promise::new(&mut |resolve, _reject| {
        if !frames.borrow().is_empty() {
            let _ = resolve.call0(&JsValue::NULL);
            return;
        }
        output_waiters.borrow_mut().push(resolve);
    });
    let not_empty = js_sys::Promise::new(&mut |resolve, _reject| {
        not_empty_waiters.borrow_mut().push(resolve);
    });
    let tick = sleep_promise_ms(0);
    let _ = JsFuture::from(js_sys::Promise::race(&js_sys::Array::of3(
        &output, &not_empty, &tick,
    )))
    .await;
}

pub(super) fn wake_waiters_deferred(waiting: Vec<js_sys::Function>) {
    if waiting.is_empty() {
        return;
    }
    // Leave the WebCodecs Closure stack before waking Rust waiters (reentrancy).
    // `spawn_local` is reliable; raw `queueMicrotask(promiseResolve)` can fail to
    // run and leave `open` / decode stuck with `busy=true` and no error.
    for resolve in waiting {
        spawn_local(async move {
            let _ = resolve.call0(&JsValue::NULL);
        });
    }
}

pub(super) fn ticks_to_micros(ticks: i64, timescale: u64) -> i64 {
    let timescale = timescale.max(1) as i128;
    ((ticks as i128) * 1_000_000 / timescale) as i64
}

/// Monotonic ms, or `0.0` when `performance` is unavailable (stall check then
/// falls back to `CONSUMER_MAX_SPINS`).
pub(super) fn now_ms() -> f64 {
    crate::timer::now_ms().unwrap_or(0.0)
}

/// Macrotask turn via `setTimeout`. Required when waiting for WebCodecs to
/// drain — microtasks alone never let it run.
///
/// Resolves with `"timeout"` so `flush_with_timeout_ms` can tell a real flush
/// from the race timer — an `undefined` resolve made every wedged flush look
/// successful (`decodeQueueSize` still full afterwards).
pub(super) fn sleep_promise_ms(ms: i32) -> js_sys::Promise {
    crate::timer::timeout_promise(ms, || JsValue::from_str("timeout"))
}

pub(super) fn promise_race(a: js_sys::Promise, b: js_sys::Promise) -> js_sys::Promise {
    js_sys::Promise::race(&js_sys::Array::of2(&a, &b))
}

pub(super) fn is_chromium() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let Ok(nav) = js_sys::Reflect::get(&window, &"navigator".into()) else {
        return false;
    };
    let Ok(ua) = js_sys::Reflect::get(&nav, &"userAgent".into()) else {
        return false;
    };
    let Some(nav) = ua.as_string() else {
        return false;
    };
    // mediabunny `isChromium`: Chrome / Edge / Chromium UA.
    nav.contains("Chrome") || nav.contains("Chromium") || nav.contains("Edg/")
}
