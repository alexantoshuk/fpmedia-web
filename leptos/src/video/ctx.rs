//! Shared host handles for one mounted [`super::Video`].

use std::cell::RefCell;
use std::rc::Rc;

use fpmedia_web::{TransportEvent, VideoPlayer};
use leptos::prelude::*;

use super::VideoRef;
use super::op_queue::OpQueue;
use super::session::FpsMeter;
use super::transport_apply::{TransportHandle, note_transport, sync_playing_from_transport};

pub(super) type PlayerHandle = Rc<RefCell<Option<Rc<VideoPlayer>>>>;

/// Player + queue + transport + UI signals. Clone is cheap (`Rc`s + `Copy`
/// signals) — clone into each callback instead of cloning fields.
#[derive(Clone)]
pub(super) struct HostCtx {
    pub video: VideoRef,
    pub player: PlayerHandle,
    pub fps_meter: Rc<RefCell<FpsMeter>>,
    pub queue: OpQueue,
    pub transport: TransportHandle,
}

impl HostCtx {
    pub fn new(video: VideoRef, queue: OpQueue, transport: TransportHandle) -> Self {
        Self {
            video,
            player: Rc::new(RefCell::new(None)),
            fps_meter: Rc::new(RefCell::new(FpsMeter::new())),
            queue,
            transport,
        }
    }

    /// Run `f` on the live player unless it is absent or borrowed (re-entry).
    pub fn with_player<T>(&self, f: impl FnOnce(&VideoPlayer) -> T) -> Option<T> {
        let borrow = self.player.try_borrow().ok()?;
        borrow.as_deref().map(f)
    }

    /// Engine → UI signals (frame, count, timestamp, loop, pipeline).
    pub fn sync_ui(&self) {
        let v = self.video;
        let Ok(borrow) = self.player.try_borrow() else {
            return;
        };
        if let Some(p) = borrow.as_ref() {
            let st = p.state();
            let engine = st.frame.min(u32::MAX as u64) as u32;
            let count = st.frame_count.min(u32::MAX as u64) as u32;
            let count_changed = v.frame_count.get_untracked() != count;
            v.frame.set(engine);
            v.frame_count.set(count);
            v.timestamp.set(st.timestamp.seconds);
            // Follow engine while playing, or reset playhead on a new clip.
            // While paused after scrub, leave `playhead` (UI target) alone.
            if v.playing.get_untracked() || count_changed {
                v.playhead.set(engine);
            }
            // fMP4 bg backfill grows the index after warm open — refresh meta.
            if count_changed {
                v.metadata.set(Some(p.metadata()));
            }
            // Do NOT sync `playing` from the engine — transport FSM owns UI
            // `playing` (`TransportState::ui_playing`).
            v.looping.set(st.looping);
            v.pipeline
                .set(Some(p.pipeline_stats(v.pipeline_interest())));
        }
    }

    /// Hard stop: abort in-flight play-prime / scrub-release / prefetch.
    pub fn stop_playback(&self) {
        let v = self.video;
        self.queue.bump();
        self.queue.clear_pending_scrub_and_play();
        let leaving_play = v.playing.get_untracked();
        self.transport.force_ready_unless_closed();
        sync_playing_from_transport(self);
        let p = self.player.borrow().as_ref().cloned();
        if let Some(p) = p {
            p.pause();
        }
        self.fps_meter.borrow_mut().reset();
        v.fps.set(None);
        self.sync_ui();
        // `sync_ui` skips playhead while paused (preserve scrub targets). Leaving
        // play must still align playhead to the committed canvas/engine frame —
        // otherwise Pause→Seek/Prev/Next use a ~100ms-stale playhead.
        // EOF path: `note_transport(Ended)` clears `playing` *before*
        // `stop_playback`, so `leaving_play` is already false — still snap via
        // `ended` (throttled play sync can leave tip/stats 1–2 frames behind
        // the painted last frame).
        if leaving_play || v.ended.get_untracked() {
            v.playhead.set(v.frame.get_untracked());
        }
        // `busy` stays with the op queue (open/seek may still be draining).
    }

    /// Drop the clip: cancel all ops, drop the player, zero UI signals.
    ///
    /// mediabunny media-player `initMediaPlayer`: pause → iterator.return →
    /// asyncId++ before a new Input. Dropping the retiring VideoPlayer lets
    /// UrlSource dispose abort Range workers.
    pub fn retire_clip(&self) {
        let v = self.video;
        note_transport(self, TransportEvent::Closed);
        self.queue.cancel_all();
        // Drop *before* zeroing frame_count — paused rAF sync_ui treats
        // count 0→N as a new clip and would restore the old playhead.
        let retiring = self.player.borrow_mut().take();
        if let Some(p) = retiring {
            // Bump thumb epoch while the sink still exists; Drop clears it.
            p.cancel_hover_thumb();
            p.pause();
            // DOM canvas outlives the player — clear A’s aspect/buffer before B opens.
            p.reset_stage_layout();
        }
        // `retiring` drops here → decoder/caches + UrlSource orch.dispose.
        // `ready=false` also hides HoverThumbPreview (Effect on ready).
        self.fps_meter.borrow_mut().reset();
        v.ready.set(false);
        v.ended.set(false);
        v.error.set(None);
        v.fps.set(None);
        v.pipeline.set(None);
        v.metadata.set(None);
        v.frame.set(0);
        v.playhead.set(0);
        v.frame_count.set(0);
        v.timestamp.set(0.0);
        v.has_audio.set(false);
    }
}
