//! Shared scrub / hover / strip timeline handle — [`crate::Video`] or [`crate::VideoPlaylist`].
//!
//! Gesture helpers ([`crate::Scrubber`], [`crate::HoverThumb`], [`crate::TimelineMeta`])
//! take [`TimelineRef`] as the scrub **domain**. Pipeline strips follow
//! [`Self::strip_video`] (playlist → live slot); Exact/Keyframes/Fetch remap here;
//! Covered reads VT ledger (sole). Meta does not special-case playlist.
//! Design: `.ai/architecture.md` § Playlist composition / Virtual Timeline; UX: `.ai/product.md`.

use web_sys::HtmlCanvasElement;

use leptos::prelude::*;

use crate::ReadyFrameKind;
use crate::ReadyFrameMark;
use crate::VideoPlaylistRef;
use crate::VideoRef;
use crate::video_playlist::PlaylistPos;
use crate::video_playlist::video_playlist_ref::ScrubPhase;
use crate::virtual_timeline::{LedgerHandle, note_covered_from_video_ref};

/// Scrub-domain handle: `playhead` / `frame_count` are discrete timeline units
/// ([`VideoRef`]: presentation frames; [`VideoPlaylistRef`]: Σ presentation frames).
#[derive(Clone, Copy)]
pub struct TimelineRef {
    pub ready: RwSignal<bool>,
    pub playhead: RwSignal<u32>,
    pub frame_count: RwSignal<u32>,
    inner: TimelineInner,
}

#[derive(Clone, Copy)]
#[allow(clippy::large_enum_variant)] // Copy handle; VideoPlaylistRef carries two VideoRefs by design
enum TimelineInner {
    Video(VideoRef),
    Playlist(VideoPlaylistRef),
}

/// Identity of [`TimelineRef::strip_video`] for pipeline interest re-lease.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StripVideoKey {
    /// Single-clip timeline.
    Video,
    /// Playlist live slot (`true` = slot A).
    PlaylistLive { live_is_a: bool },
}

impl TimelineRef {
    /// View onto a mounted [`VideoRef`] (same signals + scrub/hover ops).
    pub fn from_video(video: VideoRef) -> Self {
        Self {
            ready: video.ready,
            playhead: video.playhead,
            frame_count: video.frame_count,
            inner: TimelineInner::Video(video),
        }
    }

    /// View onto a mounted [`VideoPlaylistRef`] — units are Σ presentation frames.
    pub fn from_playlist(playlist: VideoPlaylistRef) -> Self {
        Self {
            ready: playlist.ready,
            playhead: playlist.playhead,
            frame_count: playlist.frame_count,
            inner: TimelineInner::Playlist(playlist),
        }
    }

    /// Underlying single-clip video when this timeline is a [`VideoRef`] view.
    pub fn video(self) -> Option<VideoRef> {
        match self.inner {
            TimelineInner::Video(v) => Some(v),
            TimelineInner::Playlist(_) => None,
        }
    }

    /// Underlying playlist when this timeline is a [`VideoPlaylistRef`] view (Σ frames).
    pub fn playlist(self) -> Option<VideoPlaylistRef> {
        match self.inner {
            TimelineInner::Playlist(p) => Some(p),
            TimelineInner::Video(_) => None,
        }
    }

    /// Video whose `pipeline` feeds timeline strips / meta.
    ///
    /// Tracks playlist `live_is_a` when composed. Hosts must **not** remount
    /// `<Scrubber>` when this flips — Meta re-leases via [`Self::strip_video_key`].
    pub fn strip_video(self) -> Option<VideoRef> {
        match self.inner {
            TimelineInner::Video(v) => Some(v),
            TimelineInner::Playlist(p) => Some(if p.live_is_a.get() {
                p.slot_a
            } else {
                p.slot_b
            }),
        }
    }

    pub(crate) fn strip_video_key(self) -> Option<StripVideoKey> {
        match self.inner {
            TimelineInner::Video(_) => Some(StripVideoKey::Video),
            TimelineInner::Playlist(p) => Some(StripVideoKey::PlaylistLive {
                live_is_a: p.live_is_a.get(),
            }),
        }
    }

    /// Domain length for strip `f/n` (same as tip / gesture `frame_count`).
    pub fn strip_domain_len(self) -> f64 {
        self.frame_count.get().max(1) as f64
    }

    /// VT Covered ledger (Single or Playlist session).
    pub fn exact_ledger(self) -> Option<LedgerHandle> {
        match self.inner {
            TimelineInner::Video(v) => v.exact_ledger(),
            TimelineInner::Playlist(p) => p.exact_ledger(),
        }
    }

    /// Bumps when VT Covered changes (strip subscribe).
    pub fn exact_residency_epoch(self) -> RwSignal<u64> {
        match self.inner {
            TimelineInner::Video(v) => v.exact_residency_epoch,
            TimelineInner::Playlist(p) => p.exact_residency_epoch,
        }
    }

    /// Mirror live (+ idle) slot Covered → VT global markers. **no equivalent**.
    pub fn mirror_covered_from_slots(self) {
        let Some(h) = self.exact_ledger() else {
            return;
        };
        let changed = match self.inner {
            TimelineInner::Video(v) => {
                let mut led = h.borrow_mut();
                note_covered_from_video_ref(&mut led, &v, |local| Some(u64::from(local)))
            }
            TimelineInner::Playlist(p) => {
                let model = p.model.get_untracked();
                let mut led = h.borrow_mut();
                let mut changed = false;
                let live_clip = p.live_clip.get_untracked();
                let live = if p.live_is_a.get_untracked() {
                    p.slot_a
                } else {
                    p.slot_b
                };
                changed |= note_covered_from_video_ref(&mut led, &live, |local| {
                    model
                        .global_frame_at(PlaylistPos::new(live_clip, local))
                        .map(u64::from)
                });
                if let Some(idle_idx) = p.idle_clip.get_untracked() {
                    let idle = if p.live_is_a.get_untracked() {
                        p.slot_b
                    } else {
                        p.slot_a
                    };
                    changed |= note_covered_from_video_ref(&mut led, &idle, |local| {
                        model
                            .global_frame_at(PlaylistPos::new(idle_idx, local))
                            .map(u64::from)
                    });
                }
                changed
            }
        };
        if changed {
            match self.inner {
                TimelineInner::Video(v) => v.bump_exact_residency(),
                TimelineInner::Playlist(p) => p.bump_exact_residency(),
            }
        }
    }

    /// Covered strip marks from VT ledger (global FrameIds).
    pub fn strip_covered_marks(self) -> Vec<ReadyFrameMark> {
        let Some(h) = self.exact_ledger() else {
            return Vec::new();
        };
        h.borrow()
            .covered_ready_marks()
            .into_iter()
            .map(|(lo, hi)| ReadyFrameMark::run(lo, hi, ReadyFrameKind::Covered))
            .collect()
    }

    /// Live pipeline ready marks → timeline units (identity for single-clip;
    /// playlist remaps local→global). **Exact + Keyframes** from slots; Covered = VT
    /// ([`Self::strip_covered_marks`]). Idle Exact remapped so Exact spans clips.
    /// **no equivalent**.
    pub fn strip_ready_marks(self, marks: &[ReadyFrameMark]) -> Vec<ReadyFrameMark> {
        match self.inner {
            TimelineInner::Video(_) => marks
                .iter()
                .copied()
                .filter(|m| m.kind != ReadyFrameKind::Covered)
                .collect(),
            TimelineInner::Playlist(p) => {
                let live_clip = p.live_clip.get();
                let model = p.model.get();
                let remap = |clip: u32, marks: &[ReadyFrameMark]| -> Vec<ReadyFrameMark> {
                    marks
                        .iter()
                        .filter(|m| m.kind != ReadyFrameKind::Covered)
                        .filter_map(|m| {
                            let lo_f = u32::try_from(m.frame).ok()?;
                            let hi_f = u32::try_from(m.end).ok()?;
                            let lo =
                                u64::from(model.global_frame_at(PlaylistPos::new(clip, lo_f))?);
                            let hi =
                                u64::from(model.global_frame_at(PlaylistPos::new(clip, hi_f))?);
                            Some(ReadyFrameMark::run(lo, hi, m.kind))
                        })
                        .collect()
                };
                let mut out = remap(live_clip, marks);
                if let Some(idle_idx) = p.idle_clip.get() {
                    let idle = if p.live_is_a.get() {
                        p.slot_b
                    } else {
                        p.slot_a
                    };
                    // Subscribe idle pipeline so Exact grows on the neighbor clip.
                    let _ = idle.pipeline.get();
                    if let Some(pipe) = idle.pipeline.get_untracked() {
                        let idle_exact: Vec<ReadyFrameMark> = pipe
                            .ready_frames
                            .iter()
                            .copied()
                            .filter(|m| m.kind == ReadyFrameKind::Exact)
                            .collect();
                        out.extend(remap(idle_idx, &idle_exact));
                    }
                }
                out
            }
        }
    }

    /// Live FetchMap file fractions → timeline bar fractions (playlist: clip window).
    /// Diagnostic; strip paint uses [`Self::strip_fetch_playable_runs`].
    pub fn strip_fetch_ranges(self, ranges: &[(f32, f32)]) -> Vec<(f32, f32)> {
        match self.inner {
            TimelineInner::Video(_) => ranges.to_vec(),
            TimelineInner::Playlist(p) => {
                let live_clip = p.live_clip.get();
                let model = p.model.get();
                let total = self.frame_count.get().max(1);
                let start = model.clip_start_frame(live_clip).unwrap_or(0);
                let end = model.clip_end_frame(live_clip).unwrap_or(total);
                fetch_ranges_in_clip_window(ranges, start, end, total)
            }
        }
    }

    /// Playable local FrameId runs → timeline units (playlist: live clip remap).
    /// Gray = buffered-like; sole-on-VT later. **no equivalent**.
    pub fn strip_fetch_playable_runs(self, runs: &[(u64, u64)]) -> Vec<(u64, u64)> {
        match self.inner {
            TimelineInner::Video(_) => runs.to_vec(),
            TimelineInner::Playlist(p) => {
                let live_clip = p.live_clip.get();
                let model = p.model.get();
                runs.iter()
                    .filter_map(|&(lo, hi)| {
                        let lo_f = u32::try_from(lo).ok()?;
                        let hi_f = u32::try_from(hi).ok()?;
                        let glo =
                            u64::from(model.global_frame_at(PlaylistPos::new(live_clip, lo_f))?);
                        let ghi =
                            u64::from(model.global_frame_at(PlaylistPos::new(live_clip, hi_f))?);
                        Some((glo, ghi))
                    })
                    .collect()
            }
        }
    }

    pub fn scrub_pointer_down(self, unit: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.scrub_pointer_down(unit),
            TimelineInner::Playlist(p) => p.scrub_timeline(unit, ScrubPhase::Down),
        }
    }

    pub fn scrub_pointer_move(self, unit: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.scrub_pointer_move(unit),
            TimelineInner::Playlist(p) => p.scrub_timeline(unit, ScrubPhase::Move),
        }
    }

    pub fn scrub_pointer_up(self, unit: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.scrub_pointer_up(unit),
            TimelineInner::Playlist(p) => p.scrub_timeline(unit, ScrubPhase::Up),
        }
    }

    pub fn paint_hover_thumb(self, unit: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32) {
        match self.inner {
            TimelineInner::Video(v) => v.paint_hover_thumb(unit, dest, max_w, max_h),
            TimelineInner::Playlist(p) => p.paint_hover_thumb(unit, dest, max_w, max_h),
        }
    }

    pub fn try_paint_hover_thumb(
        self,
        unit: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> bool {
        match self.inner {
            TimelineInner::Video(v) => v.try_paint_hover_thumb(unit, dest, max_w, max_h),
            TimelineInner::Playlist(p) => p.try_paint_hover_thumb(unit, dest, max_w, max_h),
        }
    }

    pub fn cancel_hover_thumb(self) {
        match self.inner {
            TimelineInner::Video(v) => v.cancel_hover_thumb(),
            TimelineInner::Playlist(p) => p.cancel_hover_thumb(),
        }
    }
}

fn fetch_ranges_in_clip_window(
    ranges: &[(f32, f32)],
    clip_start: u32,
    clip_end: u32,
    total: u32,
) -> Vec<(f32, f32)> {
    if ranges.is_empty() || total == 0 || clip_end <= clip_start {
        return Vec::new();
    }
    let t = f64::from(total);
    let origin = f64::from(clip_start) / t;
    let span = f64::from(clip_end.saturating_sub(clip_start)) / t;
    ranges
        .iter()
        .map(|&(a, b)| {
            let a = (origin + f64::from(a).clamp(0.0, 1.0) * span) as f32;
            let b = (origin + f64::from(b).clamp(0.0, 1.0) * span) as f32;
            (a.min(b), a.max(b))
        })
        .collect()
}

impl From<VideoRef> for TimelineRef {
    fn from(video: VideoRef) -> Self {
        Self::from_video(video)
    }
}

impl From<VideoPlaylistRef> for TimelineRef {
    fn from(playlist: VideoPlaylistRef) -> Self {
        Self::from_playlist(playlist)
    }
}

impl VideoRef {
    /// Scrubber / hover / strip surface for this element (shared with VideoPlaylist).
    pub fn as_timeline(self) -> TimelineRef {
        TimelineRef::from_video(self)
    }
}

impl VideoPlaylistRef {
    /// Frame-sum scrub/hover/strip surface (units = Σ presentation frames).
    pub fn as_timeline(self) -> TimelineRef {
        TimelineRef::from_playlist(self)
    }
}
