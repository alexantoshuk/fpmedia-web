//! Feature `thumbs` off: same [`super::VideoPlayer`] thumb API as `thumb.rs`,
//! every hover paint is a soft miss (`Cancelled`) and policy reads `Off`.

use web_sys::HtmlCanvasElement;

use crate::error::MediaError;
use crate::policy::ThumbPolicy;
use crate::types::FrameId;

use super::VideoPlayer;

impl VideoPlayer {
    pub fn cancel_hover_thumb(&self) {}

    pub fn set_thumb_policy(&self, _policy: ThumbPolicy) {}

    pub fn thumb_policy(&self) -> ThumbPolicy {
        ThumbPolicy::Off
    }

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
