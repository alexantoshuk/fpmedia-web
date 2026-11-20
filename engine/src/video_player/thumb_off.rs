//! Feature `thumbs` off: same [`super::VideoPlayer`] thumb API as `thumb.rs`,
//! every hover paint is a soft miss (`Cancelled`) and policy reads `Off`.

use std::rc::Rc;

use web_sys::HtmlCanvasElement;

use crate::error::MediaError;
use crate::policy::ThumbPolicy;
use crate::types::FrameId;

use super::VideoPlayer;

impl VideoPlayer {
    pub fn cancel_hover_thumb(&self) {}

    pub fn set_thumb_policy(&self, _policy: ThumbPolicy) {}

    pub fn set_thumb_max_size(&self, _max_w: u32, _max_h: u32) {}

    pub fn thumb_policy(&self) -> ThumbPolicy {
        ThumbPolicy::Off
    }

    pub fn set_vt_thumb_authority(&self, _on: bool) {}

    pub fn vt_thumb_authority(&self) -> bool {
        false
    }

    pub fn sync_frame_ids(&self) -> Vec<FrameId> {
        Vec::new()
    }

    pub fn clone_thumb_bitmap(
        &self,
        _frame: FrameId,
        _max_w: u32,
        _max_h: u32,
    ) -> Option<web_sys::ImageBitmap> {
        None
    }

    pub fn thumb_max_thumbs(&self) -> usize {
        0
    }

    pub fn thumb_has_key(&self, _key: FrameId) -> bool {
        false
    }

    pub async fn fill_thumb_key_at(self: &Rc<Self>, _key: FrameId) {}

    pub(super) fn clear_thumbs(&self) {}

    pub(super) fn drop_thumb_sink(&self) {}

    pub(super) fn schedule_thumb_key_fill(&self) {}

    pub fn try_paint_hover_thumb_cached(
        &self,
        _frame: FrameId,
        _dest: &HtmlCanvasElement,
        _max_w: u32,
        _max_h: u32,
    ) -> Result<Option<FrameId>, MediaError> {
        Err(MediaError::Cancelled)
    }

    pub async fn paint_hover_thumb(
        &self,
        _frame: FrameId,
        _dest: &HtmlCanvasElement,
        _max_w: u32,
        _max_h: u32,
    ) -> Result<FrameId, MediaError> {
        Err(MediaError::Cancelled)
    }
}
