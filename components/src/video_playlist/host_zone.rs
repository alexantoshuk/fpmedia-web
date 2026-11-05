//! Pure Host LoopZone wall helpers (native-testable).
//!
//! Callers live under `#[cfg(target_arch = "wasm32")]` playlist modules — on
//! native only the unit tests below exercise these symbols.

/// Soft op supersede (`MediaError::Cancelled` / epoch) — not a sticky seam failure.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn is_soft_seam_cancel(err: &str) -> bool {
    let e = err.trim();
    e == "operation cancelled" || e.eq_ignore_ascii_case("cancelled")
}

/// Who arms LoopZone wrap — mirror of wasm `loop_seam::LoopOwner` for tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) enum HostZoneOwner {
    Off,
    Engine,
    Host,
}

/// Host scrub-while-play / Play wall: aim is outside the half-open zone (parity
/// with playlist Effect / `play_fn`: `g < in` or `g ≥ out−1`). Full-axis zone
/// never wraps here. **no equivalent**.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn host_play_outside_zone(
    owner: HostZoneOwner,
    looping: bool,
    zone_in: u32,
    zone_out: u32,
    total_frames: u32,
    global: u32,
) -> bool {
    if !looping || !matches!(owner, HostZoneOwner::Host) {
        return false;
    }
    if zone_out <= zone_in {
        return false;
    }
    // Full axis — same as pre-zone last→0 / engine path; not Host outside-wrap.
    if zone_in == 0 && zone_out == total_frames {
        return false;
    }
    let zone_last = zone_out.saturating_sub(1);
    global < zone_in || global >= zone_last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_play_outside_zone_table() {
        let host = HostZoneOwner::Host;
        // Zone [100, 500) on a 1000-frame playlist.
        assert!(host_play_outside_zone(host, true, 100, 500, 1000, 50));
        assert!(host_play_outside_zone(host, true, 100, 500, 1000, 499));
        assert!(!host_play_outside_zone(host, true, 100, 500, 1000, 100));
        assert!(!host_play_outside_zone(host, true, 100, 500, 1000, 250));
        assert!(!host_play_outside_zone(host, true, 100, 500, 1000, 498));
        // Full axis / Engine / loop off — never.
        assert!(!host_play_outside_zone(host, true, 0, 1000, 1000, 0));
        assert!(!host_play_outside_zone(
            HostZoneOwner::Engine,
            true,
            100,
            500,
            1000,
            50
        ));
        assert!(!host_play_outside_zone(host, false, 100, 500, 1000, 50));
    }

    #[test]
    fn soft_seam_cancel_strings() {
        assert!(is_soft_seam_cancel("operation cancelled"));
        assert!(is_soft_seam_cancel("cancelled"));
        assert!(is_soft_seam_cancel("Cancelled"));
        assert!(!is_soft_seam_cancel("open timed out"));
        assert!(!is_soft_seam_cancel("LoopZone in out of range"));
    }
}
