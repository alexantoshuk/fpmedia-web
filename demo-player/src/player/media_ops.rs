use fpmedia_web_components::{MediaRef, VideoPlaylistRef, VideoRef};
use leptos::prelude::*;

/// Which media host the demo mounts (`<Show>` — one at a time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DemoSession {
    Single,
    Playlist,
}

impl DemoSession {
    pub(super) fn is_playlist(self) -> bool {
        matches!(self, Self::Playlist)
    }
}

/// Active [`MediaRef`] for the current demo session (reactive on `session`).
pub(super) fn demo_media(
    video: VideoRef,
    playlist: VideoPlaylistRef,
    session: RwSignal<DemoSession>,
) -> MediaRef {
    if session.get().is_playlist() {
        MediaRef::from_playlist(playlist)
    } else {
        MediaRef::from_video(video)
    }
}

pub(super) fn demo_media_untracked(
    video: VideoRef,
    playlist: VideoPlaylistRef,
    session: RwSignal<DemoSession>,
) -> MediaRef {
    if session.get_untracked().is_playlist() {
        MediaRef::from_playlist(playlist)
    } else {
        MediaRef::from_video(video)
    }
}

/// Source fps of the current clip (playlist model or single-clip metadata).
///
/// Playlist: subscribe to [`VideoPlaylistRef::live_clip`] — not `pos` (pos
/// mirrors every sync tick and would thrash rate chrome / main thread).
pub(super) fn demo_source_fps(media: MediaRef) -> Option<f64> {
    if let Some(playlist) = media.playlist() {
        let idx = playlist.live_clip.get() as usize;
        playlist
            .model
            .with(|m| m.clip(idx).and_then(|c| c.fps))
            .or_else(|| playlist.live_video().metadata.get().and_then(|m| m.fps))
    } else {
        media.active_video().metadata.get().and_then(|m| m.fps)
    }
}

pub(super) fn demo_toggle_play(media: MediaRef, playing: bool) {
    if playing {
        media.pause();
    } else {
        media.play();
    }
}

/// Step −1 (bar / overflow / ←). Stop-Play + ignore LoopZone live on MediaRef.
pub(super) fn demo_previous_frame(media: MediaRef) {
    media.previous_frame();
}

/// Step +1 (bar / overflow / →). Stop-Play + ignore LoopZone live on MediaRef.
pub(super) fn demo_next_frame(media: MediaRef) {
    media.next_frame();
}

pub(super) fn demo_toggle_loop(media: MediaRef) {
    media.set_loop(!media.looping.get_untracked());
}

/// LoopZone in = playhead; same spot again → reset in to 0. Arms loop if off (`[` / menu).
pub(super) fn demo_mark_loop_in(media: MediaRef) {
    if !media.looping.get_untracked() {
        media.set_loop(true);
    }
    let ph = media.playhead.get_untracked();
    let zone = media.loop_zone();
    if zone.in_frame == ph {
        media.set_loop_zone(0, zone.out_frame);
    } else {
        media.set_loop_zone(ph, zone.out_frame.max(ph.saturating_add(1)));
    }
}

/// LoopZone out = playhead + 1; same spot again → reset out to full. Arms loop if off (`]` / menu).
pub(super) fn demo_mark_loop_out(media: MediaRef) {
    if !media.looping.get_untracked() {
        media.set_loop(true);
    }
    let ph = media.playhead.get_untracked();
    let zone = media.loop_zone();
    let want_out = ph.saturating_add(1);
    let count = media.frame_count.get_untracked();
    if zone.out_frame == want_out {
        media.set_loop_zone(zone.in_frame, count.max(zone.in_frame.saturating_add(1)));
    } else {
        let inn = zone.in_frame.min(want_out.saturating_sub(1));
        media.set_loop_zone(inn, want_out.max(inn.saturating_add(1)));
    }
}

/// LoopZone → full axis (`[0, frame_count)`). Does not change loop on/off (settings menu).
pub(super) fn demo_reset_loop_zone(media: MediaRef) {
    let count = media.frame_count.get_untracked();
    media.set_loop_zone(0, count);
}
