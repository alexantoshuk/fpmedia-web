/// Matches `.wm-scrubber-track` background (`#2a2a2a`). Zebra even/odd =
/// this ± `contrast` per channel (demo chrome).
pub(super) const SCRUBBER_TRACK_BASE: u8 = 0x2a;

pub(super) fn gray_hex(v: u8) -> String {
    format!("#{v:02x}{v:02x}{v:02x}")
}

/// Even / odd zebra colors from track base ± `contrast` (saturating).
pub(super) fn zebra_pair_hex(contrast: u8) -> (String, String) {
    (
        gray_hex(SCRUBBER_TRACK_BASE.saturating_add(contrast)),
        gray_hex(SCRUBBER_TRACK_BASE.saturating_sub(contrast)),
    )
}

/// Forward shuttle ladder for demo J/L keys (K → 1× + play/pause).
pub(super) const JKL_STEPS: [f64; 4] = [1.0, 2.0, 4.0, 8.0];

/// Review-rate tick marks in the rate popover (0.25…1.75).
pub(super) const RATE_TICKS: [f64; 7] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75];

pub(super) const RATE_SLIDER_MIN: f64 = 0.25;

pub(super) const RATE_SLIDER_MAX: f64 = 1.75;

pub(super) fn format_rate_tick(r: f64) -> String {
    if (r - r.round()).abs() < 1e-9 {
        format!("{}", r.round() as i32)
    } else {
        format!("{r}")
    }
}

/// Near-1× — same epsilon as active rate tick highlight.
pub(super) fn rate_is_unity(r: f64) -> bool {
    (r - 1.0).abs() < 0.026
}

/// Compact fps for rate chrome (`24fps`, `23.9fps`; ≤1 fractional digit).
/// Truncate (not round) — `23.976` → `23.9`, never `24`.
pub(super) fn format_fps_label(fps: f64) -> String {
    let truncated = (fps * 10.0).trunc() / 10.0;
    if (truncated - truncated.trunc()).abs() < 1e-9 {
        format!("{}fps", truncated.trunc() as i32)
    } else {
        format!("{truncated:.1}fps")
    }
}

/// Effective fps in the rate pop heading — round to ≤3 fractional digits.
pub(super) fn format_fps_label_precise(fps: f64) -> String {
    let rounded = (fps * 1000.0).round() / 1000.0;
    if (rounded - rounded.trunc()).abs() < 1e-9 {
        format!("{} fps", rounded.trunc() as i32)
    } else {
        let s = format!("{rounded:.3}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        format!("{s} fps")
    }
}

pub(super) fn rate_tick_left_pct(r: f64) -> String {
    let span = RATE_SLIDER_MAX - RATE_SLIDER_MIN;
    let p = ((r - RATE_SLIDER_MIN) / span * 100.0).clamp(0.0, 100.0);
    format!("{p}%")
}

pub(super) fn jkl_nearest_index(rate: f64) -> usize {
    let mut best = 0;
    let mut best_dist = f64::INFINITY;
    for (i, &step) in JKL_STEPS.iter().enumerate() {
        let d = (rate - step).abs();
        if d < best_dist {
            best_dist = d;
            best = i;
        }
    }
    best
}

/// `dir`: −1 = J (slower), +1 = L (faster).
pub(super) fn jkl_step(rate: f64, dir: i32) -> f64 {
    let i = jkl_nearest_index(rate);
    let next = (i as i32 + dir).clamp(0, (JKL_STEPS.len() - 1) as i32) as usize;
    JKL_STEPS[next]
}
