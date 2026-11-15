//! Shared Exact settle law on the virtual timeline axis.
//!
//! Cut vs mid-clip must not fork here — both ask [`plan_exact_settle`] with
//! [`crate::virtual_timeline::ExactLedger::has_exact`]. Backend only decodes/presents.
//! **no equivalent** (scrub local).

/// What pause Exact settle must do at the aimed timeline unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactSettlePlan {
    /// Exact already in the VT ledger at aim — present, no key→N rebuild.
    PresentHit,
    /// Miss — decode Exact at aim, then import into the ledger.
    DecodeExact,
}

/// Single Exact-settle law for Single Video and Playlist (global aim).
///
/// `has_exact_at_aim` = [`super::ExactLedger::has_exact`] (not per-slot blit).
#[inline]
pub fn plan_exact_settle(has_exact_at_aim: bool) -> ExactSettlePlan {
    if has_exact_at_aim {
        ExactSettlePlan::PresentHit
    } else {
        ExactSettlePlan::DecodeExact
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_presents_miss_decodes() {
        assert_eq!(plan_exact_settle(true), ExactSettlePlan::PresentHit);
        assert_eq!(plan_exact_settle(false), ExactSettlePlan::DecodeExact);
    }
}
