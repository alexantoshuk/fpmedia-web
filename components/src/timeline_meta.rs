//! One timeline meta strip — hides `pipeline` → paint from the host.
//!
//! Must sit under [`crate::Scrubber`] (reads [`crate::ScrubberCtx`]). Spec:
//! `.ai/product.md` § Element-layer helpers.

use leptos::html::Canvas;
use leptos::prelude::*;
use web_sys::window;

use crate::PipelinePlane;
use crate::ReadyFrameKind;
use crate::helpers::{
    COVERED_STRIP_COLOR, EXACT_STRIP_COLOR, FETCH_STRIP_COLOR, KEYFRAME_STRIP_COLOR,
    StripPaintLatch, cache_strip_runs_for_width, fetch_strip_runs_for_width, paint_strip_canvas,
    strip_canvas_buffer_width,
};
use crate::scrubber::ScrubberCtx;

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
    let video = ctx
        .video
        .expect("TimelineMeta requires <Scrubber video=…> (pipeline strips)");
    let css_width = ctx.css_width;
    let canvas_ref = NodeRef::<Canvas>::new();
    let latch = StoredValue::new_local(StripPaintLatch::new());
    let paint_color = color.unwrap_or_else(|| kind.default_color().to_string());
    let root_class = class.unwrap_or_default();

    // Compose → interest: only this plane is filled in `pipeline_stats`.
    let interest_lease = video.lease_pipeline_plane(kind.pipeline_plane());
    on_cleanup(move || drop(interest_lease));

    Effect::new(move |_| {
        // Track pipeline / count even when the canvas NodeRef is not ready yet —
        // early-return before these reads left strips stuck until a resize.
        let pipe = video.pipeline.get();
        // Unified `frame_count` grid with tip / zebra / scrub hit-test.
        let frame_count = video.frame_count.get().max(1) as f64;
        let css_w = css_width.get();
        let dpr = window()
            .map(|w| w.device_pixel_ratio())
            .unwrap_or(1.0)
            .max(1.0);
        let buf_w = strip_canvas_buffer_width(css_w, dpr);
        let width = f64::from(buf_w);
        let Some(canvas) = canvas_ref.get() else {
            return;
        };
        let cand = match kind {
            TimelineMetaKind::Fetch => {
                let ranges = pipe
                    .as_ref()
                    .map(|s| s.fetch_file_ranges.as_slice())
                    .unwrap_or(&[]);
                fetch_strip_runs_for_width(ranges, width)
            }
            TimelineMetaKind::Keyframes => {
                let marks = pipe
                    .as_ref()
                    .map(|s| s.ready_frames.as_slice())
                    .unwrap_or(&[]);
                cache_strip_runs_for_width(marks, ReadyFrameKind::Keyframes, frame_count, width)
            }
            TimelineMetaKind::Covered => {
                let marks = pipe
                    .as_ref()
                    .map(|s| s.ready_frames.as_slice())
                    .unwrap_or(&[]);
                cache_strip_runs_for_width(marks, ReadyFrameKind::Covered, frame_count, width)
            }
            TimelineMetaKind::Exact => {
                let marks = pipe
                    .as_ref()
                    .map(|s| s.ready_frames.as_slice())
                    .unwrap_or(&[]);
                cache_strip_runs_for_width(marks, ReadyFrameKind::Exact, frame_count, width)
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
