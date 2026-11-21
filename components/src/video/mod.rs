//! Leptos `<Video>` element — frame-precise media stage over [`fpmedia_web::VideoPlayer`].
//!
//! Analogous to HTML `<video>`: owns canvas, open/decode, play clock, seek/step.
//! Host UI (controls, file picker) lives in the demo `app` crate.

mod component;
mod ctx;
mod host;
mod host_ops;
mod op_queue;
mod open_url;
mod pipeline_interest;
mod session;
mod transport_apply;

pub use component::Video;
pub use fpmedia_web::{
    PipelineInterest, PipelineStats, ReadyFrameKind, ReadyFrameMark, ScrubCachePolicy, ThumbPolicy,
    VideoMetadata,
};
pub use pipeline_interest::{PipelineInterestLease, PipelinePlane};

use fpmedia_web::FrameId;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlCanvasElement;

use crate::virtual_timeline::{resolve_zone, store_zone};
use host::VideoHost;

/// Cloneable handle to a mounted [`Video`] — like a ref to an HTMLMediaElement.
#[derive(Clone, Copy)]
pub struct VideoRef {
    pub busy: RwSignal<bool>,
    pub ready: RwSignal<bool>,
    pub playing: RwSignal<bool>,
    pub looping: RwSignal<bool>,
    /// Inclusive LoopZone start (source FrameId). Default 0. See `glossary.md`.
    pub loop_in: RwSignal<u32>,
    /// Exclusive LoopZone end; `None` = full `frame_count`.
    pub loop_out: RwSignal<Option<u32>>,
    /// True after playback stops because the last frame was reached (`loop` off).
    /// Cleared on [`Self::open_blob`], [`Self::close`], seek/scrub/step, and play.
    pub ended: RwSignal<bool>,
    /// Engine-committed presentation frame (may lag during async scrub).
    pub frame: RwSignal<u32>,
    /// UI / transport playhead — scrubber, Prev/Next/Seek/Play/Pause target.
    /// Scrub updates this immediately without locking; sync ops promote it and
    /// drive the engine from this value (not from a stale `frame`).
    pub playhead: RwSignal<u32>,
    pub frame_count: RwSignal<u32>,
    pub timestamp: RwSignal<f64>,
    pub fps: RwSignal<Option<f64>>,
    pub metadata: RwSignal<Option<VideoMetadata>>,
    pub error: RwSignal<Option<String>>,
    pub pipeline: RwSignal<Option<PipelineStats>>,
    pub status: RwSignal<String>,
    /// True when the open file has a decodable audio track.
    pub has_audio: RwSignal<bool>,
    pub volume: RwSignal<f64>,
    pub muted: RwSignal<bool>,
    /// Frame.io-style playback rate (forward 0.25–8).
    pub playback_rate: RwSignal<f64>,
    /// Non-`Send` host registered by the mounted [`Video`] component.
    pub(crate) host: StoredValue<Option<VideoHost>, LocalStorage>,
    /// Refcounts for [`PipelineInterest`] planes (strips / debug meters).
    pipeline_interest_counts: StoredValue<pipeline_interest::InterestCounts, LocalStorage>,
    /// Host mid-zone: next SeamHold release restarts PlayIter @aim.
    /// Cleared by PlayPrime. Full-axis joins leave false.
    seam_hold_restart_on_release: StoredValue<bool, LocalStorage>,
    /// VT ledger — Covered sole on VT; Exact write unused (slot blit).
    pub(crate) exact_ledger:
        StoredValue<Option<crate::virtual_timeline::LedgerHandle>, LocalStorage>,
    /// Bumps when VT Covered changes (TimelineMeta Covered subscribe).
    pub exact_residency_epoch: RwSignal<u64>,
    /// Host thumb decode box (max edge). Progressive + hover share this.
    /// Default = engine `DEFAULT_THUMB_MAX_*` (160). See `product.md` § Hover thumbs.
    pub thumb_max_w: RwSignal<u32>,
    pub thumb_max_h: RwSignal<u32>,
}

impl VideoRef {
    pub fn new() -> Self {
        Self {
            busy: RwSignal::new(false),
            ready: RwSignal::new(false),
            playing: RwSignal::new(false),
            looping: RwSignal::new(false),
            loop_in: RwSignal::new(0),
            loop_out: RwSignal::new(None),
            ended: RwSignal::new(false),
            frame: RwSignal::new(0),
            playhead: RwSignal::new(0),
            frame_count: RwSignal::new(0),
            timestamp: RwSignal::new(0.0),
            fps: RwSignal::new(None),
            metadata: RwSignal::new(None),
            error: RwSignal::new(None),
            pipeline: RwSignal::new(None),
            status: RwSignal::new(String::from("Load an MP4 to begin.")),
            has_audio: RwSignal::new(false),
            // HTMLMediaElement default volume is 1.0.
            volume: RwSignal::new(1.0),
            muted: RwSignal::new(false),
            playback_rate: RwSignal::new(1.0),
            host: StoredValue::new_local(None),
            pipeline_interest_counts: StoredValue::new_local(
                pipeline_interest::InterestCounts::default(),
            ),
            seam_hold_restart_on_release: StoredValue::new_local(false),
            exact_ledger: StoredValue::new_local(
                Some(crate::virtual_timeline::new_ledger_handle()),
            ),
            exact_residency_epoch: RwSignal::new(0),
            thumb_max_w: RwSignal::new(fpmedia_web::DEFAULT_THUMB_MAX_W),
            thumb_max_h: RwSignal::new(fpmedia_web::DEFAULT_THUMB_MAX_H),
        }
    }

    /// Thumb decode max box `(max_w, max_h)` — host API (hover + Pause progressive).
    pub fn thumb_max_size(&self) -> (u32, u32) {
        (
            self.thumb_max_w.get_untracked().max(1),
            self.thumb_max_h.get_untracked().max(1),
        )
    }

    /// Set thumb decode max box. Clears cached thumbs so sizes do not mix.
    pub fn set_thumb_max_size(&self, max_w: u32, max_h: u32) {
        let w = max_w.max(1);
        let h = max_h.max(1);
        if self.thumb_max_w.get_untracked() == w && self.thumb_max_h.get_untracked() == h {
            return;
        }
        self.thumb_max_w.set(w);
        self.thumb_max_h.set(h);
        if let Some(host) = self.host() {
            host.apply_thumb_max_size(w, h);
        }
    }

    /// VT ledger handle (Covered + Fetch sole; Exact AimWindow parked).
    pub fn exact_ledger(&self) -> Option<crate::virtual_timeline::LedgerHandle> {
        self.exact_ledger.get_value()
    }

    /// Notify Covered/Fetch strip that the VT ledger changed.
    pub fn bump_exact_residency(&self) {
        self.exact_residency_epoch
            .update(|n| *n = n.wrapping_add(1));
    }

    /// Clear VT Covered only (user Scrub Off). Idle park must not call this.
    /// Does **not** clear Fetch (bytes still playable).
    pub fn clear_vt_covered(&self) {
        if let Some(h) = self.exact_ledger.get_value() {
            h.borrow_mut().clear_covered();
            self.bump_exact_residency();
        }
    }

    /// Clear VT ledger (new open / close) — Covered + Fetch + parked Exact values.
    pub fn reset_exact_ledger(&self) {
        if let Some(h) = self.exact_ledger.get_value() {
            let mut led = h.borrow_mut();
            for (_, bmp) in led.take_all_exact() {
                bmp.close();
            }
        } else {
            self.exact_ledger
                .set_value(Some(crate::virtual_timeline::new_ledger_handle()));
        }
        self.bump_exact_residency();
    }

    /// Next SeamHold → Live release restarts PlayIter (Host mid-zone only).
    pub fn set_seam_hold_restart_on_release(&self, yes: bool) {
        self.seam_hold_restart_on_release.set_value(yes);
    }

    /// Take-and-clear the Host mid-zone restart latch (PlayPrime).
    pub(crate) fn take_seam_hold_restart_on_release(&self) -> bool {
        let yes = self.seam_hold_restart_on_release.get_value();
        self.seam_hold_restart_on_release.set_value(false);
        yes
    }

    /// Clone the mounted host out of [`Self::host`] (call it after the store
    /// borrow ends — host methods may re-enter `VideoRef`).
    fn host(&self) -> Option<VideoHost> {
        self.host.try_with_value(|h| h.clone()).flatten()
    }

    /// Clamp `frame` to the clip, clear `ended`, move the UI playhead there.
    fn aim(&self, frame: u32) -> u32 {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let frame = frame.min(max);
        self.ended.set(false);
        self.playhead.set(frame);
        frame
    }

    pub fn open_blob(&self, blob: web_sys::Blob) {
        if let Some(host) = self.host() {
            host.open_blob(blob);
        } else {
            self.status
                .set("Video not mounted — hard-refresh the page.".into());
            self.error.set(Some("Video not mounted".into()));
        }
    }

    /// Open a remote or `blob:` URL.
    ///
    /// `https:` / `http:` use progressive [`fpmedia_web::UrlSource`] (HTTP Range)
    /// when the server returns `206` + `Content-Range`; otherwise falls back to
    /// a full download → [`Self::open_blob`]. Same-document `blob:` always goes
    /// through fetch → Blob.
    ///
    /// Cross-origin URLs need a CORS-allowing host (or a same-origin proxy URL
    /// the host supplies). This crate does **not** rewrite URLs.
    pub fn open_url(&self, url: impl Into<String>) {
        let url = url.into();
        if let Some(host) = self.host() {
            host.open_url(url);
        } else {
            self.status
                .set("Video not mounted — hard-refresh the page.".into());
            self.error.set(Some("Video not mounted".into()));
        }
    }

    /// Drop the decode session (decoder + caches). Canvas stays mounted for a later open.
    pub fn close(&self) {
        if let Some(host) = self.host() {
            host.close();
        }
    }

    pub fn play(&self) {
        self.ended.set(false);
        // Do **not** `seek_frame` here when `frame != playhead`. After scrub, settle
        // is already cascading key→playhead; a Play-time seek cancels that work and
        // restarts the GOP (multi-second lag on long-GOP clips). PlayPrime
        // waits behind settle (or seeks only if still mismatched after).
        if let Some(host) = self.host() {
            host.play();
        }
    }

    pub fn pause(&self) {
        // Play→Pause: stop first, then snap playhead to committed engine/canvas.
        // During play, UI playhead lags ~100ms behind (`sync_ui` throttle); seeking
        // to that stale playhead jumped the picture on Pause→Seek / Prev→Next.
        // Already paused with playhead ≠ frame (scrub target): promote via seek.
        let was_playing = self.playing.get_untracked();
        let ph = self.playhead.get_untracked();
        if let Some(host) = self.host() {
            host.pause();
        }
        if !was_playing && self.frame.get_untracked() != ph {
            self.seek_frame(ph);
        }
    }

    /// Internal relocate (transport seek). Prefer [`Self::goto_frame`] for UI chrome.
    pub fn seek_frame(&self, frame: u32) {
        let frame = self.aim(frame);
        if let Some(host) = self.host() {
            host.seek_frame(frame);
        }
    }

    /// User goto: aim FrameId, stop Play, ignore LoopZone. See `glossary.md` § Goto.
    pub fn goto_frame(&self, frame: u32) {
        self.seek_frame(frame);
    }

    /// Fast scrub preview — snap to the nearest keyframe at or before `frame`.
    pub fn seek_keyframe(&self, frame: u32) {
        let frame = self.aim(frame);
        if let Some(host) = self.host() {
            host.seek_keyframe(frame);
        }
    }

    /// Timeline pointer down — transport FSM (`architecture.md` § State machines).
    pub fn scrub_pointer_down(&self, frame: u32) {
        let frame = self.aim(frame);
        if let Some(host) = self.host() {
            host.scrub_pointer_down(frame);
        }
    }

    /// Timeline pointer move while scrubbing.
    pub fn scrub_pointer_move(&self, frame: u32) {
        let frame = self.aim(frame);
        if let Some(host) = self.host() {
            host.scrub_pointer_move(frame);
        }
    }

    /// Timeline pointer up — settle (± resume play when scrub began while playing).
    pub fn scrub_pointer_up(&self, frame: u32) {
        let frame = self.aim(frame);
        if let Some(host) = self.host() {
            host.scrub_pointer_up(frame);
        }
    }

    /// Advance one presentation frame from the engine, or seek to a divergent
    /// scrub `playhead`. Key-repeat must **not** keep bumping `playhead` ahead of
    /// `frame` — that promoted Step→Seek and jumped several frames on hold.
    pub fn next_frame(&self) {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let engine = self.frame.get_untracked();
        let ph = self.playhead.get_untracked();
        self.ended.set(false);

        // Scrub/seek target still ahead/behind engine — promote via seek.
        let adjacent_aim = engine.saturating_add(1).min(max);
        if ph != engine && ph != adjacent_aim {
            if let Some(host) = self.host() {
                host.seek_frame(ph);
            }
            return;
        }
        if engine >= max {
            return;
        }
        self.playhead.set(adjacent_aim);
        if let Some(host) = self.host() {
            host.next_frame();
        }
    }

    /// Step one frame backward from the engine (same hold-safe rules as
    /// [`Self::next_frame`]).
    pub fn previous_frame(&self) {
        let engine = self.frame.get_untracked();
        let ph = self.playhead.get_untracked();
        self.ended.set(false);

        let adjacent_aim = engine.saturating_sub(1);
        if ph != engine && ph != adjacent_aim {
            if let Some(host) = self.host() {
                host.seek_frame(ph);
            }
            return;
        }
        if engine == 0 {
            return;
        }
        self.playhead.set(adjacent_aim);
        if let Some(host) = self.host() {
            host.previous_frame();
        }
    }

    pub fn set_loop(&self, on: bool) {
        self.looping.set(on);
        if let Some(host) = self.host() {
            host.set_loop(on);
        }
    }

    /// Cold stop without densify (playlist LoopZone wall / seam). Not Pause button.
    pub fn pause_hard_no_densify(&self) {
        self.playing.set(false);
        self.ended.set(false);
        if let Some(host) = self.host() {
            host.pause_hard_no_densify();
        }
    }

    /// Resolved half-open LoopZone (`out` follows `frame_count` when unset).
    pub fn loop_zone(&self) -> (u32, u32) {
        resolve_zone(
            self.frame_count.get_untracked(),
            self.loop_in.get_untracked(),
            self.loop_out.get_untracked(),
        )
    }

    /// Set half-open `[in_frame, out_frame)` while looping. Full file when
    /// `in_frame == 0` and `out_frame == frame_count` (or `out >= count`).
    pub fn set_loop_zone(&self, in_frame: u32, out_frame: u32) {
        let zone = store_zone(self.frame_count.get_untracked(), in_frame, out_frame);
        self.loop_in.set(zone.in_frame);
        self.loop_out.set(zone.loop_out);
        if let Some(host) = self.host() {
            host.set_loop_zone(zone.in_frame, zone.out_frame);
        }
    }

    pub fn set_volume(&self, volume: f64) {
        let v = volume.clamp(0.0, 1.0);
        self.volume.set(v);
        if let Some(host) = self.host() {
            host.set_volume(v);
        }
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.set(muted);
        if let Some(host) = self.host() {
            host.set_muted(muted);
        }
    }

    pub fn set_playback_rate(&self, rate: f64) {
        let r = fpmedia_web::normalize_playback_rate(rate);
        self.playback_rate.set(r);
        if let Some(host) = self.host() {
            host.set_playback_rate(r);
        }
    }

    /// Paint a Frame.io-style hover thumb into `dest`.
    ///
    /// ThumbSink cache / Exact downscale — does not move playhead/stage.
    /// Prefer [`crate::HoverThumbPreview`]; call
    /// [`Self::cancel_hover_thumb`] on leave/scrub. Prefer
    /// [`Self::try_paint_hover_thumb`] on pointermove for cache hits.
    pub fn paint_hover_thumb(&self, frame: u32, dest: HtmlCanvasElement, max_w: u32, max_h: u32) {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let frame = frame.min(max);
        if let Some(host) = self.host() {
            host.paint_hover_thumb(frame, dest, max_w, max_h);
        }
    }

    /// Sync paint from thumb / Exact blit cache only. `true` if `dest` was painted.
    pub fn try_paint_hover_thumb(
        &self,
        frame: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> bool {
        let max = self.frame_count.get_untracked().saturating_sub(1);
        let frame = frame.min(max);
        self.host()
            .is_some_and(|host| host.try_paint_hover_thumb(frame, dest, max_w, max_h))
    }

    /// Abort in-flight hover thumb Exact downscale / hide race with scrub.
    pub fn cancel_hover_thumb(&self) {
        if let Some(host) = self.host() {
            host.cancel_hover_thumb();
        }
    }

    /// Runtime thumb sink gate — persists across opens. `Off` drops [`fpmedia_web::ThumbSink`].
    pub fn set_thumb_policy(&self, policy: ThumbPolicy) {
        if let Some(host) = self.host() {
            host.set_thumb_policy(policy);
        }
    }

    /// Convenience: current thumb max width (see [`Self::thumb_max_size`]).
    pub fn thumb_max_w(&self) -> u32 {
        self.thumb_max_size().0
    }

    /// Convenience: current thumb max height (see [`Self::thumb_max_size`]).
    pub fn thumb_max_h(&self) -> u32 {
        self.thumb_max_size().1
    }

    /// Runtime scrub-cache gate — persists across opens. `Off` (no scrub UI)
    /// closes the scrub decoder and drops Covered / Exact / gray fill.
    pub fn set_scrub_cache_policy(&self, policy: ScrubCachePolicy) {
        // User Scrub Off: wipe VT Covered (session clear). Idle park must not use this.
        if matches!(policy, ScrubCachePolicy::Off) {
            self.clear_vt_covered();
        }
        if let Some(host) = self.host() {
            host.set_scrub_cache_policy(policy);
        }
    }

    /// VT backend demote / idle: abort densify, keep Exact + Covered.
    /// User Scrub Off still uses [`Self::set_scrub_cache_policy`].
    pub fn park_scrub_cache(&self) {
        if let Some(host) = self.host() {
            host.park_scrub_cache();
        }
    }

    /// True when Exact blit already holds `frame` (VT Exact-settle hit test).
    pub fn has_scrub_exact_blit(&self, frame: u32) -> bool {
        self.host()
            .is_some_and(|host| host.has_scrub_exact_blit(frame))
    }

    /// Present Exact blit for `frame` when cached. Returns whether painted.
    pub fn try_present_scrub_exact(&self, frame: u32) -> bool {
        self.host()
            .is_some_and(|host| host.try_present_scrub_exact(frame))
    }

    /// Arm densify after Exact present (VT PresentHit settle).
    pub fn arm_scrub_densify_after_exact(&self, frame: u32) {
        if let Some(host) = self.host() {
            host.arm_scrub_densify_after_exact(frame);
        }
    }

    /// Clear ExactHold without arming densify (playlist PresentHit settle).
    pub fn clear_densify_hold(&self) {
        if let Some(host) = self.host() {
            host.clear_densify_hold();
        }
    }

    /// Present a VT ledger Exact bitmap onto this slot.
    pub fn present_image_bitmap(&self, local: u32, bitmap: &web_sys::ImageBitmap) -> bool {
        self.host()
            .is_some_and(|host| host.present_image_bitmap(local, bitmap))
    }

    /// Take Exact blit for import into the VT ledger.
    pub fn take_scrub_exact_blit(&self, local: u32) -> Option<web_sys::ImageBitmap> {
        self.host()
            .and_then(|host| host.take_scrub_exact_blit(local))
    }

    /// Local Exact FrameIds in the slot blit.
    pub fn scrub_exact_frame_ids(&self) -> Vec<u32> {
        self.host()
            .map(|host| host.scrub_exact_frame_ids())
            .unwrap_or_default()
    }

    /// Local Covered marker FrameIds.
    pub fn scrub_covered_frame_ids(&self) -> Vec<u32> {
        self.host()
            .map(|host| host.scrub_covered_frame_ids())
            .unwrap_or_default()
    }

    /// Local playable Fetch runs (inclusive) for VT strip mirror.
    pub fn scrub_fetch_playable_runs(&self) -> Vec<(FrameId, FrameId)> {
        self.host()
            .map(|host| host.scrub_fetch_playable_runs())
            .unwrap_or_default()
    }

    /// Clone Exact blit for VT ledger import.
    pub fn clone_scrub_exact_blit(&self, local: u32) -> Option<web_sys::ImageBitmap> {
        self.host()
            .and_then(|host| host.clone_scrub_exact_blit(local))
    }

    /// Slot Exact capacity (frames).
    pub fn scrub_exact_capacity(&self) -> usize {
        self.host()
            .map(|host| host.scrub_exact_capacity())
            .unwrap_or(1)
    }

    /// VT densify authority on/off.
    pub fn set_vt_densify_authority(&self, on: bool) {
        if let Some(host) = self.host() {
            host.set_vt_densify_authority(on);
        }
    }

    /// VT Pause progressive thumbs on/off (playlist host planner).
    pub fn set_vt_thumb_authority(&self, on: bool) {
        if let Some(host) = self.host() {
            host.set_vt_thumb_authority(on);
        }
    }

    /// Local sync FrameIds for VT thumb progressive planning.
    pub fn sync_frame_ids(&self) -> Vec<u32> {
        self.host()
            .map(|host| host.sync_frame_ids())
            .unwrap_or_default()
    }

    /// ThumbCache capacity (coarse budget).
    pub fn thumb_max_thumbs(&self) -> usize {
        self.host()
            .map(|host| host.thumb_max_thumbs())
            .unwrap_or(256)
    }

    /// Whether ThumbCache holds local sync `key`.
    pub fn thumb_has_key(&self, key: u32) -> bool {
        self.host().is_some_and(|host| host.thumb_has_key(key))
    }

    /// Clone slot thumb bitmap for the VT wallet (slot keeps its copy).
    pub fn clone_thumb_bitmap(
        &self,
        key: u32,
        max_w: u32,
        max_h: u32,
    ) -> Option<web_sys::ImageBitmap> {
        self.host()
            .and_then(|host| host.clone_thumb_bitmap(key, max_w, max_h))
    }

    /// VT host: fill one progressive sync key (awaitable).
    pub async fn fill_thumb_key_at(&self, key: u32) {
        if let Some(host) = self.host() {
            host.fill_thumb_key_at(key).await;
        }
    }

    /// Queue local FrameId for VT densify fill task.
    pub fn queue_vt_densify_target(&self, local: u32) {
        if let Some(host) = self.host() {
            host.queue_vt_densify_target(local);
        }
    }

    /// Local Exact already in VT ledger — densify must not re-decode after take.
    pub fn set_vt_exact_residents(&self, locals: &[u32]) {
        if let Some(host) = self.host() {
            host.set_vt_exact_residents(locals);
        }
    }

    /// Scrub margins for [`crate::virtual_timeline::plan_next_densify_gap`].
    pub fn scrub_plan_margins(&self) -> (FrameId, FrameId) {
        self.host()
            .map(|host| host.scrub_plan_margins())
            .unwrap_or((12, 6))
    }

    pub fn scrub_dir(&self) -> i8 {
        self.host().map(|host| host.scrub_dir()).unwrap_or(0)
    }

    /// VT densify soft-ban locals (skip in timeline plan).
    pub fn vt_densify_banned_locals(&self) -> Vec<u32> {
        self.host()
            .map(|host| host.vt_densify_banned_locals())
            .unwrap_or_default()
    }

    pub fn densify_inflight(&self) -> bool {
        self.host().is_some_and(|host| host.densify_inflight())
    }

    /// Fire-and-forget frame-0 byte + PreferSoftware stash warm (playlist
    /// near-EOF idle; joins PlayPrime origin stash hit).
    pub fn kick_warm_play_origin(&self) {
        if let Some(host) = self.host() {
            spawn_local(async move {
                host.warm_play_origin().await;
            });
        }
    }

    /// Await frame-0 byte + origin stash warm (playlist seam before PlayPrime).
    pub async fn warm_play_origin(&self) {
        if let Some(host) = self.host() {
            host.warm_play_origin().await;
        }
    }

    /// Paint origin on this (idle) slot before playlist promote.
    pub async fn prepare_seam_origin(&self) -> Result<(), String> {
        self.prepare_seam_at(0).await
    }

    /// Paint `frame` on this (idle) slot before LoopZone / playlist promote.
    pub async fn prepare_seam_at(&self, frame: u32) -> Result<(), String> {
        if let Some(host) = self.host() {
            host.prepare_seam_at(frame).await
        } else {
            Err("Video not mounted".into())
        }
    }

    /// Fire-and-forget [`Self::prepare_seam_at`] (near-wall idle warm).
    pub fn kick_prepare_seam_at(&self, frame: u32) {
        if let Some(host) = self.host() {
            spawn_local(async move {
                let _ = host.prepare_seam_at(frame).await;
            });
        }
    }

    /// Idle Play-ready under `SeamHold` (playlist Host near-EOF / promote).
    pub async fn arm_seam_hold(&self, frame: u32) -> Result<(), String> {
        if let Some(host) = self.host() {
            host.arm_seam_hold(frame).await
        } else {
            Err("Video not mounted".into())
        }
    }

    /// Fire-and-forget [`Self::arm_seam_hold`].
    pub fn kick_arm_seam_hold(&self, frame: u32) {
        if let Some(host) = self.host() {
            spawn_local(async move {
                let _ = host.arm_seam_hold(frame).await;
            });
        }
    }
}

impl Default for VideoRef {
    fn default() -> Self {
        Self::new()
    }
}
