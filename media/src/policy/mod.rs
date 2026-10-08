//! Pure clip / playback / workload policy (native-testable).
//!
//! - `clip` — per-clip policies, budgets and scrub pick math (`crate::policy::X`).
//! - [`playback`] — EOF / loop wall decisions.
//! - [`workload`] — Play / Scrub / Idle hints for background work.
//! - [`scrub_plan`] — densify gap / cold-commit planning.

mod clip;
pub mod playback;
pub mod scrub_plan;
pub mod workload;

pub use clip::*;
