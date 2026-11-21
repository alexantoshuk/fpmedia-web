//! One PreferSoftware thumb producer for the VT wallet.
//!
//! Opens clip tracks via `open_mp4` (not playlist idle slots). Reuses live/idle
//! slot decode when the clip is already open. **no equivalent** (filmstrip local).
//! Decode: `mediabunny:` CanvasSink / `get`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use fpmedia_web::{
    BlobSource, Epoch, FrameId, MediaSource, OpenMp4Options, ThumbSink, ThumbSinkOptions,
    UrlSource, hover_thumb_dims, open_mp4, paint_thumb_canvas, thumb_progressive_coarse_count,
    thumb_progressive_rank,
};
use leptos::prelude::*;
use web_sys::{HtmlCanvasElement, ImageBitmap};

use super::thumb_cache::{ThumbCacheHandle, VtThumbCache};
use crate::frame_axis::GlobalFrame;
use crate::video_playlist::source::PlaylistSource;
use crate::video_playlist::video_playlist_ref::VideoPlaylistRef;

/// In-flight / retained producer for one playlist timeline.
pub struct ThumbProducer {
    clip: Cell<Option<u32>>,
    sink: RefCell<Option<Rc<ThumbSink>>>,
    /// Bumped to cancel in-flight open/decode.
    epoch: Epoch,
    inflight: Cell<bool>,
}

impl Default for ThumbProducer {
    fn default() -> Self {
        Self::new()
    }
}

impl ThumbProducer {
    pub fn new() -> Self {
        Self {
            clip: Cell::new(None),
            sink: RefCell::new(None),
            epoch: Epoch::new(),
            inflight: Cell::new(false),
        }
    }

    pub fn cancel(&self) {
        self.epoch.bump();
        self.inflight.set(false);
        self.clip.set(None);
        *self.sink.borrow_mut() = None;
    }

    pub fn inflight(&self) -> bool {
        self.inflight.get()
    }

    pub fn current_clip(&self) -> Option<u32> {
        self.clip.get()
    }

    /// Sync keys for the currently bound producer clip (local FrameIds).
    pub fn sync_frame_ids(&self) -> Vec<FrameId> {
        let Ok(slot) = self.sink.try_borrow() else {
            return Vec::new();
        };
        slot.as_ref()
            .map(|s| s.track().index().sync_frame_ids())
            .unwrap_or_default()
    }

    /// Ensure producer `ThumbSink` for `clip_index` (dedicated open when off-slot).
    pub async fn ensure_clip(
        &self,
        pl: VideoPlaylistRef,
        clip_index: u32,
    ) -> Result<Rc<ThumbSink>, String> {
        if self.clip.get() == Some(clip_index)
            && let Ok(slot) = self.sink.try_borrow()
            && slot.is_some()
        {
            return Ok(slot.as_ref().expect("checked").clone());
        }
        let epoch = self.epoch.ticket();
        // Dedicated open — never `ensure_clip_on_idle` / steal play slots.
        // On-slot fills prefer slot `fill_thumb_key_at` + VT import (see thumbs.rs).
        let src = pl
            .sources
            .try_with_value(|s| s.get(clip_index as usize).cloned())
            .flatten()
            .ok_or_else(|| format!("missing source for clip {}", clip_index + 1))?;

        let media: Arc<dyn MediaSource> = match &src {
            PlaylistSource::Blob(blob) => Arc::new(BlobSource::new(blob.clone())),
            PlaylistSource::Url(url) => {
                let u = UrlSource::connect(url.clone())
                    .await
                    .map_err(|e| e.to_string())?;
                Arc::new(u)
            }
        };
        if !self.epoch.is_current(epoch) {
            return Err("cancelled".into());
        }
        let opened = open_mp4(
            media,
            OpenMp4Options {
                verify_avc_sync: false,
                defer_avc_sync: true,
            },
            |_| {},
        )
        .await
        .map_err(|e| e.to_string())?;
        if !self.epoch.is_current(epoch) {
            return Err("cancelled".into());
        }
        let track = opened.require_video().map_err(|e| e.to_string())?;
        let (max_w, max_h) = pl.thumb_max_size();
        let sink = Rc::new(ThumbSink::new(
            Rc::new(track),
            ThumbSinkOptions::for_max_box(max_w, max_h),
        ));
        self.clip.set(Some(clip_index));
        *self.sink.borrow_mut() = Some(Rc::clone(&sink));
        Ok(sink)
    }

    /// Decode `local` on the bound sink and insert into VT cache at `global`.
    pub async fn decode_into_vt(
        &self,
        cache: &ThumbCacheHandle,
        global: FrameId,
        local: FrameId,
        max_w: u32,
        max_h: u32,
        rank: u16,
    ) -> Result<bool, String> {
        let sink = {
            let Ok(slot) = self.sink.try_borrow() else {
                return Err("borrow".into());
            };
            slot.as_ref()
                .cloned()
                .ok_or_else(|| "no sink".to_string())?
        };
        let epoch = self.epoch.ticket();
        let cancel = move || !self.epoch.is_current(epoch);
        sink.note_size(max_w, max_h);
        match sink.get(local, &cancel).await {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(false),
            Err(e) => return Err(e.to_string()),
        }
        if cancel() {
            return Err("cancelled".into());
        }
        let meta = sink.track().metadata();
        let (tw, th) = hover_thumb_dims(meta.width, meta.height, max_w, max_h);
        let key = sink.store_key(local);
        let Some(bitmap) = clone_sink_bitmap(&sink, key, local, tw, th) else {
            return Ok(false);
        };
        let mut c = cache.borrow_mut();
        Ok(c.put(GlobalFrame::new(global), bitmap, tw, th, rank))
    }
}

fn clone_sink_bitmap(
    sink: &ThumbSink,
    key: FrameId,
    frame: FrameId,
    width: u32,
    height: u32,
) -> Option<ImageBitmap> {
    sink.with_bitmap(key, frame, width, height, |bmp| bmp.clone())
}

/// Paint from VT cache (nearest). Returns painted global id.
pub fn try_paint_vt(
    cache: &VtThumbCache,
    global: GlobalFrame,
    dest: &HtmlCanvasElement,
    width: u32,
    height: u32,
) -> Option<GlobalFrame> {
    let (id, bmp, tw, th) = cache.nearest(global, width, height)?;
    paint_thumb_canvas(dest, bmp, tw, th).ok()?;
    Some(id)
}

/// Progressive rank for a global sync key given known global keys + Σ duration.
pub fn rank_for_global_key(
    global_keys: &[FrameId],
    global: FrameId,
    duration_secs: f64,
    max_thumbs: usize,
) -> u16 {
    let coarse = thumb_progressive_coarse_count(duration_secs, global_keys.len(), max_thumbs);
    thumb_progressive_rank(global_keys, global, coarse)
}

/// Shared producer — interior mutability already on [`ThumbProducer`] (no outer RefCell).
pub type ProducerHandle = Rc<ThumbProducer>;

pub fn new_producer_handle() -> ProducerHandle {
    Rc::new(ThumbProducer::new())
}
