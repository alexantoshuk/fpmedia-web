//! Refcounted host interest in [`fpmedia_web::PipelineStats`] planes.
//!
//! `<TimelineMeta>` / debug UI acquire a lease on mount; [`Drop`] releases.
//! `sync_ui` only asks the engine for planes with `rc > 0`.

use std::cell::Cell;

use fpmedia_web::PipelineInterest;
use leptos::prelude::*;

use super::VideoRef;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelinePlane {
    Fetch,
    Ready,
    Meters,
}

#[derive(Default)]
pub(super) struct InterestCounts {
    fetch: Cell<u32>,
    ready: Cell<u32>,
    meters: Cell<u32>,
}

impl InterestCounts {
    fn bump(&self, plane: PipelinePlane, delta: i32) {
        let cell = match plane {
            PipelinePlane::Fetch => &self.fetch,
            PipelinePlane::Ready => &self.ready,
            PipelinePlane::Meters => &self.meters,
        };
        let cur = cell.get();
        let next = if delta >= 0 {
            cur.saturating_add(delta as u32)
        } else {
            cur.saturating_sub((-delta) as u32)
        };
        cell.set(next);
    }

    fn snapshot(&self) -> PipelineInterest {
        PipelineInterest {
            fetch: self.fetch.get() > 0,
            ready: self.ready.get() > 0,
            meters: self.meters.get() > 0,
        }
    }
}

/// RAII interest in one pipeline plane. Drop when the consumer unmounts.
pub struct PipelineInterestLease {
    video: VideoRef,
    plane: PipelinePlane,
}

impl Drop for PipelineInterestLease {
    fn drop(&mut self) {
        self.video.release_pipeline_plane(self.plane);
    }
}

impl VideoRef {
    /// Current planes with at least one live lease.
    pub fn pipeline_interest(&self) -> PipelineInterest {
        self.pipeline_interest_counts
            .try_with_value(|c| c.snapshot())
            .unwrap_or_default()
    }

    /// Acquire interest until the returned lease is dropped (component unmount).
    pub fn lease_pipeline_plane(&self, plane: PipelinePlane) -> PipelineInterestLease {
        self.acquire_pipeline_plane(plane);
        PipelineInterestLease {
            video: *self,
            plane,
        }
    }

    fn acquire_pipeline_plane(&self, plane: PipelinePlane) {
        self.pipeline_interest_counts
            .update_value(|c| c.bump(plane, 1));
    }

    fn release_pipeline_plane(&self, plane: PipelinePlane) {
        self.pipeline_interest_counts
            .update_value(|c| c.bump(plane, -1));
    }
}
