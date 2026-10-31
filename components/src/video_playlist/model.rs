//! Frame-sum clip list and global FrameId ↔ [`super::PlaylistPos`] maps.
//!
//! Scrub / tip / zebra / strips use Σ presentation frames (same `w/n` as a single
//! clip). Wall-time helpers remain for diagnostics; Play clock stays in the engine.

use fpmedia_web::VideoMetadata;

use super::PlaylistPos;

/// One playlist entry after metadata probe (timing only — no decode session).
#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistClip {
    pub frame_count: u32,
    /// Presentation duration in seconds (`VideoMetadata::duration`).
    pub duration_secs: f64,
    pub fps: Option<f64>,
}

impl PlaylistClip {
    pub fn from_meta(meta: &VideoMetadata) -> Self {
        let frame_count = u32::try_from(meta.frame_count).unwrap_or(u32::MAX);
        Self {
            frame_count,
            duration_secs: meta.duration().seconds.max(0.0),
            fps: meta.fps,
        }
    }

    pub fn last_frame(&self) -> u32 {
        self.frame_count.saturating_sub(1)
    }

    /// Local presentation frame for time within this clip (`0…duration`).
    pub fn frame_at_local_time(&self, local_secs: f64) -> u32 {
        let last = self.last_frame();
        if self.frame_count == 0 || last == 0 {
            return 0;
        }
        if self.duration_secs <= f64::EPSILON {
            return 0;
        }
        let t = local_secs.clamp(0.0, self.duration_secs);
        // Half-open mapping: end instant → last frame.
        if t >= self.duration_secs {
            return last;
        }
        let f = (t / self.duration_secs) * f64::from(self.frame_count);
        (f.floor() as u32).min(last)
    }

    /// Start time of `local_frame` within this clip.
    pub fn time_at_frame(&self, local_frame: u32) -> f64 {
        if self.frame_count == 0 || self.duration_secs <= f64::EPSILON {
            return 0.0;
        }
        let f = local_frame.min(self.last_frame());
        (f64::from(f) / f64::from(self.frame_count)) * self.duration_secs
    }
}

/// Ordered playlist with cached **frame** offsets (scrub domain) + duration starts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaylistModel {
    clips: Vec<PlaylistClip>,
    /// `frame_starts[i]` = Σ `frame_count` for clips `0..i`.
    frame_starts: Vec<u32>,
    total_frame_count: u32,
    /// `starts[i]` = sum of `duration_secs` for clips `0..i` (wall-time helpers).
    starts: Vec<f64>,
    total_duration_secs: f64,
}

impl PlaylistModel {
    pub fn new(clips: Vec<PlaylistClip>) -> Self {
        let mut frame_starts = Vec::with_capacity(clips.len());
        let mut starts = Vec::with_capacity(clips.len());
        let mut frame_acc = 0u32;
        let mut time_acc = 0.0;
        for c in &clips {
            frame_starts.push(frame_acc);
            starts.push(time_acc);
            frame_acc = frame_acc.saturating_add(c.frame_count);
            time_acc += c.duration_secs.max(0.0);
        }
        Self {
            clips,
            frame_starts,
            total_frame_count: frame_acc,
            starts,
            total_duration_secs: time_acc,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    pub fn len(&self) -> usize {
        self.clips.len()
    }

    pub fn clips(&self) -> &[PlaylistClip] {
        &self.clips
    }

    pub fn clip(&self, index: usize) -> Option<&PlaylistClip> {
        self.clips.get(index)
    }

    pub fn total_duration_secs(&self) -> f64 {
        self.total_duration_secs
    }

    /// Scrub-domain length for [`crate::TimelineRef`] (Σ presentation frames).
    pub fn total_frame_count(&self) -> u32 {
        self.total_frame_count
    }

    /// Global frame index at the start of clip `index`.
    pub fn clip_start_frame(&self, index: u32) -> Option<u32> {
        self.frame_starts.get(index as usize).copied()
    }

    /// Exclusive end global frame of clip `index` (next start / total).
    pub fn clip_end_frame(&self, index: u32) -> Option<u32> {
        let i = index as usize;
        if i >= self.clips.len() {
            return None;
        }
        if i + 1 < self.clips.len() {
            self.clip_start_frame(index + 1)
        } else {
            Some(self.total_frame_count)
        }
    }

    /// Map playlist scrub units (global frame) → position.
    pub fn at_global_frame(&self, global: u32) -> Option<PlaylistPos> {
        if self.clips.is_empty() {
            return None;
        }
        if self.total_frame_count == 0 {
            return Some(PlaylistPos::new(0, 0));
        }
        if global >= self.total_frame_count {
            let i = self.clips.len() - 1;
            return Some(PlaylistPos {
                clip_index: i as u32,
                local_frame: self.clips[i].last_frame(),
            });
        }
        let mut i = 0usize;
        while i + 1 < self.clips.len() && global >= self.frame_starts[i + 1] {
            i += 1;
        }
        let local = global.saturating_sub(self.frame_starts[i]);
        let clip = &self.clips[i];
        Some(PlaylistPos {
            clip_index: i as u32,
            local_frame: local.min(clip.last_frame()),
        })
    }

    /// Global frame at the start of `pos` (clamped to the clip).
    pub fn global_frame_at(&self, pos: PlaylistPos) -> Option<u32> {
        let i = pos.clip_index as usize;
        let clip = self.clip(i)?;
        let start = *self.frame_starts.get(i)?;
        let local = pos.local_frame.min(clip.last_frame());
        Some(start.saturating_add(local))
    }

    /// Global time at the start of a clip.
    pub fn clip_start_secs(&self, index: usize) -> Option<f64> {
        self.starts.get(index).copied()
    }

    /// Map global time → clip + local frame. Empty model → `None`.
    pub fn at_time(&self, t_secs: f64) -> Option<PlaylistPos> {
        if self.clips.is_empty() {
            return None;
        }
        let t = t_secs.max(0.0);
        if t >= self.total_duration_secs {
            let i = self.clips.len() - 1;
            let clip = &self.clips[i];
            return Some(PlaylistPos {
                clip_index: i as u32,
                local_frame: clip.last_frame(),
            });
        }
        let mut i = 0usize;
        while i + 1 < self.clips.len() && t >= self.starts[i + 1] {
            i += 1;
        }
        let local = t - self.starts[i];
        let clip = &self.clips[i];
        Some(PlaylistPos {
            clip_index: i as u32,
            local_frame: clip.frame_at_local_time(local),
        })
    }

    /// Global time at the start of `pos` (clamped to the clip).
    pub fn time_at(&self, pos: PlaylistPos) -> Option<f64> {
        let i = pos.clip_index as usize;
        let clip = self.clip(i)?;
        let start = self.starts[i];
        Some(start + clip.time_at_frame(pos.local_frame))
    }

    /// `frac` in `0…1` along scrub domain (Σ frames) → position.
    pub fn at_fraction(&self, frac: f64) -> Option<PlaylistPos> {
        if self.clips.is_empty() {
            return None;
        }
        if self.total_frame_count == 0 {
            return Some(PlaylistPos {
                clip_index: 0,
                local_frame: 0,
            });
        }
        let f = frac.clamp(0.0, 1.0);
        let g = (f * f64::from(self.total_frame_count)).floor() as u32;
        self.at_global_frame(g.min(self.total_frame_count.saturating_sub(1)))
    }

    /// Next clip index after `clip_index`, if any.
    pub fn next_clip_index(&self, clip_index: u32) -> Option<u32> {
        let n = self.clips.len() as u32;
        let next = clip_index.checked_add(1)?;
        (next < n).then_some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(frames: u32, duration_secs: f64, fps: f64) -> PlaylistClip {
        PlaylistClip {
            frame_count: frames,
            duration_secs,
            fps: Some(fps),
        }
    }

    #[test]
    fn empty_model() {
        let m = PlaylistModel::new(vec![]);
        assert!(m.is_empty());
        assert!(m.at_time(0.0).is_none());
        assert!(m.at_fraction(0.5).is_none());
        assert_eq!(m.total_frame_count(), 0);
    }

    #[test]
    fn single_clip_time_roundtrip() {
        let m = PlaylistModel::new(vec![clip(48, 2.0, 24.0)]);
        assert!((m.total_duration_secs() - 2.0).abs() < 1e-9);
        assert_eq!(m.total_frame_count(), 48);
        let p0 = m.at_time(0.0).unwrap();
        assert_eq!(p0, PlaylistPos::new(0, 0));
        let p_mid = m.at_time(1.0).unwrap();
        assert_eq!(p_mid.clip_index, 0);
        assert_eq!(p_mid.local_frame, 24);
        assert_eq!(m.global_frame_at(p_mid), Some(24));
        assert_eq!(m.at_global_frame(24), Some(p_mid));
    }

    #[test]
    fn two_clips_frame_sum_axis() {
        // 24 frames + 60 frames → scrub midpoint is frame 42 (in clip 1), not the cut.
        let m = PlaylistModel::new(vec![clip(24, 1.0, 24.0), clip(60, 1.0, 60.0)]);
        assert_eq!(m.total_frame_count(), 84);
        assert_eq!(m.clip_start_frame(0), Some(0));
        assert_eq!(m.clip_end_frame(0), Some(24));
        assert_eq!(m.clip_start_frame(1), Some(24));
        assert_eq!(m.clip_end_frame(1), Some(84));

        assert_eq!(m.at_global_frame(0), Some(PlaylistPos::new(0, 0)));
        assert_eq!(m.at_global_frame(23), Some(PlaylistPos::new(0, 23)));
        assert_eq!(m.at_global_frame(24), Some(PlaylistPos::new(1, 0)));
        assert_eq!(m.at_global_frame(83), Some(PlaylistPos::new(1, 59)));
        assert_eq!(m.at_global_frame(99), Some(PlaylistPos::new(1, 59)));

        // Half of Σ frames ≈ frame 42 → clip 1 local 18.
        assert_eq!(m.at_fraction(0.5).unwrap(), PlaylistPos::new(1, 18));
    }

    #[test]
    fn two_clips_time_helpers_unchanged() {
        let m = PlaylistModel::new(vec![clip(24, 1.0, 24.0), clip(60, 1.0, 60.0)]);
        assert_eq!(m.at_time(1.0).unwrap(), PlaylistPos::new(1, 0));
        let in_b = m.at_time(1.5).unwrap();
        assert_eq!(in_b.clip_index, 1);
        assert_eq!(in_b.local_frame, 30);
    }

    #[test]
    fn next_clip_index() {
        let m = PlaylistModel::new(vec![clip(10, 1.0, 10.0), clip(10, 1.0, 10.0)]);
        assert_eq!(m.next_clip_index(0), Some(1));
        assert_eq!(m.next_clip_index(1), None);
    }
}
