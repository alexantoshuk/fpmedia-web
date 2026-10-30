//! [`VideoPlaylistRef`] — host handle for a mounted [`super::VideoPlaylist`].

use std::rc::Rc;

use leptos::prelude::*;
use web_sys::HtmlCanvasElement;

use crate::VideoRef;
use crate::video_playlist::{PlaylistModel, PlaylistPos};

use super::slots::{ensure_clip_on_idle, preload_idle, try_promote_idle};
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
    /// Scrub domain: milliseconds from playlist start ([`PlaylistModel::total_duration_ms`]).
    pub playhead_ms: RwSignal<u32>,
    pub duration_ms: RwSignal<u32>,
    pub live_clip: RwSignal<u32>,
    pub idle_clip: RwSignal<Option<u32>>,
    /// When true, [`Self::slot_a`] is the live (visible) stage.
    pub live_is_a: RwSignal<bool>,
    /// Host scrub gesture in flight — mirror Effect must not overwrite `playhead_ms`.
    pub scrubbing: RwSignal<bool>,
    pub slot_a: VideoRef,
    pub slot_b: VideoRef,
    pub(crate) ops: StoredValue<Option<VideoPlaylistOps>, LocalStorage>,
    pub(crate) sources: StoredValue<Vec<PlaylistSource>, LocalStorage>,
    pub(crate) open_gen: StoredValue<u64, LocalStorage>,
}

#[derive(Clone)]
pub(crate) struct VideoPlaylistOps {
    pub open_blobs: Rc<dyn Fn(Vec<web_sys::Blob>)>,
    pub open_urls: Rc<dyn Fn(Vec<String>)>,
    pub play: Rc<dyn Fn()>,
    pub pause: Rc<dyn Fn()>,
    pub close: Rc<dyn Fn()>,
    pub seek_ms: Rc<dyn Fn(u32)>,
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
            playhead_ms: RwSignal::new(0),
            duration_ms: RwSignal::new(0),
            live_clip: RwSignal::new(0),
            idle_clip: RwSignal::new(None),
            live_is_a: RwSignal::new(true),
            scrubbing: RwSignal::new(false),
            slot_a: VideoRef::new(),
            slot_b: VideoRef::new(),
            ops: StoredValue::new_local(None),
            sources: StoredValue::new_local(Vec::new()),
            open_gen: StoredValue::new_local(0),
        }
    }

    pub fn set_loop(self, on: bool) {
        self.looping.set(on);
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

    pub fn seek_ms(self, ms: u32) {
        self.ended.set(false);
        if let Some(seek) = self.op(|o| &o.seek_ms) {
            seek(ms);
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

    /// Timeline scrub units (ms). Promotes idle when it already holds the aim clip;
    /// settle (`Up`) async-seeks if the clip still needs open/rebind.
    pub(crate) fn scrub_timeline(self, ms: u32, phase: ScrubPhase) {
        self.ended.set(false);
        match phase {
            ScrubPhase::Down | ScrubPhase::Move => self.scrubbing.set(true),
            ScrubPhase::Up => self.scrubbing.set(false),
        }
        self.playhead_ms.set(ms);
        let Some(aim) = self.model.get_untracked().at_ms(ms) else {
            return;
        };
        self.pos.set(aim);

        let mut promoted = false;
        if aim.clip_index != self.live_clip.get_untracked() {
            // Leave Scrubbing on the outgoing live so idle is not stuck soft-paused.
            if matches!(phase, ScrubPhase::Down | ScrubPhase::Move) {
                self.live_video().pause();
            }
            promoted = try_promote_idle(self, aim.clip_index);
            // Aim clip not on idle (e.g. drag back across cut while idle holds next):
            // warm idle once; later Moves promote when ready. Do not remount Scrubber.
            if !promoted
                && matches!(phase, ScrubPhase::Down | ScrubPhase::Move)
                && self.idle_clip.get_untracked() != Some(aim.clip_index)
            {
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
                let neighbor = self.idle_clip.get_untracked();
                let epoch = self.open_gen.get_value();
                spawn_local(async move {
                    preload_idle(self, neighbor, epoch).await;
                });
            }
            return;
        }

        // Aim clip not live yet — settle via seek (open/swap).
        if matches!(phase, ScrubPhase::Up) {
            self.seek_pos(aim);
        }
    }

    pub(crate) fn paint_hover_thumb(
        self,
        ms: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) {
        let Some(pos) = self.model.get_untracked().at_ms(ms) else {
            return;
        };
        let video = self.video_for_clip(pos.clip_index);
        video.paint_hover_thumb(pos.local_frame, dest, max_w, max_h);
    }

    pub(crate) fn try_paint_hover_thumb(
        self,
        ms: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> bool {
        let Some(pos) = self.model.get_untracked().at_ms(ms) else {
            return false;
        };
        let video = self.video_for_clip(pos.clip_index);
        video.try_paint_hover_thumb(pos.local_frame, dest, max_w, max_h)
    }

    pub(crate) fn cancel_hover_thumb(self) {
        self.slot_a.cancel_hover_thumb();
        self.slot_b.cancel_hover_thumb();
    }

    fn video_for_clip(self, clip_index: u32) -> VideoRef {
        if self.live_clip.get_untracked() == clip_index {
            return self.live_video();
        }
        if self.idle_clip.get_untracked() == Some(clip_index) {
            return self.idle_video();
        }
        // Fall back to live until idle is rebound to that clip.
        self.live_video()
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
