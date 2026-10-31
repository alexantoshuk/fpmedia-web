//! One timeline meta strip — hides `pipeline` → paint from the host.
//!
//! Must sit under [`crate::Scrubber`] (reads [`crate::ScrubberCtx`]). Spec:
//! `.ai/product.md` § Element-layer helpers.
//!
//! Strip domain + live remap live on [`crate::TimelineRef`] (`strip_video` /
//! `strip_ready_marks` / `strip_fetch_ranges`). Meta re-leases pipeline interest
//! when [`TimelineRef::strip_video_key`] changes — no Scrubber remount, no dual-lease.

use leptos::html::Canvas;
use leptos::prelude::*;
use web_sys::window;

use crate::PipelineInterestLease;
use crate::PipelinePlane;
use crate::ReadyFrameKind;
use crate::helpers::{
    COVERED_STRIP_COLOR, EXACT_STRIP_COLOR, FETCH_STRIP_COLOR, KEYFRAME_STRIP_COLOR,
    StripPaintLatch, cache_strip_runs_for_width, fetch_strip_runs_for_width, paint_strip_canvas,
    strip_canvas_buffer_width,
};
use crate::scrubber::ScrubberCtx;
use crate::timeline::StripVideoKey;

/// Which residency / fetch plane to paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelineMetaKind {
    /// Ever-fetched byte ranges (`FetchMap` fractions).
    Fetch,
    /// Sync / keyframe positions (structural ticks).
    Keyframes,
    /// Densify markers (Covered).
    Covered,
    /// Step-ready Exact blit frames.
    Exact,
}

impl TimelineMetaKind {
    fn default_color(self) -> &'static str {
        match self {
            Self::Fetch => FETCH_STRIP_COLOR,
            Self::Keyframes => KEYFRAME_STRIP_COLOR,
            Self::Covered => COVERED_STRIP_COLOR,
            Self::Exact => EXACT_STRIP_COLOR,
        }
    }

    fn pipeline_plane(self) -> PipelinePlane {
        match self {
            Self::Fetch => PipelinePlane::Fetch,
            Self::Keyframes | Self::Covered | Self::Exact => PipelinePlane::Ready,
        }
    }
}

/// Single 1 px-tall canvas strip (float `f/n` runs); CSS-stretch via host `class`.
#[component]
pub fn TimelineMeta(
    kind: TimelineMetaKind,
    #[prop(optional)] class: Option<String>,
    #[prop(optional)] color: Option<String>,
    /// Optional `data-testid` (demo: `scrubber-fetched` for Fetch).
    #[prop(optional)]
    test_id: Option<&'static str>,
) -> impl IntoView {
    let ctx = use_context::<ScrubberCtx>().expect("TimelineMeta must be used inside <Scrubber>");
    let timeline = ctx.timeline;
    let css_width = ctx.css_width;
    let canvas_ref = NodeRef::<Canvas>::new();
    let latch = StoredValue::new_local(StripPaintLatch::new());
    let paint_color = color.unwrap_or_else(|| kind.default_color().to_string());
    let root_class = class.unwrap_or_default();
    let plane = kind.pipeline_plane();

    // Re-lease only when strip video identity changes (playlist live swap).
    let interest = StoredValue::new_local(None::<PipelineInterestLease>);
    let interest_key = StoredValue::new_local(None::<StripVideoKey>);
    on_cleanup(move || {
        interest.set_value(None);
        interest_key.set_value(None);
    });

    Effect::new(move |_| {
        let css_w = css_width.get();
        let dpr = window()
            .map(|w| w.device_pixel_ratio())
            .unwrap_or(1.0)
            .max(1.0);
        let buf_w = strip_canvas_buffer_width(css_w, dpr);
        let width = f64::from(buf_w);

        // Playlist probe open→close would paint then wipe Fetch — hold strips until ready.
        // **no equivalent** (playlist probe / strip host).
        if !timeline.ready.get() {
            interest.set_value(None);
            interest_key.set_value(None);
            latch.update_value(|l| l.clear());
            if let Some(canvas) = canvas_ref.get() {
                let _ = paint_strip_canvas(&canvas, buf_w, &[], &paint_color);
            }
            return;
        }

        let Some(key) = timeline.strip_video_key() else {
            interest.set_value(None);
            interest_key.set_value(None);
            return;
        };
        let Some(video) = timeline.strip_video() else {
            interest.set_value(None);
            interest_key.set_value(None);
            return;
        };
        if interest_key.get_value() != Some(key) {
            interest.set_value(Some(video.lease_pipeline_plane(plane)));
            interest_key.set_value(Some(key));
        }

        // Track pipeline / domain even when canvas NodeRef is not ready yet.
        let pipe = video.pipeline.get();
        let frame_count = timeline.strip_domain_len();
        let marks = pipe
            .as_ref()
            .map(|s| s.ready_frames.as_slice())
            .unwrap_or(&[]);
        let ready_marks = timeline.strip_ready_marks(marks);
        let raw_fetch = pipe
            .as_ref()
            .map(|s| s.fetch_file_ranges.as_slice())
            .unwrap_or(&[]);
        let fetch_ranges = timeline.strip_fetch_ranges(raw_fetch);

        let Some(canvas) = canvas_ref.get() else {
            return;
        };
        let cand = match kind {
            TimelineMetaKind::Fetch => fetch_strip_runs_for_width(&fetch_ranges, width),
            TimelineMetaKind::Keyframes => cache_strip_runs_for_width(
                &ready_marks,
                ReadyFrameKind::Keyframes,
                frame_count,
                width,
            ),
            TimelineMetaKind::Covered => cache_strip_runs_for_width(
                &ready_marks,
                ReadyFrameKind::Covered,
                frame_count,
                width,
            ),
            TimelineMetaKind::Exact => {
                cache_strip_runs_for_width(&ready_marks, ReadyFrameKind::Exact, frame_count, width)
            }
        };
        let mut runs = Vec::new();
        latch.update_value(|l| {
            runs = l.resolve(&cand, buf_w).to_vec();
        });
        let _ = paint_strip_canvas(&canvas, buf_w, &runs, &paint_color);
    });

    let test_id = test_id.unwrap_or("");

    view! {
        <canvas
            class=root_class
            node_ref=canvas_ref
            data-testid=test_id
            aria-hidden="true"
        />
    }
}
