//! Resident file-byte coverage for the demo scrubber gray strip.
//!
//! Mirrored from [`super::CachedSource`] LRU windows (not a historical union).
//! File offset → timeline % (not sample-accurate). mediabunny: no equivalent meter.

use std::sync::Mutex;

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

    /// Replace coverage with the exact resident cache windows (LRU slots).
    pub fn set_windows(&self, windows: &[(u64, u64)]) {
        let Ok(mut ranges) = self.ranges.lock() else {
            return;
        };
        ranges.clear();
        for &(start, end) in windows {
            if end <= start {
                continue;
            }
            merge_touching(&mut ranges, start, end);
        }
    }

    /// Note a new cache fill (tests / incremental). Prefer [`Self::set_windows`]
    /// when the caller already knows the full resident set.
    pub fn note_window(&self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let Ok(mut ranges) = self.ranges.lock() else {
            return;
        };
        merge_touching(&mut ranges, start, end);
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

/// Merge only overlapping / abutting intervals (no soft gap).
fn merge_touching(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
    let mut i = 0;
    while i < ranges.len() && ranges[i].1 < start {
        i += 1;
    }
    let mut merged_start = start;
    let mut merged_end = end;
    while i < ranges.len() && ranges[i].0 <= merged_end {
        merged_start = merged_start.min(ranges[i].0);
        merged_end = merged_end.max(ranges[i].1);
        ranges.remove(i);
    }
    ranges.insert(i, (merged_start, merged_end));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_window_merges_overlap() {
        let m = FetchMap::new();
        m.note_window(0, 1_000_000);
        m.note_window(900_000, 2_000_000);
        assert_eq!(m.snapshot(), vec![(0, 2_000_000)]);
    }

    #[test]
    fn note_window_keeps_disjoint() {
        let m = FetchMap::new();
        m.note_window(0, 8 * 1024 * 1024);
        m.note_window(30_000_000, 30_000_000 + 8 * 1024 * 1024);
        assert_eq!(m.snapshot().len(), 2);
    }

    #[test]
    fn set_windows_replaces() {
        let m = FetchMap::new();
        m.note_window(0, 8 * 1024 * 1024);
        m.set_windows(&[(10_000_000, 10_000_000 + 4 * 1024 * 1024)]);
        assert_eq!(m.snapshot().len(), 1);
        assert_eq!(m.snapshot()[0].0, 10_000_000);
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
