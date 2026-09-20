//! Resident file-byte coverage for the demo scrubber gray strip.
//!
//! Backed by [`super::CachedSource`] fills (~8 MiB each). We keep a **bounded
//! union** of recent windows (not a single replace) so the bar does not
//! teleport when the cache slides, and not an unbounded historical union
//! (that painted the whole timeline after AVC verify).
//!
//! File offset → timeline % (not sample-accurate). mediabunny: no equivalent meter.

use std::sync::Mutex;

/// Cap total resident UI bytes (~3× default cache windows).
const MAX_UI_BYTES: u64 = 24 * 1024 * 1024;
/// Soft cap on disjoint segments after merge.
const MAX_UI_RANGES: usize = 8;
/// Merge windows separated by at most this many bytes (mdat gaps / A‑V interleave).
const MERGE_GAP: u64 = 256 * 1024;

/// Half-open `[start, end)` intervals, merged, file-absolute.
#[derive(Debug, Default)]
pub struct FetchMap {
    ranges: Mutex<Vec<(u64, u64)>>,
}

impl FetchMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// In-memory / full-download opens — entire file is resident.
    pub fn mark_full(&self, size: u64) {
        if size == 0 {
            return;
        }
        self.set_only(0, size);
    }

    /// Replace all coverage with one range (tests / mark_full).
    pub fn set_resident(&self, start: u64, end: u64) {
        self.set_only(start, end);
    }

    /// Note a new cache fill: merge into the union, then drop farthest segments
    /// until under [`MAX_UI_BYTES`] / [`MAX_UI_RANGES`].
    pub fn note_window(&self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let Ok(mut ranges) = self.ranges.lock() else {
            return;
        };
        merge_interval(&mut ranges, start, end, MERGE_GAP);
        trim_to_budget(&mut ranges, start, end, MAX_UI_BYTES, MAX_UI_RANGES);
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.ranges.lock() {
            g.clear();
        }
    }

    /// Coverage as fractions of `file_size` in `0.0..1.0`.
    pub fn file_fractions(&self, file_size: u64) -> Vec<(f32, f32)> {
        if file_size == 0 {
            return Vec::new();
        }
        let Ok(ranges) = self.ranges.lock() else {
            return Vec::new();
        };
        ranges
            .iter()
            .filter_map(|&(s, e)| {
                if e <= s {
                    return None;
                }
                let a = (s as f64 / file_size as f64) as f32;
                let b = (e as f64 / file_size as f64) as f32;
                Some((a.clamp(0.0, 1.0), b.clamp(0.0, 1.0).max(a)))
            })
            .collect()
    }

    fn set_only(&self, start: u64, end: u64) {
        let Ok(mut ranges) = self.ranges.lock() else {
            return;
        };
        if end <= start {
            ranges.clear();
            return;
        }
        ranges.clear();
        ranges.push((start, end));
    }

    #[cfg(test)]
    fn snapshot(&self) -> Vec<(u64, u64)> {
        self.ranges.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

fn merge_interval(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64, gap: u64) {
    let mut i = 0;
    while i < ranges.len() && ranges[i].1.saturating_add(gap) < start {
        i += 1;
    }
    let mut merged_start = start;
    let mut merged_end = end;
    while i < ranges.len() && ranges[i].0 <= merged_end.saturating_add(gap) {
        merged_start = merged_start.min(ranges[i].0);
        merged_end = merged_end.max(ranges[i].1);
        ranges.remove(i);
    }
    ranges.insert(i, (merged_start, merged_end));
}

fn total_bytes(ranges: &[(u64, u64)]) -> u64 {
    ranges.iter().map(|(s, e)| e.saturating_sub(*s)).sum()
}

fn trim_to_budget(
    ranges: &mut Vec<(u64, u64)>,
    new_start: u64,
    new_end: u64,
    max_bytes: u64,
    max_ranges: usize,
) {
    let anchor = new_start / 2 + new_end / 2;
    while ranges.len() > max_ranges || total_bytes(ranges) > max_bytes {
        if ranges.len() <= 1 {
            break;
        }
        let mut farthest_i = 0;
        let mut farthest_d = 0u64;
        for (i, &(s, e)) in ranges.iter().enumerate() {
            let mid = s / 2 + e / 2;
            let d = mid.abs_diff(anchor);
            if d >= farthest_d {
                farthest_d = d;
                farthest_i = i;
            }
        }
        // Never drop the segment that contains the new window center if unique.
        if ranges.len() > 1
            && ranges[farthest_i].0 <= anchor
            && anchor < ranges[farthest_i].1
            && ranges
                .iter()
                .filter(|&&(s, e)| s <= anchor && anchor < e)
                .count()
                == 1
        {
            // Pick next-farthest instead.
            let mut second = None;
            let mut second_d = 0u64;
            for (i, &(s, e)) in ranges.iter().enumerate() {
                if i == farthest_i {
                    continue;
                }
                let mid = s / 2 + e / 2;
                let d = mid.abs_diff(anchor);
                if second.is_none() || d >= second_d {
                    second_d = d;
                    second = Some(i);
                }
            }
            if let Some(i) = second {
                ranges.remove(i);
                continue;
            }
        }
        ranges.remove(farthest_i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_window_merges_nearby() {
        let m = FetchMap::new();
        m.note_window(0, 1_000_000);
        m.note_window(1_100_000, 2_100_000); // within MERGE_GAP of end? 100k < 256k
        assert_eq!(m.snapshot().len(), 1);
        assert_eq!(m.snapshot()[0], (0, 2_100_000));
    }

    #[test]
    fn note_window_keeps_recent_when_far() {
        let m = FetchMap::new();
        m.note_window(0, 8 * 1024 * 1024);
        m.note_window(30_000_000, 30_000_000 + 8 * 1024 * 1024);
        let snap = m.snapshot();
        assert!(!snap.is_empty());
        // Newest region must remain.
        assert!(
            snap.iter()
                .any(|&(s, e)| s <= 30_000_000 && 34_000_000 < e + 1)
        );
    }

    #[test]
    fn mark_full_spans_file() {
        let m = FetchMap::new();
        m.mark_full(100);
        assert_eq!(m.file_fractions(100), vec![(0.0, 1.0)]);
    }

    #[test]
    fn set_resident_replaces() {
        let m = FetchMap::new();
        m.note_window(0, 8 * 1024 * 1024);
        m.set_resident(10_000_000, 10_000_000 + 8 * 1024 * 1024);
        assert_eq!(m.snapshot().len(), 1);
        assert_eq!(m.snapshot()[0].0, 10_000_000);
    }
}
