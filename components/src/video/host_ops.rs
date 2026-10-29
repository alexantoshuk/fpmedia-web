//! Host-op enqueue helpers + the async bodies the drain loop runs.

use std::rc::Rc;

use fpmedia_web::OpenPhase;
use leptos::prelude::*;

use super::VideoRef;
use super::ctx::HostCtx;
use super::op_queue::{HostOp, OpenArgs};
use super::open_url::open_source_staged;
use super::transport_apply::sync_playing_from_transport;

pub(super) fn enqueue_seek_step(ctx: &HostCtx, kick_drain: &Rc<dyn Fn()>, op: HostOp) {
    let HostCtx { queue, player, .. } = ctx;
    if player.borrow().as_ref().is_none() {
        return;
    }
    // Transport already left Playing; pause the engine (hard — Exact seek/step).
    if let Some(p) = player.borrow().as_ref().cloned() {
        // Sync Prev/Next/Seek wait for each other, but preempt async scrub.
        if queue.scrub_inflight() {
            p.cancel_ops();
        }
        p.pause();
    }
    if queue.enqueue(op) {
        kick_drain();
    }
}

pub(super) enum SeekOrStep {
    Seek(u32),
    SeekKeyframe(u32),
    ScrubDrag(u32),
    /// Soft-drag stillness Exact — finger still down (no transport Settling).
    ScrubDwellExact(u32),
    ScrubRelease {
        target: u32,
        then_play: bool,
    },
    Step(bool),
}

pub(super) async fn run_open(ctx: &HostCtx, id: u64, OpenArgs { source, canvas }: OpenArgs) {
    let HostCtx {
        queue,
        player,
        fps_meter,
        ..
    } = ctx;
    let VideoRef {
        ready,
        metadata,
        error,
        status,
        fps,
        pipeline,
        has_audio,
        volume,
        muted,
        playback_rate,
        ..
    } = ctx.video;
    web_sys::console::log_1(&"fpmedia-web: open task running".into());
    if !queue.is_current(id) {
        return;
    }
    *player.borrow_mut() = None;
    let opened = open_source_staged(source, canvas, move |phase: OpenPhase| {
        let msg = phase.label();
        web_sys::console::log_1(&format!("fpmedia-web: status {msg}").into());
        status.set(msg);
    })
    .await;
    if !queue.is_current(id) {
        return;
    }
    match opened {
        Ok(p) => {
            let p = Rc::new(p);
            // Before `seek_frame(0)` — its pipeline entry would arm densify.
            p.set_scrub_cache_policy(ctx.scrub_cache_policy.get());
            p.set_thumb_policy(ctx.thumb_policy.get());
            metadata.set(Some(p.metadata()));
            has_audio.set(p.has_audio());
            p.set_playback_rate(playback_rate.get_untracked());
            if p.has_audio() {
                p.set_volume(volume.get_untracked());
                p.set_muted(muted.get_untracked());
            }
            let _ = p.relayout();
            *player.borrow_mut() = Some(Rc::clone(&p));
            status.set(OpenPhase::DecodingStart.label());
            let seek = p.seek_frame(0).await;
            if !queue.is_current(id) {
                return;
            }
            match seek {
                Ok(()) => {
                    ready.set(true);
                    status.set(OpenPhase::Ready.label());
                    fps_meter.borrow_mut().reset();
                    fps.set(None);
                    pipeline.set(Some(p.pipeline_stats(ctx.video.pipeline_interest())));
                    ctx.sync_ui();
                }
                Err(e) if e.is_cancelled() => {
                    status.set(OpenPhase::Cancelled.label());
                }
                Err(e) => {
                    ready.set(false);
                    error.set(Some(e.to_string()));
                    status.set(OpenPhase::SeekStartFailed.label());
                }
            }
        }
        Err(e) => {
            ready.set(false);
            metadata.set(None);
            error.set(Some(e.clone()));
            status.set(OpenPhase::Failed.label());
            web_sys::console::error_1(&e.clone().into());
        }
    }
}

pub(super) async fn run_seek_or_step(ctx: &HostCtx, id: u64, op: SeekOrStep) -> bool {
    let HostCtx { queue, player, .. } = ctx;
    let VideoRef {
        error, playhead, ..
    } = ctx.video;
    if !queue.is_current(id) {
        return false;
    }
    let Some(p) = player.borrow().as_ref().cloned() else {
        return false;
    };
    // Scrub settle / dwell: host already soft-paused on pointerdown and
    // `prepare_scrub_settle` latched densify hold. Calling hard `pause()` cleared
    // that hold and re-kicked densify under exact goto → `decoder busy` /
    // resume Play no-op (URL fMP4).
    let scrub_settle = matches!(
        &op,
        SeekOrStep::ScrubRelease { .. } | SeekOrStep::ScrubDwellExact(_)
    );
    if scrub_settle {
        if p.is_playing() {
            p.pause_for_scrub();
        }
    } else {
        p.pause();
    }
    let label = match &op {
        SeekOrStep::Seek(t) => format!("seek {t}"),
        SeekOrStep::SeekKeyframe(t) => format!("seek-key {t}"),
        SeekOrStep::ScrubDrag(t) => format!("scrub {t}"),
        SeekOrStep::ScrubDwellExact(t) => format!("scrub dwell Exact {t}"),
        SeekOrStep::ScrubRelease {
            target: t,
            then_play,
        } => {
            if *then_play {
                format!("scrub release+play {t}")
            } else {
                format!("scrub release {t}")
            }
        }
        SeekOrStep::Step(true) => "next".into(),
        SeekOrStep::Step(false) => "prev".into(),
    };
    web_sys::console::log_1(&format!("fpmedia-web: {label} start").into());
    let is_step = matches!(op, SeekOrStep::Step(_));
    let is_dwell = matches!(op, SeekOrStep::ScrubDwellExact(_));
    let result = match op {
        SeekOrStep::Seek(target) => p.seek_frame(u64::from(target)).await,
        SeekOrStep::SeekKeyframe(target) => p.seek_keyframe(u64::from(target)).await,
        SeekOrStep::ScrubDrag(target) => p.scrub_to(u64::from(target), false).await,
        // Stillness Exact while finger down — same `scrub_to(settle)` as release,
        // transport stays Scrubbing. **no equivalent** (scrub local).
        SeekOrStep::ScrubDwellExact(target) => {
            let r = p.scrub_to(u64::from(target), true).await;
            if r.is_ok() && queue.is_current(id) && !queue.has_pending_play() {
                p.arm_scrub_densify_after_exact(u64::from(target));
            }
            r
        }
        // Resume Play: no Exact settle — PlayPrime's iterator draws the aim and
        // arms the clock on it (`mediabunny: media-player seekToTime`).
        SeekOrStep::ScrubRelease {
            target,
            then_play: true,
        } => p.prepare_play_from_scrub(u64::from(target)).map(|_| ()),
        SeekOrStep::ScrubRelease {
            target,
            then_play: false,
        } => {
            let r = p.scrub_to(u64::from(target), true).await;
            // Settle no longer arms densify inside the engine — if Play is queued,
            // keep the warm main decoder for prime (play-after-scrub).
            if r.is_ok() && queue.is_current(id) && !queue.has_pending_play() {
                p.arm_scrub_densify_after_exact(u64::from(target));
            }
            r
        }
        SeekOrStep::Step(true) => p.next_frame().await,
        SeekOrStep::Step(false) => p.previous_frame().await,
    };
    if !queue.is_current(id) {
        return false;
    }
    let done = match &result {
        Ok(()) => "ok",
        Err(e) if e.is_cancelled() => "cancel",
        Err(_) => "err",
    };
    web_sys::console::log_1(&format!("fpmedia-web: {label} done {done}").into());
    match &result {
        Ok(()) => {
            // Clear sticky UI error after a successful seek/scrub/step — e.g.
            // FrameOutOfRange or Decoding error left from a prior failed op.
            error.set(None);
        }
        Err(e) if e.is_cancelled() => {}
        Err(e) => error.set(Some(e.to_string())),
    }
    ctx.sync_ui();
    // Snap UI playhead to committed engine after scrub settle/release (and step).
    // Soft-cold can commit a safe indexed frame while the pointer aim stays ahead —
    // leaving eng≠ph makes waitExactSettle spin and PlayPrime `seek_frame` the
    // cold aim (long hang). Step already snapped for hold-repeat.
    // Dwell keeps the finger aim on the tip — do not snap playhead under the pointer.
    if (is_step || (!is_dwell && scrub_settle))
        && let Ok(borrow) = player.try_borrow()
        && let Some(p) = borrow.as_ref()
    {
        let engine = p.state().frame.min(u32::MAX as u64) as u32;
        playhead.set(engine);
    }
    result.is_ok()
}

pub(super) async fn run_play_prime(ctx: &HostCtx, id: u64, resume_raf: &impl Fn()) -> bool {
    let HostCtx {
        queue,
        player,
        fps_meter,
        ..
    } = ctx;
    let VideoRef {
        ready,
        playhead,
        error,
        fps,
        ..
    } = ctx.video;
    if !queue.is_current(id) || !ready.get_untracked() {
        return false;
    }
    let Some(p) = player.borrow().as_ref().cloned() else {
        return false;
    };

    // Sync engine to UI playhead only if still mismatched (e.g. settle cancelled
    // by seek/step, or play without a prior settle). Do this *before* `play()` so
    // we do not cancel an in-flight scrub that was already targeting playhead —
    // that op finished ahead of us in the drain queue.
    //
    // Cold scrub soft-settle may leave UI playhead on an unindexed / uncached
    // aim while engine committed the indexed prefix. Priming that aim blocked
    // OpQueue (`warm_sample_bytes` / mfra) and stuck Play `busy`. Snap to a
    // playable frame first (mediabunny: first samples before play).
    let indexed_last = p
        .indexed_frame_count()
        .saturating_sub(1)
        .min(u64::from(u32::MAX)) as u32;
    let eng = p.state().frame.min(u64::from(u32::MAX)) as u32;
    let mut ph = playhead.get_untracked().min(indexed_last);
    if !p.frame_exact_ready(u64::from(ph)) {
        let safe = eng.min(indexed_last);
        if safe != ph {
            web_sys::console::log_1(
                &format!("fpmedia-web: play snap cold playhead {ph} → {safe}").into(),
            );
            playhead.set(safe);
            ph = safe;
        }
    }
    if p.state().frame != u64::from(ph) {
        web_sys::console::log_1(
            &format!(
                "fpmedia-web: play sync seek {ph} (engine {})",
                p.state().frame
            )
            .into(),
        );
        match p.seek_frame(u64::from(ph)).await {
            Ok(()) => {}
            Err(e) if e.is_cancelled() => return false,
            Err(e) => {
                error.set(Some(e.to_string()));
                return false;
            }
        }
        if !queue.is_current(id) {
            return false;
        }
    }

    p.play();
    fps_meter.borrow_mut().reset();
    fps.set(None);
    // UI `playing` flips only on transport PlayPrimeDone / ScrubReleaseDone
    // (`TransportState::Playing`). Keep it false during prime so rAF cannot
    // End→stop_playback mid-PlayPrime.
    let still_queued = || queue.is_current(id);

    // Unlock AudioContext during the Play click turn — before long prime awaits.
    if let Err(e) = p.resume_audio_context().await {
        web_sys::console::warn_1(&format!("fpmedia-web: audio resume: {e}").into());
    }
    if !still_queued() {
        if p.is_playing() {
            p.pause();
        }
        return false;
    }

    // Soft-park after prime/clock failure: must NOT call `stop_playback()` —
    // that bumps async_id so the drain skips `PlayPrimeFailed` and transport
    // stays deaf in `PlayPriming` (stress: Play did not start after scrub).
    // mediabunny: examples/media-player play() after seekToTime — no priming trap.
    let park_failed_prime = || {
        if p.is_playing() {
            p.pause();
        }
        fps_meter.borrow_mut().reset();
        fps.set(None);
    };

    web_sys::console::log_1(&"fpmedia-web: play prime start".into());
    let prime_ok = match p.prime_playback().await {
        Ok(()) => true,
        Err(e) if e.is_cancelled() => false,
        Err(e) => {
            if still_queued() {
                error.set(Some(e.to_string()));
            }
            false
        }
    };
    web_sys::console::log_1(&format!("fpmedia-web: play prime done ok={prime_ok}").into());

    if !still_queued() {
        if p.is_playing() {
            p.pause();
        }
        return false;
    }
    if !prime_ok {
        park_failed_prime();
        return false;
    }

    let media_origin = p.state().timestamp.seconds;
    // No AAC: defer silent PlaybackClock / wall arm to the first present rAF.
    // Arming here (after long prime on ChG-class masters) lets AudioContext run
    // ahead of the first present → skip-to-latest storm.
    // mediabunny: `play` sets `audioContextStartTime` while `render` already loops.
    // AAC: still warm/cold start here so schedule has headroom before rAF.
    let defer_silent_clock = !p.has_audio();
    let pending_silent = if defer_silent_clock {
        let _ = p.take_audio_warm_resume();
        web_sys::console::log_1(&"fpmedia-web: play clock defer silent→first rAF".into());
        true
    } else {
        // Pause button / scrub soft-held the AudioDecoder — warm restart (no
        // isConfigSupported). Open / EOF / abort-prime stay on cold start_media_clock.
        // mediabunny: examples/media-player play() → buffers(t) after pause.
        let clock = if p.take_audio_warm_resume() {
            web_sys::console::log_1(&"fpmedia-web: play clock warm resume".into());
            p.resume_media_clock(media_origin).await
        } else {
            p.start_media_clock(media_origin).await
        };
        if let Err(e) = clock {
            if still_queued() {
                error.set(Some(e.to_string()));
                park_failed_prime();
            }
            return false;
        }
        if !still_queued() {
            if p.is_playing() {
                p.pause();
            }
            return false;
        }
        false
    };

    // Engine owns prefetch / underrun / loop / silent-clock arm from here.
    p.begin_play_session(pending_silent);
    // Arm UI `playing` now (PlayPriming/Settling+session live) so rAF presents
    // before PlayPrimeDone — otherwise the clock runs with no presents → underrun.
    sync_playing_from_transport(ctx);

    if still_queued() && ctx.video.playing.get_untracked() {
        resume_raf();
    }
    still_queued() && p.play_session_live()
}
