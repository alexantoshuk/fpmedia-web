//! Coalesce adjacent byte-range needs into fewer Range GETs.
//!
//! Pattern: mediabunny `src/source.ts` `ReadOrchestrator` merges overlapping /
//! adjacent holes into one worker fill (`queuedReads` hole extend + splice).
//! Used by progressive `ftyp`/`moov`/`moof` loads in [`super::boxes`] and by
//! playback sample prefetch / warm (`mdat` spans).

/// Merge adjacent load ranges if the gap is at most this many bytes (fMP4 moof+mdat hdr).
pub(crate) const COALESCE_GAP: u64 = 256;
/// Cap a coalesced Range GET when packing across `mdat` gaps (full-file walk path).
pub(crate) const MAX_COALESCE_RUN: usize = 1024 * 1024;
/// Interleaved A/V inside one `mdat`: merge sample byte ranges when the next
/// sample starts within this many bytes of the previous end. Larger than
/// [`COALESCE_GAP`] so a few AAC frames between video NALs stay one span;
/// still well below a typical moof gap so fragment seams stay split.
pub(crate) const SAMPLE_COALESCE_GAP: u64 = 4 * 1024;

pub(crate) type BoxLoadNeed = (usize, u64, usize);
pub(crate) type CoalesceRun = (u64, usize, Vec<BoxLoadNeed>);

/// Merge only near-adjacent needs ([`COALESCE_GAP`]) — no packing across large
/// `mdat` (used by `mfra` moof body load).
pub(crate) fn coalesce_byte_runs_adjacent(needs: &[BoxLoadNeed]) -> Vec<CoalesceRun> {
    let mut sorted = needs.to_vec();
    sorted.sort_by_key(|(_, off, _)| *off);
    let mut runs: Vec<CoalesceRun> = Vec::new();
    for &(idx, off, len) in &sorted {
        if len == 0 {
            continue;
        }
        let need_end = off.saturating_add(len as u64);
        if let Some((run_off, run_len, members)) = runs.last_mut() {
            let run_end = *run_off + *run_len as u64;
            if off <= run_end.saturating_add(COALESCE_GAP) {
                *run_len = (need_end.max(run_end) - *run_off) as usize;
                members.push((idx, off, len));
                continue;
            }
        }
        runs.push((off, len, vec![(idx, off, len)]));
    }
    runs
}

/// Merge sorted `(box_idx, offset, len)` into fewer Range GETs.
///
/// Adjacent / near needs merge via [`COALESCE_GAP`]. On fMP4 full-file walk,
/// also pack into ≤[`MAX_COALESCE_RUN`] windows so one RTT covers several
/// fragments (extra media bytes are read then discarded when slicing).
pub(crate) fn coalesce_byte_runs(needs: &[BoxLoadNeed]) -> Vec<CoalesceRun> {
    let mut sorted = needs.to_vec();
    sorted.sort_by_key(|(_, off, _)| *off);
    let mut runs: Vec<CoalesceRun> = Vec::new();
    for &(idx, off, len) in &sorted {
        if len == 0 {
            continue;
        }
        let need_end = off.saturating_add(len as u64);
        if let Some((run_off, run_len, members)) = runs.last_mut() {
            let run_end = *run_off + *run_len as u64;
            let gap_ok = off <= run_end.saturating_add(COALESCE_GAP);
            let packed_end = need_end.max(run_end);
            let pack_ok = packed_end.saturating_sub(*run_off) as usize <= MAX_COALESCE_RUN
                && off < run_off.saturating_add(MAX_COALESCE_RUN as u64);
            if gap_ok || pack_ok {
                *run_len = (packed_end - *run_off) as usize;
                members.push((idx, off, len));
                continue;
            }
        }
        runs.push((off, len, vec![(idx, off, len)]));
    }
    runs
}

/// Coalesce sample `(offset, size)` pairs into `[start, end)` byte spans.
///
/// Merges when the next sample starts within `gap` of the previous end
/// (`mediabunny: ReadOrchestrator` hole extend). Does **not** pack across large
/// holes (next `moof`/`mdat`) — a single min..max span left early tiles cold on
/// fMP4 prefetch.
pub(crate) fn coalesce_sample_spans(
    samples: impl IntoIterator<Item = (u64, u64)>,
    gap: u64,
) -> Vec<(u64, u64)> {
    let mut spans: Vec<(u64, u64)> = Vec::new();
    let mut run_start = u64::MAX;
    let mut run_end = 0u64;
    let mut prev_end = None::<u64>;
    for (offset, size) in samples {
        if size == 0 {
            continue;
        }
        let e_end = offset.saturating_add(size);
        match prev_end {
            Some(pe) if offset > pe.saturating_add(gap) => {
                if run_start != u64::MAX && run_end > run_start {
                    spans.push((run_start, run_end));
                }
                run_start = offset;
                run_end = e_end;
            }
            Some(_) => {
                run_end = run_end.max(e_end);
            }
            None => {
                run_start = offset;
                run_end = e_end;
            }
        }
        prev_end = Some(e_end);
    }
    if run_start != u64::MAX && run_end > run_start {
        spans.push((run_start, run_end));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_within_gap_merges() {
        let needs = vec![(0usize, 0u64, 100usize), (1, 150, 50)]; // gap 50 < 256
        let runs = coalesce_byte_runs_adjacent(&needs);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, 0);
        assert_eq!(runs[0].1, 200);
        assert_eq!(runs[0].2.len(), 2);
    }

    #[test]
    fn adjacent_over_gap_splits() {
        let needs = vec![(0usize, 0u64, 100usize), (1, 400, 50)]; // gap 300 > 256
        let runs = coalesce_byte_runs_adjacent(&needs);
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn pack_across_gap_under_max_run() {
        let needs = vec![(0usize, 0u64, 400usize), (1, 400, 8), (2, 200_000, 400)];
        let runs = coalesce_byte_runs(&needs);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, 0);
        assert_eq!(runs[0].1, 200_400);
        assert_eq!(runs[0].2.len(), 3);
    }

    #[test]
    fn split_when_over_max_run() {
        let needs = vec![
            (0usize, 0u64, 100usize),
            (1, (MAX_COALESCE_RUN as u64) + 10, 100),
        ];
        let runs = coalesce_byte_runs(&needs);
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn skips_zero_len() {
        let needs = vec![(0usize, 0u64, 0usize), (1, 0, 50)];
        let runs = coalesce_byte_runs(&needs);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].1, 50);
        assert_eq!(runs[0].2.len(), 1);
    }

    #[test]
    fn sample_spans_merge_interleaved_av() {
        // Video NAL, small AAC gap, next video — one mdat span.
        let spans = coalesce_sample_spans(
            [(1000u64, 500u64), (1600, 200), (1900, 400)],
            SAMPLE_COALESCE_GAP,
        );
        assert_eq!(spans, vec![(1000, 2300)]);
    }

    #[test]
    fn sample_spans_split_on_moof_gap() {
        let spans = coalesce_sample_spans(
            [
                (1000u64, 500u64),
                (1000 + 500 + SAMPLE_COALESCE_GAP + 1, 400),
            ],
            SAMPLE_COALESCE_GAP,
        );
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn sample_spans_skips_zero_size() {
        let spans = coalesce_sample_spans([(0u64, 0u64), (10, 20)], SAMPLE_COALESCE_GAP);
        assert_eq!(spans, vec![(10, 30)]);
    }
}
