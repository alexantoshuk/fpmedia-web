//! Play canvas pool — decoded samples are copied here and the `VideoFrame`
//! closed at once, so Play pins no decoder outputs while waiting to present.
//!
//! `mediabunny: src/media-sink.ts CanvasSink._videoSampleToWrappedCanvas`
//! (`poolSize: 2`, canvas sized to the display box, `sample.close()` after draw).

use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, VideoFrame};

use crate::decode::canvas::canvas_2d;
use crate::error::MediaError;
use crate::types::FrameId;

const POOL_SIZE: usize = 2;

struct PoolCanvas {
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    frame: Option<FrameId>,
}

pub(super) struct PlayCanvasPool {
    slots: Vec<PoolCanvas>,
    next: usize,
}

impl PlayCanvasPool {
    pub(super) fn new() -> Result<Self, MediaError> {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .ok_or_else(|| MediaError::WebCodecs("no document for play canvas pool".into()))?;
        let mut slots = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            let canvas: HtmlCanvasElement = document
                .create_element("canvas")
                .map_err(MediaError::from)?
                .dyn_into()
                .map_err(|_| MediaError::WebCodecs("play pool canvas cast failed".into()))?;
            let ctx = canvas_2d(&canvas)?;
            slots.push(PoolCanvas {
                canvas,
                ctx,
                frame: None,
            });
        }
        Ok(Self { slots, next: 0 })
    }

    /// Copy `video` into the next pool canvas at `w`×`h`; returns the slot.
    pub(super) fn copy(
        &mut self,
        frame: FrameId,
        video: &VideoFrame,
        w: u32,
        h: u32,
    ) -> Result<usize, MediaError> {
        let slot = self.next;
        self.next = (self.next + 1) % self.slots.len();
        let s = &mut self.slots[slot];
        if s.canvas.width() != w {
            s.canvas.set_width(w);
        }
        if s.canvas.height() != h {
            s.canvas.set_height(h);
        }
        s.ctx
            .draw_image_with_video_frame_and_dw_and_dh(video, 0.0, 0.0, w as f64, h as f64)
            .map_err(MediaError::from)?;
        s.frame = Some(frame);
        Ok(slot)
    }

    /// Draw `slot` scaled onto the stage buffer.
    pub(super) fn draw(
        &self,
        slot: usize,
        stage: &HtmlCanvasElement,
        stage_ctx: &CanvasRenderingContext2d,
    ) -> Result<(), MediaError> {
        let Some(s) = self.slots.get(slot) else {
            return Ok(());
        };
        stage_ctx
            .draw_image_with_html_canvas_element_and_dw_and_dh(
                &s.canvas,
                0.0,
                0.0,
                stage.width().max(1) as f64,
                stage.height().max(1) as f64,
            )
            .map_err(MediaError::from)
    }

    /// Slot still holding `frame` pixels (relayout repaint).
    pub(super) fn slot_for(&self, frame: FrameId) -> Option<usize> {
        self.slots.iter().position(|s| s.frame == Some(frame))
    }
}
