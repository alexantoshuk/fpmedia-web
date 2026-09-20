//! Union of fetched file byte ranges for demo buffer UI (not decode cache).
//!
//! Local invention — HTML `<video>.buffered` is time-based; we map **file bytes →
//! timeline %** for speed (not sample-accurate). mediabunny: no equivalent meter.

use std::sync::Mutex;

/// Half-open `[start, end)` byte intervals, merged, file-absolute.
#[derive(Debug, Default)]
pub struct FetchMap {
    ranges: Mutex<Vec<(u64, u64)>>,
}

impl FetchMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the entire file present (in-memory / full-download opens).
    pub fn mark_full(&self, size: u64) {
        if size == 0 {
            return;
        }
        self.record(0, size as usize);
    }

    /// Record a successful read (or prefetch fill).
    pub fn record(&self, offset: u64, len: usize) {
        if len == 0 {
            return;
        }
        let start = offset;
        let end = offset.saturating_add(len as u64);
        if end <= start {
            return;
        }
        let Ok(mut ranges) = self.ranges.lock() else {
            return;
        };
        merge_interval(&mut ranges, start, end);
    }

    /// Merged ranges as fractions of `file_size` in `0.0..1.0` (for CSS gradients).
    ///
    /// Caps at [`MAX_UI_RANGES`] by coarsening (keeps UI cheap on fMP4 splatters).
    pub fn file_fractions(&self, file_size: u64) -> Vec<(f32, f32)> {
        if file_size == 0 {
            return Vec::new();
        }
        let Ok(ranges) = self.ranges.lock() else {
            return Vec::new();
        };
        let mut out: Vec<(f32, f32)> = ranges
            .iter()
            .filter_map(|&(s, e)| {
                if e <= s {
                    return None;
                }
                let a = (s as f64 / file_size as f64) as f32;
                let b = (e as f64 / file_size as f64) as f32;
                Some((a.clamp(0.0, 1.0), b.clamp(0.0, 1.0).max(a)))
            })
            .collect();
        if out.len() > MAX_UI_RANGES {
            out = coarsen_fractions(out, MAX_UI_RANGES);
        }
        out
    }

    #[cfg(test)]
    fn snapshot(&self) -> Vec<(u64, u64)> {
        self.ranges.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

const MAX_UI_RANGES: usize = 48;

fn merge_interval(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
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

fn coarsen_fractions(ranges: Vec<(f32, f32)>, max: usize) -> Vec<(f32, f32)> {
    if ranges.len() <= max || max == 0 {
        return ranges;
    }
    // Bucket into `max` bins across 0..1 and emit occupied bins.
    let mut bins = vec![false; max];
    for &(a, b) in &ranges {
        let lo = ((a as f64) * max as f64).floor() as usize;
        let hi = ((b as f64) * max as f64).ceil() as usize;
        let start = lo.min(max);
        let end = hi.min(max).max(start.saturating_add(1));
        for bin in bins.iter_mut().take(end).skip(start) {
            *bin = true;
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < max {
        if !bins[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < max && bins[i] {
            i += 1;
        }
        out.push((start as f32 / max as f32, i as f32 / max as f32));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlapping_and_adjacent() {
        let m = FetchMap::new();
        m.record(100, 50);
        m.record(140, 20);
        m.record(200, 10);
        assert_eq!(m.snapshot(), vec![(100, 160), (200, 210)]);
    }

    #[test]
    fn fractions_span_file() {
        let m = FetchMap::new();
        m.record(0, 50);
        m.record(75, 25);
        let f = m.file_fractions(100);
        assert_eq!(f.len(), 2);
        assert!((f[0].0 - 0.0).abs() < 1e-5);
        assert!((f[0].1 - 0.5).abs() < 1e-5);
        assert!((f[1].0 - 0.75).abs() < 1e-5);
        assert!((f[1].1 - 1.0).abs() < 1e-5);
    }
}
