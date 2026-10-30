//! Duration-weighted clip list and time ↔ [`super::PlaylistPos`] maps.

use fpmedia_web::VideoMetadata;

use super::PlaylistPos;

fn secs_to_ms(secs: f64) -> u32 {
    if !secs.is_finite() || secs <= 0.0 {
        return 0;
    }
    (secs * 1000.0).floor().min(u32::MAX as f64) as u32
}

fn ms_to_secs(ms: u32) -> f64 {
    f64::from(ms) / 1000.0
}

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

/// Ordered playlist with cached duration offsets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaylistModel {
    clips: Vec<PlaylistClip>,
    /// `starts[i]` = sum of `duration_secs` for clips `0..i` (length = clips.len()).
    starts: Vec<f64>,
    total_duration_secs: f64,
}

impl PlaylistModel {
    pub fn new(clips: Vec<PlaylistClip>) -> Self {
        let mut starts = Vec::with_capacity(clips.len());
        let mut acc = 0.0;
        for c in &clips {
            starts.push(acc);
            acc += c.duration_secs.max(0.0);
        }
        Self {
            clips,
            starts,
            total_duration_secs: acc,
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

    /// Discrete scrub units for [`crate::TimelineRef`] (milliseconds, floored).
    pub fn total_duration_ms(&self) -> u32 {
        secs_to_ms(self.total_duration_secs)
    }

    /// Global time at the start of a clip.
    pub fn clip_start_secs(&self, index: usize) -> Option<f64> {
        self.starts.get(index).copied()
    }

    /// Scrub-domain start of clip `index` (ms).
    pub fn clip_start_ms(&self, index: u32) -> Option<u32> {
        self.clip_start_secs(index as usize).map(secs_to_ms)
    }

    /// Scrub-domain end of clip `index` (ms, exclusive vs next start / total).
    pub fn clip_end_ms(&self, index: u32) -> Option<u32> {
        let i = index as usize;
        if i >= self.clips.len() {
            return None;
        }
        if i + 1 < self.clips.len() {
            self.clip_start_ms(index + 1)
        } else {
            Some(self.total_duration_ms())
        }
    }

    /// Map playlist scrub units (ms) → position.
    pub fn at_ms(&self, ms: u32) -> Option<PlaylistPos> {
        self.at_time(ms_to_secs(ms))
    }

    /// Start of `pos` in scrub units (ms).
    pub fn ms_at(&self, pos: PlaylistPos) -> Option<u32> {
        self.time_at(pos).map(secs_to_ms)
    }

    /// Map global time → clip + local frame. Empty model → `None`.
    ///
    /// Times in `[start_i, start_{i+1})` belong to clip `i`; `t >= total` clamps to
    /// the last frame of the last clip.
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
        // Half-open intervals; playlists stay small — linear scan is fine.
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

    /// `frac` in `0…1` along total duration → position.
    pub fn at_fraction(&self, frac: f64) -> Option<PlaylistPos> {
        if self.clips.is_empty() {
            return None;
        }
        if self.total_duration_secs <= f64::EPSILON {
            return Some(PlaylistPos {
                clip_index: 0,
                local_frame: 0,
            });
        }
        let f = frac.clamp(0.0, 1.0);
        self.at_time(f * self.total_duration_secs)
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
    }

    #[test]
    fn single_clip_time_roundtrip() {
        // 24 fps × 48 frames = 2.0 s
        let m = PlaylistModel::new(vec![clip(48, 2.0, 24.0)]);
        assert!((m.total_duration_secs() - 2.0).abs() < 1e-9);
        let p0 = m.at_time(0.0).unwrap();
        assert_eq!(p0, PlaylistPos::new(0, 0));
        let p_mid = m.at_time(1.0).unwrap();
        assert_eq!(p_mid.clip_index, 0);
        assert_eq!(p_mid.local_frame, 24);
        let p_end = m.at_time(2.0).unwrap();
        assert_eq!(p_end.local_frame, 47);
        let t = m.time_at(PlaylistPos::new(0, 24)).unwrap();
        assert!((t - 1.0).abs() < 1e-9);
    }

    #[test]
    fn two_clips_boundary_and_mixed_rate() {
        // 1 s @ 24 fps (24 frames) + 1 s @ 60 fps (60 frames)
        let m = PlaylistModel::new(vec![clip(24, 1.0, 24.0), clip(60, 1.0, 60.0)]);
        assert!((m.total_duration_secs() - 2.0).abs() < 1e-9);

        let in_a = m.at_time(0.5).unwrap();
        assert_eq!(in_a.clip_index, 0);
        assert_eq!(in_a.local_frame, 12);

        // Exact cut → second clip frame 0
        let at_cut = m.at_time(1.0).unwrap();
        assert_eq!(at_cut, PlaylistPos::new(1, 0));

        let in_b = m.at_time(1.5).unwrap();
        assert_eq!(in_b.clip_index, 1);
        assert_eq!(in_b.local_frame, 30);

        let past = m.at_time(99.0).unwrap();
        assert_eq!(past, PlaylistPos::new(1, 59));

        // Equal duration → scrub midpoint is the cut (clip 1).
        assert_eq!(m.at_fraction(0.5).unwrap(), PlaylistPos::new(1, 0));
    }

    #[test]
    fn next_clip_index() {
        let m = PlaylistModel::new(vec![clip(10, 1.0, 10.0), clip(10, 1.0, 10.0)]);
        assert_eq!(m.next_clip_index(0), Some(1));
        assert_eq!(m.next_clip_index(1), None);
    }

    #[test]
    fn ms_units_roundtrip_cut() {
        let m = PlaylistModel::new(vec![clip(24, 1.0, 24.0), clip(60, 1.0, 60.0)]);
        assert_eq!(m.total_duration_ms(), 2000);
        assert_eq!(m.at_ms(1000), Some(PlaylistPos::new(1, 0)));
        assert_eq!(m.ms_at(PlaylistPos::new(1, 0)), Some(1000));
    }

    #[test]
    fn clip_ms_windows() {
        let m = PlaylistModel::new(vec![clip(24, 1.0, 24.0), clip(60, 1.0, 60.0)]);
        assert_eq!(m.clip_start_ms(0), Some(0));
        assert_eq!(m.clip_end_ms(0), Some(1000));
        assert_eq!(m.clip_start_ms(1), Some(1000));
        assert_eq!(m.clip_end_ms(1), Some(2000));
        assert_eq!(m.clip_start_ms(2), None);
    }
}
