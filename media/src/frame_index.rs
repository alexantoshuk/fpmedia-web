//! Presentation-order frame index built from demuxed samples.

use crate::types::{FrameId, GopProfile, Timestamp, SHORT_GOP_MAX_LEN, SHORT_GOP_MAX_SPREAD};

/// One frame in presentation order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameEntry {
    pub frame: FrameId,
    /// Index into the demuxer's decode-order sample list.
    pub decode_order: usize,
    pub is_sync: bool,
    pub decode_timestamp_ticks: i64,
    pub composition_timestamp_ticks: i64,
    pub duration_ticks: u64,
    pub timescale: u64,
    /// Byte range into the original container buffer.
    pub offset: u64,
    pub size: u64,
}

impl FrameEntry {
    pub fn composition_timestamp(&self) -> Timestamp {
        Timestamp::from_ticks(self.composition_timestamp_ticks, self.timescale)
    }

    pub fn byte_range(&self) -> std::ops::Range<usize> {
        let start = self.offset as usize;
        let end = (self.offset + self.size) as usize;
        start..end
    }
}

/// Source of truth for frame ↔ timestamp mapping (handles VFR).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameIndex {
    frames: Vec<FrameEntry>,
}

impl FrameIndex {
    pub fn from_entries(mut entries: Vec<FrameEntry>) -> Self {
        entries.sort_by_key(|e| (e.composition_timestamp_ticks, e.decode_order));
        for (i, entry) in entries.iter_mut().enumerate() {
            entry.frame = i as FrameId;
        }
        Self { frames: entries }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn get(&self, frame: FrameId) -> Option<&FrameEntry> {
        self.frames.get(frame as usize)
    }

    pub fn frames(&self) -> &[FrameEntry] {
        self.frames.as_slice()
    }

    /// Update sync flag after bitstream verify (mediabunny `verifyKeyPackets`).
    pub fn set_sync(&mut self, frame: FrameId, is_sync: bool) {
        if let Some(entry) = self.frames.get_mut(frame as usize) {
            entry.is_sync = is_sync;
        }
    }

    /// Latest sync sample at or before `frame` in **decode** order.
    ///
    /// Uses `decode_order` (not DTS alone) so B-frame reordering stays correct.
    pub fn nearest_keyframe(&self, frame: FrameId) -> Option<FrameId> {
        let target = self.get(frame)?;
        self.frames
            .iter()
            .filter(|e| e.is_sync && e.decode_order <= target.decode_order)
            .max_by_key(|e| e.decode_order)
            .map(|e| e.frame)
    }

    /// Next sync sample strictly after `frame` in decode order (start of next GOP).
    pub fn next_keyframe_after(&self, frame: FrameId) -> Option<FrameId> {
        let entry = self.get(frame)?;
        self.frames
            .iter()
            .filter(|e| e.is_sync && e.decode_order > entry.decode_order)
            .min_by_key(|e| e.decode_order)
            .map(|e| e.frame)
    }

    /// All sync `FrameId`s in decode order (GOP starts).
    pub fn sync_frame_ids(&self) -> Vec<FrameId> {
        let mut keys: Vec<(usize, FrameId)> = self
            .frames
            .iter()
            .filter(|e| e.is_sync)
            .map(|e| (e.decode_order, e.frame))
            .collect();
        keys.sort_by_key(|(decode_order, _)| *decode_order);
        keys.into_iter().map(|(_, frame)| frame).collect()
    }

    /// Presentation `FrameId`s in the GOP that begins at sync `key_frame`
    /// (exclusive of the next sync). `key_frame` must be a sync sample.
    pub fn gop_frame_ids(&self, key_frame: FrameId) -> Option<Vec<FrameId>> {
        let key = self.get(key_frame)?;
        if !key.is_sync {
            return None;
        }
        let next_do = self
            .next_keyframe_after(key_frame)
            .and_then(|n| self.get(n))
            .map(|e| e.decode_order);
        let mut ids: Vec<FrameId> = self
            .frames
            .iter()
            .filter(|e| {
                e.decode_order >= key.decode_order
                    && next_do.map(|nd| e.decode_order < nd).unwrap_or(true)
            })
            .map(|e| e.frame)
            .collect();
        ids.sort_unstable();
        Some(ids)
    }

    /// Decode-order window from nearest keyframe through `frame` (inclusive).
    ///
    /// Samples are ordered by `decode_order` so a decoder can be fed correctly
    /// even when composition timestamps are reordered (B-frames).
    pub fn decode_window(&self, frame: FrameId) -> Option<Vec<FrameId>> {
        let key = self.nearest_keyframe(frame)?;
        let target = self.get(frame)?;
        let key_entry = self.get(key)?;

        let mut window: Vec<&FrameEntry> = self
            .frames
            .iter()
            .filter(|e| {
                e.decode_order >= key_entry.decode_order
                    && e.decode_order <= target.decode_order
            })
            .collect();

        window.sort_by_key(|e| e.decode_order);

        if window.first().map(|e| e.frame) != Some(key) {
            return None;
        }
        if !window.iter().any(|e| e.frame == frame) {
            return None;
        }

        Some(window.into_iter().map(|e| e.frame).collect())
    }

    /// Find the frame whose composition time is closest to `seconds`
    /// (not greater than `seconds` when possible — scrub semantics).
    pub fn frame_at_time(&self, seconds: f64) -> Option<FrameId> {
        if self.frames.is_empty() {
            return None;
        }
        let mut best = 0u64;
        for entry in &self.frames {
            if entry.composition_timestamp().seconds <= seconds {
                best = entry.frame;
            } else {
                break;
            }
        }
        Some(best)
    }

    /// Decode-order lengths of each GOP (samples from sync inclusive to next sync exclusive).
    ///
    /// Empty index → empty vec. No sync samples → one length covering the whole track.
    pub fn gop_lengths(&self) -> Vec<u32> {
        if self.frames.is_empty() {
            return Vec::new();
        }
        let syncs = self.sync_frame_ids();
        if syncs.is_empty() {
            return vec![self.frames.len() as u32];
        }
        let max_do = self
            .frames
            .iter()
            .map(|e| e.decode_order)
            .max()
            .unwrap_or(0);
        let mut lengths = Vec::with_capacity(syncs.len());
        for (i, &key) in syncs.iter().enumerate() {
            let start_do = self.get(key).map(|e| e.decode_order).unwrap_or(0);
            let end_do = if i + 1 < syncs.len() {
                self.get(syncs[i + 1])
                    .map(|e| e.decode_order)
                    .unwrap_or(start_do)
            } else {
                max_do.saturating_add(1)
            };
            lengths.push(end_do.saturating_sub(start_do).max(1) as u32);
        }
        lengths
    }

    /// Classify keyframe spacing for scrub-cache / fill policy.
    pub fn gop_profile(&self) -> GopProfile {
        classify_gop_profile(&self.gop_lengths(), self.frames.len())
    }
}

/// Pure classification from observed GOP lengths (testable without a full index).
pub(crate) fn classify_gop_profile(lengths: &[u32], frame_count: usize) -> GopProfile {
    if frame_count == 0 || lengths.is_empty() {
        return GopProfile::Long { median: 0, max: 0 };
    }
    let min = *lengths.iter().min().unwrap_or(&1);
    let max = *lengths.iter().max().unwrap_or(&1);
    // All-Intra: every GOP is length 1 (every sample sync), or one sync per frame.
    if max == 1 {
        return GopProfile::AllIntra;
    }
    let spread = max.saturating_sub(min);
    if max <= SHORT_GOP_MAX_LEN && spread <= SHORT_GOP_MAX_SPREAD {
        return GopProfile::ShortFixed { len: max };
    }
    let median = median_u32(lengths);
    GopProfile::Long { median, max }
}

fn median_u32(values: &[u32]) -> u32 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        // Lower median for even counts — stable, no float.
        sorted[mid - 1]
    } else {
        sorted[mid]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(decode_order: usize, is_sync: bool, dts: i64, cts: i64) -> FrameEntry {
        FrameEntry {
            frame: 0,
            decode_order,
            is_sync,
            decode_timestamp_ticks: dts,
            composition_timestamp_ticks: cts,
            duration_ticks: 1,
            timescale: 24,
            offset: decode_order as u64 * 10,
            size: 10,
        }
    }

    #[test]
    fn sorts_by_composition_time() {
        let index = FrameIndex::from_entries(vec![
            entry(0, true, 0, 0),
            entry(1, false, 1, 2),
            entry(2, false, 2, 1),
        ]);
        assert_eq!(index.len(), 3);
        assert_eq!(index.get(0).unwrap().composition_timestamp_ticks, 0);
        assert_eq!(index.get(1).unwrap().composition_timestamp_ticks, 1);
        assert_eq!(index.get(2).unwrap().composition_timestamp_ticks, 2);
        assert_eq!(index.get(1).unwrap().decode_order, 2);
    }

    #[test]
    fn nearest_keyframe_and_window_no_b_frames() {
        // I P P I P  — sync every 3 in decode order, cts == dts
        let index = FrameIndex::from_entries(vec![
            entry(0, true, 0, 0),
            entry(1, false, 1, 1),
            entry(2, false, 2, 2),
            entry(3, true, 3, 3),
            entry(4, false, 4, 4),
        ]);
        assert_eq!(index.nearest_keyframe(2), Some(0));
        assert_eq!(index.nearest_keyframe(4), Some(3));
        assert_eq!(index.decode_window(2), Some(vec![0, 1, 2]));
        assert_eq!(index.decode_window(4), Some(vec![3, 4]));
        assert_eq!(index.next_keyframe_after(0), Some(3));
        assert_eq!(index.gop_frame_ids(0), Some(vec![0, 1, 2]));
        assert_eq!(index.gop_frame_ids(3), Some(vec![3, 4]));
        assert_eq!(index.sync_frame_ids(), vec![0, 3]);
    }

    #[test]
    fn b_frame_window_includes_future_p_by_decode_order() {
        // Decode: I0, P4, B1, B2, B3  /  Present: I0, B1, B2, B3, P4
        let index = FrameIndex::from_entries(vec![
            entry(0, true, 0, 0),
            entry(1, false, 1, 4),
            entry(2, false, 2, 1),
            entry(3, false, 3, 2),
            entry(4, false, 4, 3),
        ]);
        // Presentation frame 1 is B1 (decode_order 2) — needs I0 + P4 first.
        assert_eq!(index.get(1).unwrap().decode_order, 2);
        assert_eq!(index.decode_window(1), Some(vec![0, 4, 1]));
        // Presentation frame 4 is P4 (decode_order 1) — only I0 + P4.
        assert_eq!(index.get(4).unwrap().decode_order, 1);
        assert_eq!(index.decode_window(4), Some(vec![0, 4]));
    }

    #[test]
    fn gop_profile_all_intra() {
        let index = FrameIndex::from_entries(vec![
            entry(0, true, 0, 0),
            entry(1, true, 1, 1),
            entry(2, true, 2, 2),
        ]);
        assert_eq!(index.gop_lengths(), vec![1, 1, 1]);
        assert_eq!(index.gop_profile(), GopProfile::AllIntra);
    }

    #[test]
    fn gop_profile_short_fixed() {
        // GOP = 3 throughout.
        let index = FrameIndex::from_entries(vec![
            entry(0, true, 0, 0),
            entry(1, false, 1, 1),
            entry(2, false, 2, 2),
            entry(3, true, 3, 3),
            entry(4, false, 4, 4),
            entry(5, false, 5, 5),
            entry(6, true, 6, 6),
            entry(7, false, 7, 7),
            entry(8, false, 8, 8),
        ]);
        assert_eq!(index.gop_lengths(), vec![3, 3, 3]);
        assert_eq!(index.gop_profile(), GopProfile::ShortFixed { len: 3 });
    }

    #[test]
    fn gop_profile_short_fixed_allows_small_spread() {
        assert_eq!(
            classify_gop_profile(&[11, 12, 12, 13], 48),
            GopProfile::ShortFixed { len: 13 }
        );
    }

    #[test]
    fn gop_profile_long_variable() {
        assert_eq!(
            classify_gop_profile(&[30, 120, 250, 250], 650),
            GopProfile::Long {
                median: 120,
                max: 250
            }
        );
        assert_eq!(
            classify_gop_profile(&[60, 60, 60], 180),
            GopProfile::Long {
                median: 60,
                max: 60
            }
        );
    }

    #[test]
    fn gop_profile_single_gop_is_long_when_over_short_cap() {
        let n = (SHORT_GOP_MAX_LEN + 1) as usize;
        let mut entries = vec![entry(0, true, 0, 0)];
        for i in 1..n {
            entries.push(entry(i, false, i as i64, i as i64));
        }
        let index = FrameIndex::from_entries(entries);
        assert_eq!(
            index.gop_profile(),
            GopProfile::Long {
                median: n as u32,
                max: n as u32
            }
        );
    }
}
