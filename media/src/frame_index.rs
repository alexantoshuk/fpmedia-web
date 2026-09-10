//! Presentation-order frame index built from demuxed samples.

use crate::types::{FrameId, Timestamp};

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
}
