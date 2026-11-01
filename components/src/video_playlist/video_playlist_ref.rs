//! [`VideoPlaylistRef`] — host handle for a mounted [`super::VideoPlaylist`].

use std::rc::Rc;

use leptos::prelude::*;
use web_sys::HtmlCanvasElement;

use crate::VideoRef;
use crate::video_playlist::{PlaylistModel, PlaylistPos};

use super::slots::{ensure_clip_on_idle, idle_neighbor, preload_idle, try_promote_idle};
use super::source::PlaylistSource;
use wasm_bindgen_futures::spawn_local;

/// Cloneable handle to a mounted [`super::VideoPlaylist`] (two [`VideoRef`] slots).
#[derive(Clone, Copy)]
pub struct VideoPlaylistRef {
    pub ready: RwSignal<bool>,
    pub busy: RwSignal<bool>,
    pub status: RwSignal<String>,
    pub error: RwSignal<Option<String>>,
    /// Playlist-level end (last clip EOF, loop off) — not a single-clip `VideoRef::ended`.
    pub ended: RwSignal<bool>,
    /// Loop the whole playlist (clips themselves stay non-looping for seam).
    pub looping: RwSignal<bool>,
    pub model: RwSignal<PlaylistModel>,
    pub pos: RwSignal<PlaylistPos>,
    /// Scrub domain: global presentation frame ([`PlaylistModel::total_frame_count`]).
    pub playhead: RwSignal<u32>,
    pub frame_count: RwSignal<u32>,
    pub live_clip: RwSignal<u32>,
    pub idle_clip: RwSignal<Option<u32>>,
    /// When true, [`Self::slot_a`] is the live (visible) stage.
    pub live_is_a: RwSignal<bool>,
    /// Host scrub gesture in flight — mirror Effect must not overwrite `playhead`.
    pub scrubbing: RwSignal<bool>,
    /// Playlist-wide volume (both slots). Facade — not per-clip.
    pub volume: RwSignal<f64>,
    /// Playlist-wide mute (both slots). Idle may stay muted while `SeamHold` arms.
    pub muted: RwSignal<bool>,
    /// Resume Play after scrub that crossed clips (per-slot `then_play` does not survive promote).
    pub(crate) scrub_resume: RwSignal<bool>,
    pub slot_a: VideoRef,
    pub slot_b: VideoRef,
    pub(crate) ops: StoredValue<Option<VideoPlaylistOps>, LocalStorage>,
    pub(crate) sources: StoredValue<Vec<PlaylistSource>, LocalStorage>,
    pub(crate) open_gen: StoredValue<u64, LocalStorage>,
    /// Clip index already kicked for near-EOF origin warm this lap (`None` = none).
    pub(crate) seam_warm_for: StoredValue<Option<u32>, LocalStorage>,
    /// Idle clip with engine `SeamHold` armed (ready for dual-present handoff).
    pub(crate) seam_hold_for: StoredValue<Option<u32>, LocalStorage>,
    /// `arm_seam_hold` in flight (avoid double spawn).
    pub(crate) seam_arming: StoredValue<bool, LocalStorage>,
    /// Handoff task in flight.
    pub(crate) seam_handoff_inflight: StoredValue<bool, LocalStorage>,
}

#[derive(Clone)]
pub(crate) struct VideoPlaylistOps {
    pub open_blobs: Rc<dyn Fn(Vec<web_sys::Blob>)>,
    pub open_urls: Rc<dyn Fn(Vec<String>)>,
    pub play: Rc<dyn Fn()>,
    pub pause: Rc<dyn Fn()>,
    pub close: Rc<dyn Fn()>,
    pub seek_frame: Rc<dyn Fn(u32)>,
    pub seek_pos: Rc<dyn Fn(PlaylistPos)>,
    pub next_frame: Rc<dyn Fn()>,
    pub previous_frame: Rc<dyn Fn()>,
}

impl VideoPlaylistRef {
    pub fn new() -> Self {
        Self {
            ready: RwSignal::new(false),
            busy: RwSignal::new(false),
            status: RwSignal::new(String::from("Load a playlist to begin.")),
            error: RwSignal::new(None),
            ended: RwSignal::new(false),
            looping: RwSignal::new(false),
            model: RwSignal::new(PlaylistModel::default()),
            pos: RwSignal::new(PlaylistPos::new(0, 0)),
            playhead: RwSignal::new(0),
            frame_count: RwSignal::new(0),
            live_clip: RwSignal::new(0),
            idle_clip: RwSignal::new(None),
            live_is_a: RwSignal::new(true),
            scrubbing: RwSignal::new(false),
            // HTMLMediaElement default volume is 1.0.
            volume: RwSignal::new(1.0),
            muted: RwSignal::new(false),
            scrub_resume: RwSignal::new(false),
            slot_a: VideoRef::new(),
            slot_b: VideoRef::new(),
            ops: StoredValue::new_local(None),
            sources: StoredValue::new_local(Vec::new()),
            open_gen: StoredValue::new_local(0),
            seam_warm_for: StoredValue::new_local(None),
            seam_hold_for: StoredValue::new_local(None),
            seam_arming: StoredValue::new_local(false),
            seam_handoff_inflight: StoredValue::new_local(false),
        }
    }

    pub fn set_loop(self, on: bool) {
        self.looping.set(on);
        // Loop on while live is last clip: idle must become wrap target 0 so
        // near-EOF SeamHold can arm (do not wait for the next EOF window).
        if on {
            let model = self.model.get_untracked();
            let live = self.live_clip.get_untracked();
            if let Some(want) = super::slots::idle_neighbor(&model, live, true)
                && self.idle_clip.get_untracked() != Some(want)
            {
                let epoch = self.open_gen.get_value();
                spawn_local(async move {
                    preload_idle(self, Some(want), epoch).await;
                });
            }
        }
    }

    /// Playlist-wide volume → both slots (live + idle).
    pub fn set_volume(self, volume: f64) {
        let v = volume.clamp(0.0, 1.0);
        self.volume.set(v);
        self.slot_a.set_volume(v);
        self.slot_b.set_volume(v);
    }

    /// Playlist-wide mute → both slots (idle forced mute while seam-arming / held).
    pub fn set_muted(self, muted: bool) {
        self.muted.set(muted);
        self.apply_mute_to_slots();
    }

    /// Re-push volume + mute onto both `<Video>` slots (after open / swap / handoff).
    pub(crate) fn apply_gain_to_slots(self) {
        let v = self.volume.get_untracked();
        self.slot_a.set_volume(v);
        self.slot_b.set_volume(v);
        self.apply_mute_to_slots();
    }

    fn apply_mute_to_slots(self) {
        let m = self.muted.get_untracked();
        let live = self.live_video();
        let idle = self.idle_video();
        live.set_muted(m);
        // Silent SeamHold prime must not unmute idle when playlist mute is off.
        let idle_seam_mute = self.seam_arming.get_value()
            || self.seam_hold_for.get_value().is_some()
            || idle.seam_hold_armed();
        idle.set_muted(m || idle_seam_mute);
    }

    pub fn live_video(self) -> VideoRef {
        if self.live_is_a.get_untracked() {
            self.slot_a
        } else {
            self.slot_b
        }
    }

    pub fn idle_video(self) -> VideoRef {
        if self.live_is_a.get_untracked() {
            self.slot_b
        } else {
            self.slot_a
        }
    }

    fn op<F: ?Sized>(&self, pick: impl FnOnce(&VideoPlaylistOps) -> &Rc<F>) -> Option<Rc<F>> {
        self.ops
            .try_with_value(|ops| ops.as_ref().map(|o| Rc::clone(pick(o))))
            .flatten()
    }

    pub fn open_blobs(self, blobs: Vec<web_sys::Blob>) {
        if let Some(open) = self.op(|o| &o.open_blobs) {
            open(blobs);
        } else {
            self.status
                .set("VideoPlaylist not mounted — hard-refresh the page.".into());
            self.error.set(Some("VideoPlaylist not mounted".into()));
        }
    }

    pub fn open_urls(self, urls: Vec<String>) {
        if let Some(open) = self.op(|o| &o.open_urls) {
            open(urls);
        } else {
            self.status
                .set("VideoPlaylist not mounted — hard-refresh the page.".into());
            self.error.set(Some("VideoPlaylist not mounted".into()));
        }
    }

    pub fn play(self) {
        self.ended.set(false);
        if let Some(play) = self.op(|o| &o.play) {
            play();
        }
    }

    pub fn pause(self) {
        if let Some(pause) = self.op(|o| &o.pause) {
            pause();
        }
    }

    pub fn close(self) {
        if let Some(close) = self.op(|o| &o.close) {
            close();
        }
    }

    /// Seek to a global presentation frame on the playlist scrub axis.
    pub fn seek_frame(self, global_frame: u32) {
        self.ended.set(false);
        if let Some(seek) = self.op(|o| &o.seek_frame) {
            seek(global_frame);
        }
    }

    pub fn seek_pos(self, pos: PlaylistPos) {
        self.ended.set(false);
        if let Some(seek) = self.op(|o| &o.seek_pos) {
            seek(pos);
        }
    }

    pub fn next_frame(self) {
        if let Some(next) = self.op(|o| &o.next_frame) {
            next();
        }
    }

    pub fn previous_frame(self) {
        if let Some(prev) = self.op(|o| &o.previous_frame) {
            prev();
        }
    }

    /// Timeline scrub units (global frames). Promotes idle when it already holds
    /// the aim clip; settle (`Up`) async-seeks if the clip still needs open/rebind.
    pub(crate) fn scrub_timeline(self, global_frame: u32, phase: ScrubPhase) {
        self.ended.set(false);
        match phase {
            ScrubPhase::Down => {
                self.scrubbing.set(true);
                // Before soft-pause / cross-clip hard-pause clears UI `playing`.
                // Per-slot `then_play` does not survive promote — playlist owns resume.
                self.scrub_resume
                    .set(self.live_video().playing.get_untracked());
            }
            ScrubPhase::Move => self.scrubbing.set(true),
            ScrubPhase::Up => self.scrubbing.set(false),
        }
        self.playhead.set(global_frame);
        let Some(aim) = self.model.get_untracked().at_global_frame(global_frame) else {
            return;
        };
        self.pos.set(aim);

        let mut promoted = false;
        if aim.clip_index != self.live_clip.get_untracked() {
            // Cross-clip: hard-pause outgoing (idle must not stay soft-held).
            // New slot PointerDown is from Ready → `then_play` false; use `scrub_resume`.
            if matches!(phase, ScrubPhase::Down | ScrubPhase::Move) {
                self.live_video().pause();
            }
            promoted = try_promote_idle(self, aim.clip_index);
            if promoted {
                // Idle now holds former live — warm the true neighbor (not a lying label).
                let want = idle_neighbor(
                    &self.model.get_untracked(),
                    aim.clip_index,
                    self.looping.get_untracked(),
                );
                let epoch = self.open_gen.get_value();
                spawn_local(async move {
                    preload_idle(self, want, epoch).await;
                });
            } else if matches!(phase, ScrubPhase::Down | ScrubPhase::Move)
                && self.idle_clip.get_untracked() != Some(aim.clip_index)
            {
                // Aim not on idle yet — warm then later Moves promote when ready.
                let epoch = self.open_gen.get_value();
                let want = aim.clip_index;
                spawn_local(async move {
                    let _ = ensure_clip_on_idle(self, want, epoch).await;
                });
            }
        }

        if aim.clip_index == self.live_clip.get_untracked() {
            let live = self.live_video();
            match phase {
                ScrubPhase::Down => live.scrub_pointer_down(aim.local_frame),
                // After promote the new live never saw PointerDown — Move alone is a no-op.
                ScrubPhase::Move if promoted => live.scrub_pointer_down(aim.local_frame),
                ScrubPhase::Move => live.scrub_pointer_move(aim.local_frame),
                ScrubPhase::Up => live.scrub_pointer_up(aim.local_frame),
            }
            if matches!(phase, ScrubPhase::Up) {
                let resume = self.scrub_resume.get_untracked();
                self.scrub_resume.set(false);
                if resume {
                    live.play();
                }
                let neighbor = idle_neighbor(
                    &self.model.get_untracked(),
                    self.live_clip.get_untracked(),
                    self.looping.get_untracked(),
                );
                let epoch = self.open_gen.get_value();
                spawn_local(async move {
                    preload_idle(self, neighbor, epoch).await;
                });
            }
            return;
        }

        // Aim clip not live yet — settle via seek (open/swap).
        if matches!(phase, ScrubPhase::Up) {
            let resume = self.scrub_resume.get_untracked();
            self.scrub_resume.set(false);
            if resume {
                let epoch = self.open_gen.get_value();
                spawn_local(async move {
                    if let Err(e) = super::slots::seek_to_pos(self, aim, epoch, true).await {
                        if self.open_gen.get_value() != epoch {
                            return;
                        }
                        self.error.set(Some(e));
                        self.status.set("Seek failed.".into());
                    }
                });
            } else {
                self.seek_pos(aim);
            }
        }
    }

    pub(crate) fn paint_hover_thumb(
        self,
        global_frame: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) {
        let Some(pos) = self.model.get_untracked().at_global_frame(global_frame) else {
            return;
        };
        let Some(video) = self.video_for_clip(pos.clip_index) else {
            self.warm_idle_for_clip(pos.clip_index);
            return;
        };
        video.paint_hover_thumb(pos.local_frame, dest, max_w, max_h);
    }

    pub(crate) fn try_paint_hover_thumb(
        self,
        global_frame: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> bool {
        let Some(pos) = self.model.get_untracked().at_global_frame(global_frame) else {
            return false;
        };
        let Some(video) = self.video_for_clip(pos.clip_index) else {
            self.warm_idle_for_clip(pos.clip_index);
            return false;
        };
        video.try_paint_hover_thumb(pos.local_frame, dest, max_w, max_h)
    }

    pub(crate) fn cancel_hover_thumb(self) {
        self.slot_a.cancel_hover_thumb();
        self.slot_b.cancel_hover_thumb();
    }

    /// Slot that owns `clip_index`, if already open on live or idle.
    fn video_for_clip(self, clip_index: u32) -> Option<VideoRef> {
        if self.live_clip.get_untracked() == clip_index {
            return Some(self.live_video());
        }
        if self.idle_clip.get_untracked() == Some(clip_index) {
            return Some(self.idle_video());
        }
        None
    }

    /// Hover/scrub over a clip not on either slot — open it on idle (same as cut drag).
    fn warm_idle_for_clip(self, clip_index: u32) {
        if self.idle_clip.get_untracked() == Some(clip_index) {
            return;
        }
        let epoch = self.open_gen.get_value();
        spawn_local(async move {
            let _ = ensure_clip_on_idle(self, clip_index, epoch).await;
        });
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ScrubPhase {
    Down,
    Move,
    Up,
}

impl Default for VideoPlaylistRef {
    fn default() -> Self {
        Self::new()
    }
}
