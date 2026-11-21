//! Mounted-`<Video>` host: methods [`super::VideoRef`] forwards to.
//!
//! Replaces the old `VideoOps` struct of ~50 `Rc<dyn Fn>` — one concrete handle,
//! no vtable. Only closures leptos-use hands back with unnameable types stay
//! boxed (`kick_drain`, scrub dwell start/stop, throttled pointer move).
//! `architecture.md` § Code rules → Ownership & types.

use std::rc::Rc;

use fpmedia_web::{
    BlobSource, FrameId, FrameRun, MediaSource, OpenPhase, OpenSourceKind, ScrubCachePolicy,
    ScrubDir, ThumbPolicy, TransportEvent, TransportState,
};
use leptos::html::Canvas;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlCanvasElement, ImageBitmap};

use super::VideoRef;
use super::ctx::{HostCtx, KickDrain};
use super::op_queue::{HostOp, OpenArgs};
use super::open_url::resolve_url_source;
use super::transport_apply::{dispatch_transport, note_transport};
use crate::virtual_timeline::store_zone;

/// Cloneable host for one mounted [`super::Video`] (cheap: `Rc`s + `Copy` signals).
#[derive(Clone)]
pub(crate) struct VideoHost {
    ctx: HostCtx,
    canvas: NodeRef<Canvas>,
    /// Starts the op-queue drain loop (built in `<Video>`; needs `resume_raf`).
    kick_drain: KickDrain,
    /// Soft-drag stillness timer (leptos-use `use_timeout_fn`).
    dwell_start: Rc<dyn Fn()>,
    dwell_stop: Rc<dyn Fn()>,
    /// Rate-limited pointer-move dispatch (leptos-use throttle).
    move_throttled: Rc<dyn Fn(u32)>,
}

impl VideoHost {
    pub(super) fn new(
        ctx: HostCtx,
        canvas: NodeRef<Canvas>,
        kick_drain: KickDrain,
        dwell_start: Rc<dyn Fn()>,
        dwell_stop: Rc<dyn Fn()>,
        move_throttled: Rc<dyn Fn(u32)>,
    ) -> Self {
        Self {
            ctx,
            canvas,
            kick_drain,
            dwell_start,
            dwell_stop,
            move_throttled,
        }
    }

    fn dispatch(&self, event: TransportEvent) {
        dispatch_transport(&self.ctx, &self.kick_drain, event);
    }

    // --- open / close -----------------------------------------------------

    pub(crate) fn open_blob(&self, blob: web_sys::Blob) {
        let ctx = &self.ctx;
        let VideoRef { error, status, .. } = ctx.video;
        let Some(canvas) = self.canvas.get_untracked() else {
            error.set(Some("canvas not mounted".into()));
            status.set("Canvas missing — refresh the page.".into());
            return;
        };
        let canvas: HtmlCanvasElement = canvas;
        ctx.retire_clip();
        let size_kb = (blob.size() as u64) / 1024;
        status.set(OpenPhase::ProbingSource(OpenSourceKind::Progressive { size_kb }).label());
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&format!("fpmedia-web: open_blob start ~{size_kb} KB").into());
        let source: std::sync::Arc<dyn MediaSource> = std::sync::Arc::new(BlobSource::new(blob));
        if ctx.queue.enqueue(HostOp::Open(OpenArgs { source, canvas })) {
            (self.kick_drain)();
        }
    }

    pub(crate) fn open_url(&self, url: String) {
        let ctx = &self.ctx;
        let VideoRef {
            ready,
            error,
            status,
            ..
        } = ctx.video;
        let Some(canvas) = self.canvas.get_untracked() else {
            error.set(Some("canvas not mounted".into()));
            status.set("Canvas missing — refresh the page.".into());
            return;
        };
        let canvas: HtmlCanvasElement = canvas;
        let url = url.trim().to_string();
        if url.is_empty() {
            error.set(Some("empty URL".into()));
            status.set("Enter a URL to open.".into());
            return;
        }
        ctx.retire_clip();
        let fetch_id = ctx.queue.current_id();
        // Do **not** latch `busy` across resolve — cancelled fetch used to
        // leave busy=true forever when drain had already ended. `!ready`
        // keeps Play disabled; Open enqueue sets busy for demux/seek.
        // Cross-origin: host must pass a CORS-ok or same-origin URL.
        status.set(OpenPhase::OpeningUrl { url: url.clone() }.label());
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&format!("fpmedia-web: open_url start {url}").into());

        let queue = ctx.queue.clone();
        let kick_drain = Rc::clone(&self.kick_drain);
        spawn_local(async move {
            let opened = resolve_url_source(&url, |phase| status.set(phase.label())).await;
            if !queue.is_current(fetch_id) {
                queue.sync_busy();
                return;
            }
            match opened {
                Ok(source) => {
                    if queue.enqueue(HostOp::Open(OpenArgs { source, canvas })) {
                        kick_drain();
                    }
                }
                Err(e) => {
                    queue.sync_busy();
                    ready.set(false);
                    error.set(Some(e.clone()));
                    status.set("Open URL failed.".into());
                    #[cfg(target_arch = "wasm32")]
                    web_sys::console::error_1(&e.into());
                }
            }
        });
    }

    pub(crate) fn close(&self) {
        self.ctx.retire_clip();
        self.ctx.video.status.set("Closed.".into());
    }

    // --- transport --------------------------------------------------------

    pub(crate) fn play(&self) {
        let ctx = &self.ctx;
        if !ctx.video.ready.get_untracked() {
            web_sys::console::log_1(&"fpmedia-web: play ignored (not ready)".into());
            return;
        }
        // Play intent already on (Playing / scrub-while-play / resume-settle).
        // Recover only stale `Playing` with a dead session — not Settling/Scrubbing
        // resume (soft-hold clears live while intent stays). `product.md` § Play chrome.
        let live = ctx
            .player
            .borrow()
            .as_ref()
            .is_some_and(|p| p.play_session_live());
        let state = ctx.transport.state();
        if state.ui_playing(live) {
            if live {
                web_sys::console::log_1(&"fpmedia-web: play ignored (already playing)".into());
                return;
            }
            if matches!(state, TransportState::Playing) {
                web_sys::console::log_1(&"fpmedia-web: recover stale transport playing".into());
                ctx.stop_playback();
            } else {
                web_sys::console::log_1(
                    &"fpmedia-web: play ignored (play intent, session soft-held)".into(),
                );
                return;
            }
        }
        if ctx.player.borrow().as_ref().is_none() {
            web_sys::console::log_1(&"fpmedia-web: play ignored (no player)".into());
            return;
        }
        self.dispatch(TransportEvent::PlayPressed);
    }

    pub(crate) fn pause(&self) {
        self.dispatch(TransportEvent::PausePressed);
    }

    pub(crate) fn seek_frame(&self, frame: u32) {
        self.dispatch(TransportEvent::SeekRequested { frame });
    }

    pub(crate) fn seek_keyframe(&self, frame: u32) {
        self.dispatch(TransportEvent::SeekKeyframeRequested { frame });
    }

    pub(crate) fn next_frame(&self) {
        self.dispatch(TransportEvent::StepRequested { forward: true });
    }

    pub(crate) fn previous_frame(&self) {
        self.dispatch(TransportEvent::StepRequested { forward: false });
    }

    /// Pointer-down: Exact hit → paint once; miss → arm dwell settle.
    pub(crate) fn scrub_pointer_down(&self, frame: u32) {
        self.dispatch(TransportEvent::PointerDown { frame });
        (self.dwell_stop)();
        let hit = self
            .ctx
            .with_player(|p| p.try_present_scrub_exact(frame))
            .unwrap_or(false);
        if hit {
            return;
        }
        (self.dwell_start)();
    }

    /// Pointer-move: never sync-present Exact — that raced throttled ScrubDrag on
    /// Exact-strip R→L (flicker + key стоп-кадр). Canvas = ScrubDrag; dwell Exact
    /// only after stillness when blit miss. **no equivalent** (scrub local).
    pub(crate) fn scrub_pointer_move(&self, target: u32) {
        (self.dwell_stop)();
        let has = self
            .ctx
            .with_player(|p| p.has_scrub_exact_blit(target))
            .unwrap_or(false);
        if !has {
            (self.dwell_start)();
        }
        (self.move_throttled)(target);
    }

    pub(crate) fn scrub_pointer_up(&self, frame: u32) {
        (self.dwell_stop)();
        self.dispatch(TransportEvent::PointerUp { frame });
    }

    pub(crate) fn set_loop(&self, on: bool) {
        self.ctx.with_player(|p| p.set_loop(on));
        self.ctx.sync_ui();
    }

    pub(crate) fn set_loop_zone(&self, in_frame: u32, out_frame: u32) {
        let ctx = &self.ctx;
        let video = ctx.video;
        ctx.with_player(|p| p.set_loop_zone(in_frame, out_frame));
        let zone = store_zone(video.frame_count.get_untracked(), in_frame, out_frame);
        video.loop_in.set(zone.in_frame);
        video.loop_out.set(zone.loop_out);
        ctx.sync_ui();
    }

    /// Leave Playing without SoftPause densify (not PausePressed).
    pub(crate) fn pause_hard_no_densify(&self) {
        note_transport(&self.ctx, TransportEvent::Ended);
        self.ctx.with_player(|p| p.pause_hard_no_densify());
        self.ctx.video.playing.set(false);
        self.ctx.sync_ui();
    }

    pub(crate) fn set_volume(&self, v: f64) {
        self.ctx.with_player(|p| p.set_volume(v));
    }

    pub(crate) fn set_muted(&self, m: bool) {
        self.ctx.with_player(|p| p.set_muted(m));
    }

    pub(crate) fn set_playback_rate(&self, r: f64) {
        self.ctx.with_player(|p| p.set_playback_rate(r));
    }

    // --- hover thumb / policies ------------------------------------------

    pub(crate) fn paint_hover_thumb(
        &self,
        frame: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) {
        let Some(p) = self.ctx.player.borrow().as_ref().cloned() else {
            return;
        };
        spawn_local(async move {
            let _ = p.paint_hover_thumb(frame, &dest, max_w, max_h).await;
        });
    }

    pub(crate) fn try_paint_hover_thumb(
        &self,
        frame: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> bool {
        self.ctx
            .with_player(|p| {
                let aim = frame;
                match p.try_paint_hover_thumb_cached(aim, &dest, max_w, max_h) {
                    // Exact frame hit → skip defer. Key/far interim still
                    // paints, but return false so host defers Exact refine
                    // (Pause floor; Play refuses aim decode).
                    // Cold miss: keep last pixels (nearest stand-in) — host
                    // must not wipe to empty (`<HoverThumb>` paint-first).
                    Ok(Some(painted)) => p.hover_thumb_close_enough(aim, painted),
                    Ok(None) | Err(_) => false,
                }
            })
            .unwrap_or(false)
    }

    pub(crate) fn cancel_hover_thumb(&self) {
        self.ctx.with_player(|p| p.cancel_hover_thumb());
    }

    pub(crate) fn set_thumb_policy(&self, policy: ThumbPolicy) {
        self.ctx.thumb_policy.set(policy);
        self.ctx.with_player(|p| p.set_thumb_policy(policy));
    }

    pub(crate) fn apply_thumb_max_size(&self, max_w: u32, max_h: u32) {
        self.ctx.with_player(|p| p.set_thumb_max_size(max_w, max_h));
    }

    pub(crate) fn set_scrub_cache_policy(&self, policy: ScrubCachePolicy) {
        self.ctx.scrub_cache_policy.set(policy);
        self.ctx.with_player(|p| p.set_scrub_cache_policy(policy));
    }

    pub(crate) fn park_scrub_cache(&self) {
        // Capability Off without wipe — host signal stays Off so open re-apply
        // does not clear parked Exact. User Scrub Off uses set_scrub_cache_policy.
        self.ctx.scrub_cache_policy.set(ScrubCachePolicy::Off);
        self.ctx.with_player(|p| p.park_scrub_cache());
    }

    // --- scrub / VT queries ----------------------------------------------

    pub(crate) fn has_scrub_exact_blit(&self, frame: u32) -> bool {
        self.ctx
            .with_player(|p| p.has_scrub_exact_blit(frame))
            .unwrap_or(false)
    }

    pub(crate) fn try_present_scrub_exact(&self, frame: u32) -> bool {
        self.ctx
            .with_player(|p| p.try_present_scrub_exact(frame))
            .unwrap_or(false)
    }

    pub(crate) fn arm_scrub_densify_after_exact(&self, frame: u32) {
        self.ctx
            .with_player(|p| p.arm_scrub_densify_after_exact(frame));
    }

    pub(crate) fn clear_densify_hold(&self) {
        self.ctx.with_player(|p| p.clear_densify_hold());
    }

    pub(crate) fn present_image_bitmap(&self, frame: u32, bitmap: &ImageBitmap) -> bool {
        self.ctx
            .with_player(|p| p.present_image_bitmap(frame, bitmap).is_ok())
            .unwrap_or(false)
    }

    pub(crate) fn take_scrub_exact_blit(&self, frame: u32) -> Option<ImageBitmap> {
        self.ctx
            .with_player(|p| p.take_scrub_exact_blit(frame))
            .flatten()
    }

    pub(crate) fn scrub_exact_frame_ids(&self) -> Vec<u32> {
        self.ctx
            .with_player(|p| p.scrub_exact_frame_ids().into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
    }

    pub(crate) fn scrub_covered_frame_ids(&self) -> Vec<u32> {
        self.ctx
            .with_player(|p| p.scrub_covered_frame_ids().into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
    }

    pub(crate) fn scrub_fetch_playable_runs(&self) -> Vec<FrameRun> {
        self.ctx
            .with_player(|p| p.scrub_fetch_playable_runs())
            .unwrap_or_default()
    }

    pub(crate) fn clone_scrub_exact_blit(&self, frame: u32) -> Option<ImageBitmap> {
        self.ctx
            .with_player(|p| p.clone_scrub_exact_blit(frame))
            .flatten()
    }

    pub(crate) fn scrub_exact_capacity(&self) -> usize {
        self.ctx
            .with_player(|p| p.scrub_exact_capacity())
            .unwrap_or(1)
    }

    pub(crate) fn set_vt_densify_authority(&self, on: bool) {
        self.ctx.with_player(|p| p.set_vt_densify_authority(on));
    }

    pub(crate) fn set_vt_thumb_authority(&self, on: bool) {
        self.ctx.with_player(|p| p.set_vt_thumb_authority(on));
    }

    pub(crate) fn sync_frame_ids(&self) -> Vec<u32> {
        self.ctx
            .with_player(|p| p.sync_frame_ids().into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
    }

    pub(crate) fn thumb_max_thumbs(&self) -> usize {
        self.ctx
            .with_player(|p| p.thumb_max_thumbs())
            .unwrap_or(256)
    }

    pub(crate) fn thumb_has_key(&self, key: u32) -> bool {
        self.ctx
            .with_player(|p| p.thumb_has_key(key))
            .unwrap_or(false)
    }

    pub(crate) fn clone_thumb_bitmap(
        &self,
        key: u32,
        max_w: u32,
        max_h: u32,
    ) -> Option<ImageBitmap> {
        self.ctx
            .with_player(|p| p.clone_thumb_bitmap(key, max_w, max_h))
            .flatten()
    }

    pub(crate) fn queue_vt_densify_target(&self, frame: u32) {
        self.ctx.with_player(|p| p.queue_vt_densify_target(frame));
    }

    pub(crate) fn set_vt_exact_residents(&self, locals: &[u32]) {
        self.ctx.with_player(|p| p.set_vt_exact_residents(locals));
    }

    pub(crate) fn scrub_plan_margins(&self) -> (FrameId, FrameId) {
        self.ctx
            .with_player(|p| p.scrub_plan_margins())
            .unwrap_or((12, 6))
    }

    pub(crate) fn scrub_dir(&self) -> ScrubDir {
        self.ctx.with_player(|p| p.scrub_dir()).unwrap_or_default()
    }

    pub(crate) fn vt_densify_banned_locals(&self) -> Vec<u32> {
        self.ctx
            .with_player(|p| p.vt_densify_banned_locals().into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
    }

    pub(crate) fn densify_inflight(&self) -> bool {
        self.ctx
            .with_player(|p| p.densify_inflight())
            .unwrap_or(false)
    }

    // --- async (clone the player out before `.await`) ---------------------

    pub(crate) async fn fill_thumb_key_at(&self, key: u32) {
        let Some(p) = self.ctx.player.borrow().as_ref().cloned() else {
            return;
        };
        p.fill_thumb_key_at(key).await;
    }

    pub(crate) async fn warm_play_origin(&self) {
        let Some(p) = self.ctx.player.borrow().as_ref().cloned() else {
            return;
        };
        // Bytes + PreferSoftware origin stash (playlist seam / loop cousin).
        let _ = p.warm_play_origin_bytes().await;
        let _ = p.warm_play_origin_stash().await;
    }

    pub(crate) async fn prepare_seam_at(&self, frame: u32) -> Result<(), String> {
        let Some(p) = self.ctx.player.borrow().as_ref().cloned() else {
            return Err("no player".into());
        };
        p.prepare_playlist_seam_at(frame)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn arm_seam_hold(&self, frame: u32) -> Result<(), String> {
        let Some(p) = self.ctx.player.borrow().as_ref().cloned() else {
            return Err("no player".into());
        };
        p.arm_playlist_seam_hold_at(frame)
            .await
            .map_err(|e| e.to_string())
    }
}
