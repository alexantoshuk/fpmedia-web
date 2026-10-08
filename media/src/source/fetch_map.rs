//! Resident file-byte coverage for the demo scrubber gray strip.
//!
//! **Url + Blob/Memory:** accumulate fetched ranges (`note_window`) — gray =
//! ever-fetched this session, not the current WASM LRU (URL eviction must not
//! punch permanent holes). Blob quantum expand is prefetch-only (densify/scrub
//! — not per-sample); Blob is already sliceable in JS.
//! File offset → timeline % (not sample-accurate). mediabunny: no equivalent meter.
//!
//! **Display soft-merge:** [`Self::file_fractions`] closes gaps ≤
//! [`display_merge_gap`] so (1) fMP4 `moof` (~1 KiB) between resident `mdat`
//! islands and (2) sub‑pixel unread holes on large files after scrub LRU
//! fragmentation do not paint a 1‑px “blurry hole”. [`Self::windows`] /
//! [`Self::interior_gaps`] stay exact (diagnostics / tests).

use std::sync::Mutex;

/// Floor for gray-strip soft-merge (fMP4 `moof` between `mdat` is typically
/// ≤2 KiB; 8 KiB leaves headroom without swallowing real unread islands on
/// small files).
pub const FETCH_MAP_DISPLAY_MERGE_GAP: u64 = 8 * 1024;

/// Assumed scrubber CSS width when scaling soft-merge to ~1 px of `file_size`
/// (engine has no layout width; demo track is ~full panel).
const ASSUMED_SCRUBBER_PX: u64 = 800;

/// Paint-only merge threshold: at least [`FETCH_MAP_DISPLAY_MERGE_GAP`], else
/// ~**two** scrubber pixels of `file_size` so sub‑pixel holes stay closed and
/// 1‑px-threshold flicker (merge↔split every rAF) does not swim the gray edge.
pub fn display_merge_gap(file_size: u64) -> u64 {
    if file_size == 0 {
        return FETCH_MAP_DISPLAY_MERGE_GAP;
    }
    let pixel = (file_size / ASSUMED_SCRUBBER_PX).max(1);
    FETCH_MAP_DISPLAY_MERGE_GAP.max(pixel.saturating_mul(2))
}

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

    /// Replace coverage with the exact resident cache windows.
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

    /// Resident half-open windows `[start, end)` (file-absolute), merged hard.
    pub fn windows(&self) -> Vec<(u64, u64)> {
        self.ranges.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// True if `[start, end)` is fully inside some resident window.
    pub fn covers_range(&self, start: u64, end: u64) -> bool {
        if end <= start {
            return true;
        }
        let Ok(ranges) = self.ranges.lock() else {
            return false;
        };
        ranges.iter().any(|&(s, e)| s <= start && end <= e)
    }

    /// Gaps between resident windows inside `[0, file_size)` (half-open).
    pub fn gaps(&self, file_size: u64) -> Vec<(u64, u64)> {
        if file_size == 0 {
            return Vec::new();
        }
        let Ok(ranges) = self.ranges.lock() else {
            return Vec::new();
        };
        let mut gaps = Vec::new();
        let mut cursor = 0u64;
        for &(s, e) in ranges.iter() {
            if s > cursor {
                gaps.push((cursor, s.min(file_size)));
            }
            cursor = cursor.max(e);
            if cursor >= file_size {
                break;
            }
        }
        if cursor < file_size {
            gaps.push((cursor, file_size));
        }
        gaps
    }

    /// Gaps strictly between consecutive resident windows (not unread head/tail).
    pub fn interior_gaps(&self) -> Vec<(u64, u64)> {
        let windows = self.windows();
        let mut gaps = Vec::new();
        for pair in windows.windows(2) {
            let end0 = pair[0].1;
            let start1 = pair[1].0;
            if start1 > end0 {
                gaps.push((end0, start1));
            }
        }
        gaps
    }

    /// Coverage as fractions of `file_size` in `0.0..1.0` for the scrubber.
    /// Soft-merges holes ≤ [`display_merge_gap`], then snaps to an integer
    /// scrubber pixel so f32 noise / sub‑byte edges do not jitter the CSS stops.
    pub fn file_fractions(&self, file_size: u64) -> Vec<(f32, f32)> {
        if file_size == 0 {
            return Vec::new();
        }
        let Ok(ranges) = self.ranges.lock() else {
            return Vec::new();
        };
        let merged = soft_merge_windows(&ranges, display_merge_gap(file_size));
        let pixel = (file_size / ASSUMED_SCRUBBER_PX).max(1);
        let snap = |off: u64, round_up: bool| -> u64 {
            if round_up {
                let q = off.div_ceil(pixel);
                q.saturating_mul(pixel).min(file_size)
            } else {
                (off / pixel).saturating_mul(pixel)
            }
        };
        let mut out: Vec<(f32, f32)> = Vec::new();
        for &(s, e) in &merged {
            if e <= s {
                continue;
            }
            let s = snap(s, false);
            let e = snap(e, true).max(s.saturating_add(pixel).min(file_size));
            if e <= s {
                continue;
            }
            let a = s as f64 / file_size as f64;
            let b = e as f64 / file_size as f64;
            let a = ((a * ASSUMED_SCRUBBER_PX as f64).round() / ASSUMED_SCRUBBER_PX as f64)
                .clamp(0.0, 1.0);
            let b = ((b * ASSUMED_SCRUBBER_PX as f64).round() / ASSUMED_SCRUBBER_PX as f64)
                .clamp(0.0, 1.0)
                .max(a);
            // Coalesce after snap (adjacent pixel bands → one stop pair).
            if let Some((_, prev_b)) = out.last_mut()
                && a <= f64::from(*prev_b) + 0.5 / ASSUMED_SCRUBBER_PX as f64
            {
                *prev_b = (b as f32).max(*prev_b);
                continue;
            }
            out.push((a as f32, b as f32));
        }
        out
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

/// Paint-time merge: close holes of at most `max_gap` bytes.
fn soft_merge_windows(windows: &[(u64, u64)], max_gap: u64) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for &(start, end) in windows {
        if end <= start {
            continue;
        }
        if let Some((_, prev_end)) = out.last_mut()
            && start <= prev_end.saturating_add(max_gap)
        {
            *prev_end = (*prev_end).max(end);
            continue;
        }
        out.push((start, end));
    }
    out
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
    fn gaps_between_windows() {
        let m = FetchMap::new();
        m.set_windows(&[(0, 100), (150, 200)]);
        assert_eq!(m.gaps(250), vec![(100, 150), (200, 250)]);
        assert_eq!(m.interior_gaps(), vec![(100, 150)]);
    }

    #[test]
    fn gaps_empty_when_full() {
        let m = FetchMap::new();
        m.mark_full(100);
        assert!(m.gaps(100).is_empty());
        assert!(m.interior_gaps().is_empty());
    }

    #[test]
    fn set_resident_replaces() {
        let m = FetchMap::new();
        m.note_window(0, 8 * 1024 * 1024);
        m.set_resident(10_000_000, 10_000_000 + 8 * 1024 * 1024);
        assert_eq!(m.snapshot().len(), 1);
        assert_eq!(m.snapshot()[0].0, 10_000_000);
    }

    #[test]
    fn file_fractions_soft_merges_moof_sized_gap() {
        let m = FetchMap::new();
        // Two mdat islands with a ~1.5 KiB moof between them.
        m.set_windows(&[(0, 1_000_000), (1_000_000 + 1500, 2_000_000)]);
        assert_eq!(m.interior_gaps(), vec![(1_000_000, 1_001_500)]);
        let fracs = m.file_fractions(2_000_000);
        assert_eq!(fracs.len(), 1, "moof gap must not paint a scrubber hole");
        assert!((fracs[0].0 - 0.0).abs() < 1e-6);
        assert!((fracs[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn file_fractions_keeps_real_unread_island() {
        let m = FetchMap::new();
        // 2 MiB file → pixel gap = 2 MiB/800 = 2.5 KiB; floor stays 8 KiB.
        // 64 KiB hole > display merge → still paints as a hole.
        m.set_windows(&[(0, 1_000_000), (1_000_000 + 64 * 1024, 2_000_000)]);
        assert_eq!(m.file_fractions(2_000_000).len(), 2);
    }

    #[test]
    fn file_fractions_soft_merges_subpixel_gap_on_large_file() {
        // ~725 MiB progressive Blob (BBB-class): 1 px ≈ 900 KiB.
        let file_size = 725 * 1024 * 1024u64;
        let pixel = display_merge_gap(file_size);
        assert!(pixel > FETCH_MAP_DISPLAY_MERGE_GAP);
        let gap = pixel / 2; // larger than moof floor, still < 1 px
        let mid = file_size / 2;
        let m = FetchMap::new();
        m.set_windows(&[(0, mid), (mid + gap, file_size)]);
        assert_eq!(m.interior_gaps(), vec![(mid, mid + gap)]);
        assert_eq!(
            m.file_fractions(file_size).len(),
            1,
            "sub-pixel scrub-LRU gap must not paint a blurry hole"
        );
    }

    #[test]
    fn display_merge_gap_scales_with_file() {
        assert_eq!(display_merge_gap(0), FETCH_MAP_DISPLAY_MERGE_GAP);
        assert_eq!(display_merge_gap(800_000), FETCH_MAP_DISPLAY_MERGE_GAP); // 1 KiB < floor
        assert_eq!(display_merge_gap(800 * 1024 * 1024), 2 * 1024 * 1024); // 2 px × 1 MiB/px
    }

    #[test]
    fn covers_range_inside_window() {
        let m = FetchMap::new();
        m.set_windows(&[(100, 200)]);
        assert!(m.covers_range(100, 150));
        assert!(m.covers_range(150, 200));
        assert!(!m.covers_range(90, 110));
        assert!(!m.covers_range(190, 210));
    }

    #[test]
    fn soft_merge_windows_unit() {
        let w = soft_merge_windows(&[(0, 100), (108, 200), (10_000, 11_000)], 8);
        assert_eq!(w, vec![(0, 200), (10_000, 11_000)]);
    }
}
