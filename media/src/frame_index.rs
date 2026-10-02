//! Presentation-order frame index built from demuxed samples.
//!
//! Sync lookup is O(log k) via a decode-order sync table; GOP / decode windows
//! are O(gop) via a dense `decode_order → FrameId` map (not a full-frame scan).
//! Local invention — mediabunny has no `FrameIndex` equivalent; packet cursors
//! use their own key probes.

use crate::types::{FrameId, GopProfile, SHORT_GOP_MAX_LEN, Timestamp};

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
    /// Sync samples sorted by `decode_order`: `(decode_order, frame_id)`.
    sync_by_decode: Vec<(usize, FrameId)>,
    /// Dense map: `by_decode[decode_order] = presentation FrameId`.
    by_decode: Vec<FrameId>,
}

impl FrameIndex {
    pub fn from_entries(mut entries: Vec<FrameEntry>) -> Self {
        entries.sort_by_key(|e| (e.composition_timestamp_ticks, e.decode_order));
        for (i, entry) in entries.iter_mut().enumerate() {
            entry.frame = i as FrameId;
        }
        let mut index = Self {
            frames: entries,
            sync_by_decode: Vec::new(),
            by_decode: Vec::new(),
        };
        index.rebuild_aux();
        index
    }

    /// Append fragment samples (`mediabunny: readFragment` grows the sample table).
    ///
    /// `new` uses local decode_order `0..`; remapped to continue after the current
    /// max. Re-sorts by composition time and rebuilds sync/decode maps.
    pub fn append_entries(&mut self, mut new: Vec<FrameEntry>) {
        if new.is_empty() {
            return;
        }
        let base_do = self
            .frames
            .iter()
            .map(|e| e.decode_order)
            .max()
            .map(|d| d.saturating_add(1))
            .unwrap_or(0);
        for e in &mut new {
            e.decode_order = e.decode_order.saturating_add(base_do);
        }
        self.frames.extend(new);
        self.frames
            .sort_by_key(|e| (e.composition_timestamp_ticks, e.decode_order));
        for (i, entry) in self.frames.iter_mut().enumerate() {
            entry.frame = i as FrameId;
        }
        self.rebuild_aux();
    }

    fn rebuild_aux(&mut self) {
        let n = self.frames.len();
        if n == 0 {
            self.by_decode.clear();
            self.sync_by_decode.clear();
            return;
        }
        let max_do = self
            .frames
            .iter()
            .map(|e| e.decode_order)
            .max()
            .unwrap_or(0);
        self.by_decode = vec![0; max_do + 1];
        self.sync_by_decode.clear();
        self.sync_by_decode.reserve(n);
        for e in &self.frames {
            if e.decode_order < self.by_decode.len() {
                self.by_decode[e.decode_order] = e.frame;
            }
            if e.is_sync {
                self.sync_by_decode.push((e.decode_order, e.frame));
            }
        }
        self.sync_by_decode.sort_unstable_by_key(|(d, _)| *d);
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
    ///
    /// Maintains [`Self::sync_by_decode`] incrementally (O(k) shift on insert/remove).
    pub fn set_sync(&mut self, frame: FrameId, is_sync: bool) {
        let Some(entry) = self.frames.get_mut(frame as usize) else {
            return;
        };
        if entry.is_sync == is_sync {
            return;
        }
        let decode_order = entry.decode_order;
        entry.is_sync = is_sync;
        if is_sync {
            let pos = self
                .sync_by_decode
                .partition_point(|(d, _)| *d < decode_order);
            self.sync_by_decode.insert(pos, (decode_order, frame));
        } else if let Ok(pos) = self
            .sync_by_decode
            .binary_search_by_key(&decode_order, |(d, _)| *d)
        {
            self.sync_by_decode.remove(pos);
        }
    }

    /// Latest sync sample at or before `frame` in **decode** order.
    ///
    /// Uses `decode_order` (not DTS alone) so B-frame reordering stays correct.
    /// O(log k) over sync samples.
    pub fn nearest_keyframe(&self, frame: FrameId) -> Option<FrameId> {
        let target = self.get(frame)?;
        let i = self
            .sync_by_decode
            .partition_point(|(d, _)| *d <= target.decode_order);
        if i == 0 {
            return None;
        }
        Some(self.sync_by_decode[i - 1].1)
    }

    /// Next sync sample strictly after `frame` in decode order (start of next GOP).
    /// O(log k) over sync samples.
    pub fn next_keyframe_after(&self, frame: FrameId) -> Option<FrameId> {
        let entry = self.get(frame)?;
        let i = self
            .sync_by_decode
            .partition_point(|(d, _)| *d <= entry.decode_order);
        self.sync_by_decode.get(i).map(|(_, f)| *f)
    }

    /// True when `new_key_do` is the sync decode-order that immediately follows
    /// `prev_key_do` (Play crossing one GOP boundary). Non-adjacent scrub jumps
    /// are false. Soft sequential next-key — mediabunny `media-sink.ts` keeps
    /// the decoder across adjacent GOP starts without recreate.
    pub fn is_sequential_next_sync_do(&self, prev_key_do: usize, new_key_do: usize) -> bool {
        if new_key_do <= prev_key_do {
            return false;
        }
        let Some(prev_key_frame) = self
            .frames
            .iter()
            .find(|e| e.is_sync && e.decode_order == prev_key_do)
            .map(|e| e.frame)
        else {
            return false;
        };
        self.next_keyframe_after(prev_key_frame)
            .and_then(|nk| self.get(nk).map(|e| e.decode_order))
            .is_some_and(|d| d == new_key_do)
    }

    /// All sync `FrameId`s in decode order (GOP starts). O(k).
    pub fn sync_frame_ids(&self) -> Vec<FrameId> {
        self.sync_by_decode.iter().map(|(_, f)| *f).collect()
    }

    /// Presentation `FrameId`s in the GOP that begins at sync `key_frame`
    /// (exclusive of the next sync). `key_frame` must be a sync sample.
    ///
    /// O(gop + log k) via decode-order map + sync binary search.
    pub fn gop_frame_ids(&self, key_frame: FrameId) -> Option<Vec<FrameId>> {
        let key = self.get(key_frame)?;
        if !key.is_sync {
            return None;
        }
        let start = key.decode_order;
        let end = self
            .next_keyframe_after(key_frame)
            .and_then(|n| self.get(n))
            .map(|e| e.decode_order)
            .unwrap_or(self.by_decode.len());
        if start > end || end > self.by_decode.len() {
            return None;
        }
        let mut ids: Vec<FrameId> = self.by_decode[start..end].to_vec();
        ids.sort_unstable();
        Some(ids)
    }

    /// Next `limit` presentation frames after `after_do` in track decode order.
    ///
    /// O(`limit`) via dense [`Self::by_decode`] — do **not** scan/sort all
    /// remaining frames (Play pump calls this every packet; ChG-class ~45k
    /// frames made the filter+sort path a main-thread skip storm).
    pub fn frames_after_decode(&self, after_do: usize, limit: usize) -> Vec<FrameId> {
        if limit == 0 {
            return Vec::new();
        }
        let start = after_do.saturating_add(1);
        if start >= self.by_decode.len() {
            return Vec::new();
        }
        let end = start.saturating_add(limit).min(self.by_decode.len());
        self.by_decode[start..end].to_vec()
    }

    /// Next `limit` frames after `after_do` within the GOP that starts at `key_do`.
    ///
    /// O(`limit` + log k) — sync binary search for the next key, then `by_decode`.
    pub fn frames_after_in_gop(
        &self,
        key_do: usize,
        after_do: usize,
        limit: usize,
    ) -> Vec<FrameId> {
        if limit == 0 {
            return Vec::new();
        }
        let start = after_do.saturating_add(1);
        if start >= self.by_decode.len() {
            return Vec::new();
        }
        let gop_end = self.sync_by_decode.partition_point(|(d, _)| *d <= key_do);
        let next_key_do = self.sync_by_decode.get(gop_end).map(|(d, _)| *d);
        let end_cap = next_key_do.unwrap_or(self.by_decode.len());
        if start >= end_cap {
            return Vec::new();
        }
        let end = start.saturating_add(limit).min(end_cap);
        self.by_decode[start..end].to_vec()
    }

    /// Decode-order window from nearest keyframe through `frame` (inclusive).
    ///
    /// Samples are ordered by `decode_order` so a decoder can be fed correctly
    /// even when composition timestamps are reordered (B-frames).
    /// O(window + log k).
    pub fn decode_window(&self, frame: FrameId) -> Option<Vec<FrameId>> {
        let key = self.nearest_keyframe(frame)?;
        let target = self.get(frame)?;
        let key_entry = self.get(key)?;

        let start = key_entry.decode_order;
        let end = target.decode_order;
        if end < start || end >= self.by_decode.len() {
            return None;
        }

        let window: Vec<FrameId> = self.by_decode[start..=end].to_vec();

        if window.first().copied() != Some(key) {
            return None;
        }
        if !window.contains(&frame) {
            return None;
        }

        Some(window)
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
        let max_do = self.by_decode.len().saturating_sub(1);
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
    let max = *lengths.iter().max().unwrap_or(&1);
    // All-Intra: every GOP is length 1 (every sample sync), or one sync per frame.
    if max == 1 {
        return GopProfile::AllIntra;
    }
    // Short-IP: max ≤ cap. Scene cuts shrink some GOPs (large spread) — still Short.
    // Was: spread ≤ 2 → false Long on ShotGrid-like GOP≈30 with cuts.
    if max <= SHORT_GOP_MAX_LEN {
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
    if sorted.len().is_multiple_of(2) {
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
    fn append_entries_continues_decode_order() {
        let mut index = FrameIndex::from_entries(vec![entry(0, true, 0, 0), entry(1, false, 1, 1)]);
        index.append_entries(vec![entry(0, true, 2, 2), entry(1, false, 3, 3)]);
        assert_eq!(index.len(), 4);
        assert_eq!(index.get(2).unwrap().decode_order, 2);
        assert_eq!(index.get(3).unwrap().decode_order, 3);
        assert!(index.get(2).unwrap().is_sync);
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
        assert_eq!(index.frames_after_decode(0, 2), vec![1, 2]);
        assert_eq!(index.frames_after_decode(2, 10), vec![3, 4]);
        assert_eq!(index.frames_after_in_gop(0, 0, 10), vec![1, 2]);
        assert_eq!(index.frames_after_in_gop(0, 1, 10), vec![2]);
        assert!(index.is_sequential_next_sync_do(0, 3));
        assert!(!index.is_sequential_next_sync_do(0, 6)); // no such sync
        assert!(!index.is_sequential_next_sync_do(3, 0));
    }

    #[test]
    fn sequential_next_sync_do_skips_non_adjacent() {
        let index = FrameIndex::from_entries(vec![
            entry(0, true, 0, 0),
            entry(1, false, 1, 1),
            entry(2, false, 2, 2),
            entry(3, true, 3, 3),
            entry(4, false, 4, 4),
            entry(5, false, 5, 5),
            entry(6, true, 6, 6),
        ]);
        assert!(index.is_sequential_next_sync_do(0, 3));
        assert!(index.is_sequential_next_sync_do(3, 6));
        assert!(!index.is_sequential_next_sync_do(0, 6));
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
    fn set_sync_demote_updates_nearest_and_gop() {
        // Two syncs; demote the second → one long GOP from frame 0.
        let mut index = FrameIndex::from_entries(vec![
            entry(0, true, 0, 0),
            entry(1, false, 1, 1),
            entry(2, false, 2, 2),
            entry(3, true, 3, 3),
            entry(4, false, 4, 4),
        ]);
        assert_eq!(index.nearest_keyframe(4), Some(3));
        index.set_sync(3, false);
        assert_eq!(index.sync_frame_ids(), vec![0]);
        assert_eq!(index.nearest_keyframe(4), Some(0));
        assert_eq!(index.next_keyframe_after(0), None);
        assert_eq!(index.gop_frame_ids(0), Some(vec![0, 1, 2, 3, 4]));
        assert_eq!(index.gop_frame_ids(3), None); // no longer sync
    }

    #[test]
    fn set_sync_promote_inserts_in_decode_order() {
        let mut index = FrameIndex::from_entries(vec![
            entry(0, true, 0, 0),
            entry(1, false, 1, 1),
            entry(2, false, 2, 2),
            entry(3, false, 3, 3),
            entry(4, false, 4, 4),
        ]);
        index.set_sync(3, true);
        assert_eq!(index.sync_frame_ids(), vec![0, 3]);
        assert_eq!(index.nearest_keyframe(4), Some(3));
        assert_eq!(index.gop_frame_ids(0), Some(vec![0, 1, 2]));
        assert_eq!(index.gop_frame_ids(3), Some(vec![3, 4]));
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
    fn gop_profile_short_with_scene_cut_spread() {
        // ShotGrid-like: ~GOP30 with short GOPs at cuts — must not become Long.
        assert_eq!(
            classify_gop_profile(&[3, 30, 28, 30, 5], 96),
            GopProfile::ShortFixed { len: 30 }
        );
        assert_eq!(
            classify_gop_profile(&[1, 27, 30, 12, 30], 100),
            GopProfile::ShortFixed { len: 30 }
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
