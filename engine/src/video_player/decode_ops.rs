//! Main-decoder checkout / goto / invalidate for [`super::VideoPlayer`].
//!
//! Extracted with P3 present split (was nested in the present chunk). Navigation
//! only — no behavior change. `mediabunny:` seekToTime / iterator return.

use std::rc::Rc;

use crate::decode::owned_video_frame::OwnedVideoFrame;
use crate::decode::webcodecs::WebCodecsDecoder;
use crate::error::MediaError;
use crate::model::decoder_slot::{DecoderSlotCmd, DecoderSlotEvent, reduce as decoder_slot_reduce};
use crate::types::FrameId;

use super::VideoPlayer;
use super::scrub::ingest_scrub_frame_shared;
use super::shared::{QueuedFrame, STEP_BEHIND_CAP};
use super::{now_ms, sleep_ms};

impl VideoPlayer {
    pub(super) fn apply_decoder_slot(&self, event: DecoderSlotEvent) -> Vec<DecoderSlotCmd> {
        let (next, cmds) = decoder_slot_reduce(self.shared.decoder_slot.get(), event);
        self.shared.decoder_slot.set(next);
        cmds
    }

    pub(super) fn invalidate_decoder(&self) {
        // Soft clear is not enough: clearing `input_timestamps` while the browser
        // still holds in-flight chunks desyncs PTS remap → rare
        // `flush/idle without pts=… (held=0)` on rapid PREV.
        // mediabunny: terminate iterator → close decoder; next range creates fresh.
        let cmds = self.apply_decoder_slot(DecoderSlotEvent::Invalidate);
        for cmd in cmds {
            match cmd {
                DecoderSlotCmd::ResetNow => {
                    // Take out so `reset()` does not hold the RefCell (JS re-entry / Drop).
                    let taken = self.decoder.borrow_mut().take();
                    if let Some(mut decoder) = taken {
                        if decoder.reset().is_ok() {
                            let _ = self.apply_decoder_slot(DecoderSlotEvent::ClearReset);
                        }
                        *self.decoder.borrow_mut() = Some(decoder);
                    }
                }
                DecoderSlotCmd::ApplyReset => {}
            }
        }
    }

    /// Take the decoder out of the slot for the duration of an async decode so
    /// we never hold `RefMut` across `.await` (avoids freezes / panics).
    ///
    /// Waits briefly if another op still holds the decoder (quick Pause→Play:
    /// cancelled prefetch must put the decoder back before prime can run).
    pub(super) async fn decode_frame(
        &self,
        track: &Rc<crate::demux::Mp4VideoTrack>,
        frame: FrameId,
        epoch: u64,
    ) -> Result<OwnedVideoFrame, MediaError> {
        self.decode_frame_inner(track, frame, epoch, None).await
    }

    pub(super) async fn decode_frame_with_interim_cb(
        &self,
        track: &Rc<crate::demux::Mp4VideoTrack>,
        frame: FrameId,
        epoch: u64,
        on_interim: Box<dyn FnMut(FrameId, OwnedVideoFrame)>,
    ) -> Result<OwnedVideoFrame, MediaError> {
        self.decode_frame_inner(track, frame, epoch, Some(on_interim))
            .await
    }

    /// The decoder's packet pump outlives this call's borrow, so the track must
    /// be shared (`Rc`) rather than borrowed.
    pub(super) async fn decode_frame_inner(
        &self,
        track: &Rc<crate::demux::Mp4VideoTrack>,
        frame: FrameId,
        epoch: u64,
        on_interim: Option<Box<dyn FnMut(FrameId, OwnedVideoFrame)>>,
    ) -> Result<OwnedVideoFrame, MediaError> {
        let mut decoder = self.take_decoder(epoch).await?;
        let cancelled = || !self.is_current(epoch);
        let result = match on_interim {
            Some(cb) => {
                decoder
                    .decode_frame_with_interim(track, frame, &cancelled, cb)
                    .await
            }
            None => decoder.decode_frame(track, frame, &cancelled).await,
        };
        let result = match result {
            // One hard reset + retry — orphan PTS after a raced invalidate.
            Err(e) if e.is_retryable_stall() => {
                if cancelled() {
                    Err(MediaError::Cancelled)
                } else {
                    decoder.reset()?;
                    decoder.decode_frame(track, frame, &cancelled).await
                }
            }
            other => other,
        };
        if cancelled() {
            let _ = decoder.reset();
            let _ = self.apply_decoder_slot(DecoderSlotEvent::ClearReset);
            self.put_decoder(decoder);
            if let Ok(video) = result {
                drop(video);
            }
            return Err(MediaError::Cancelled);
        }
        self.put_decoder(decoder);
        result
    }

    pub(super) fn put_decoder(&self, decoder: WebCodecsDecoder) {
        let _ = self.apply_decoder_slot(DecoderSlotEvent::PutBack);
        *self.decoder.borrow_mut() = Some(decoder);
    }

    pub(super) async fn take_decoder(&self, epoch: u64) -> Result<WebCodecsDecoder, MediaError> {
        // Wait for a peer that holds the slot (prefetch / seek / GOP fill). Yield
        // macrotasks — microtask-only spins can return busy before the holder
        // resumes. Cap by wall-clock: Chromium often clamps `setTimeout(0)` to
        // ~4ms, so a fixed 120-spin budget was only ~0.5s and false-fired
        // `decoder busy` under play load (Firefox was fine).
        // mediabunny: no shared slot — iterators use `terminated`; we cancel via
        // `op_epoch` and must keep waiting while the epoch is current
        // (`skills/leptos.md` Quick Pause→Play).
        let wait_ms = self
            .shared
            .track
            .metadata_ref()
            .settle_policy()
            .take_decoder_wait_ms;
        let busy_log_ms = self
            .shared
            .track
            .metadata_ref()
            .settle_policy()
            .densify_yield_ms;
        let start = now_ms().unwrap_or(0.0);
        let mut waited_logged = false;
        loop {
            if !self.is_current(epoch) {
                return Err(MediaError::Cancelled);
            }
            let taken = self.decoder.borrow_mut().take();
            if let Some(mut decoder) = taken {
                let waited = now_ms().unwrap_or(start) - start;
                if waited >= 100.0 {
                    web_sys::console::warn_1(
                        &format!("fpmedia-web: take_decoder waited {waited:.0}ms").into(),
                    );
                }
                let cmds = self.apply_decoder_slot(DecoderSlotEvent::Checkout);
                for cmd in cmds {
                    if matches!(cmd, DecoderSlotCmd::ApplyReset) {
                        decoder.reset()?;
                        let _ = self.apply_decoder_slot(DecoderSlotEvent::ClearReset);
                    }
                }
                return Ok(decoder);
            }
            let now = now_ms().unwrap_or(start);
            if !waited_logged && now - start >= busy_log_ms {
                waited_logged = true;
                web_sys::console::warn_1(
                    &format!(
                        "fpmedia-web: take_decoder still busy (≥{busy_log_ms:.0}ms) — peer holds slot"
                    )
                    .into(),
                );
            }
            if now - start >= wait_ms {
                break;
            }
            sleep_ms(0).await;
        }
        Err(MediaError::WebCodecs(
            "decoder busy (Pause/Play raced — try Play again)".into(),
        ))
    }

    pub(super) async fn goto_frame(&self, frame: FrameId, epoch: u64) -> Result<(), MediaError> {
        let track = self.shared.track.clone();
        let video_frame = self.decode_frame(&track, frame, epoch).await?;
        if !self.is_current(epoch) {
            drop(video_frame);
            return Err(MediaError::Cancelled);
        }
        self.present_and_cache(frame, video_frame)?;
        self.shared.commit_frame(frame)?;
        Ok(())
    }

    /// Exact seek that salvages pruned early frames into densify markers (no canvas blit).
    ///
    /// Canvas stays on the caller's static best cached/key preview until exact.
    /// One key→N pass warms the GOP so post-exact densify is cheap.
    pub(super) async fn goto_frame_with_interim(
        &self,
        frame: FrameId,
        epoch: u64,
    ) -> Result<(), MediaError> {
        let track = self.shared.track.clone();
        let shared = Rc::clone(&self.shared);
        let on_interim: Box<dyn FnMut(FrameId, OwnedVideoFrame)> =
            Box::new(move |id: FrameId, video: OwnedVideoFrame| {
                ingest_scrub_frame_shared(&shared, id, video, false);
            });
        let video_frame = self
            .decode_frame_with_interim_cb(&track, frame, epoch, on_interim)
            .await?;
        if !self.is_current(epoch) {
            drop(video_frame);
            return Err(MediaError::Cancelled);
        }
        self.present_and_cache(frame, video_frame)?;
        self.shared.commit_frame(frame)?;
        Ok(())
    }

    /// Exact settle/seek: one recreate+retry on densify/settle poison
    /// (`Decoding error` / closed codec) after abort races.
    pub(super) async fn exact_goto_with_decoder_retry(
        &self,
        frame: FrameId,
        epoch: u64,
    ) -> Result<(), MediaError> {
        match self.goto_frame_with_interim(frame, epoch).await {
            Ok(()) => Ok(()),
            Err(e) if e.is_cancelled() => Err(e),
            Err(e) if e.is_decoder_poison() && self.is_current(epoch) => {
                web_sys::console::warn_1(
                    &format!("fpmedia-web: Exact retry after decoder poison ({e})").into(),
                );
                self.invalidate_decoder();
                self.goto_frame_with_interim(frame, epoch).await
            }
            Err(e) => Err(e),
        }
    }

    /// Prev miss: key→target cascade; park near-target pruned VF into the behind ring.
    ///
    /// `mediabunny: mediaSamplesAtTimestamps` restarts from key on rewind — we keep
    /// a capped behind window so the next few Prev hits avoid another cascade.
    pub(super) async fn goto_frame_fill_behind(
        &self,
        frame: FrameId,
        epoch: u64,
    ) -> Result<(), MediaError> {
        let track = self.shared.track.clone();
        let shared = Rc::clone(&self.shared);
        let target = frame;
        let on_interim: Box<dyn FnMut(FrameId, OwnedVideoFrame)> =
            Box::new(move |id: FrameId, video: OwnedVideoFrame| {
                let floor = target.saturating_sub(STEP_BEHIND_CAP as FrameId);
                if id >= floor && id < target {
                    let mut behind = shared.step_behind.borrow_mut();
                    behind.retain(|x| x.frame != id);
                    match behind.iter().position(|x| x.frame > id) {
                        Some(i) => behind.insert(i, QueuedFrame { frame: id, video }),
                        None => behind.push_back(QueuedFrame { frame: id, video }),
                    }
                    while behind.len() > STEP_BEHIND_CAP {
                        if let Some(old) = behind.pop_front() {
                            drop(old.video);
                        }
                    }
                } else {
                    ingest_scrub_frame_shared(&shared, id, video, false);
                }
            });
        let video_frame = self
            .decode_frame_with_interim_cb(&track, frame, epoch, on_interim)
            .await?;
        if !self.is_current(epoch) {
            drop(video_frame);
            return Err(MediaError::Cancelled);
        }
        self.present_and_cache(frame, video_frame)?;
        self.shared.commit_frame(frame)?;
        Ok(())
    }
}
