//! [`VideoPlaylistRef`] — host handle for a mounted [`super::VideoPlaylist`].

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use web_sys::HtmlCanvasElement;

use crate::VideoRef;
use crate::video_playlist::{PlaylistModel, PlaylistPos};
use crate::virtual_timeline::{resolve_zone, store_zone};

use super::host::PlaylistHost;
use super::loop_seam::{loop_owner, seam_busy};
use super::loop_seam_exec::start_host_wrap;
use super::plan::{ScrubUpCtx, ScrubUpPlan, plan_scrub_up};
use super::slots::{
    idle_neighbor, kick_cut_join_exact_warm, kick_ensure_idle, preload_idle, try_promote_idle,
};
use super::source::PlaylistSource;
use wasm_bindgen_futures::spawn_local;

/// Cloneable handle to a mounted [`super::VideoPlaylist`] (two [`VideoRef`] slots).
#[derive(Clone, Copy)]
pub struct VideoPlaylistRef {
    pub ready: RwSignal<bool>,
    pub busy: RwSignal<bool>,
    pub status: RwSignal<String>,
    pub error: RwSignal<Option<String>>,
    /// Playlist-level end (last clip EOF, loop off) — not a single-clip `VideoRef::ended`.
    pub ended: RwSignal<bool>,
    /// Playlist play intent (UI). Stays true across Host seam `pause_hard` /
    /// slot EOF — do **not** mirror live `VideoRef::playing` (gap → Play icon flash).
    pub playing: RwSignal<bool>,
    /// Loop the whole playlist (clips themselves stay non-looping for seam).
    pub looping: RwSignal<bool>,
    /// Inclusive LoopZone start in **global** frames. Default 0.
    pub loop_in: RwSignal<u32>,
    /// Exclusive LoopZone end (global); `None` = full Σ frames.
    pub loop_out: RwSignal<Option<u32>>,
    pub model: RwSignal<PlaylistModel>,
    pub pos: RwSignal<PlaylistPos>,
    /// Scrub domain: global presentation frame ([`PlaylistModel::total_frame_count`]).
    pub playhead: RwSignal<u32>,
    pub frame_count: RwSignal<u32>,
    pub live_clip: RwSignal<u32>,
    pub idle_clip: RwSignal<Option<u32>>,
    /// When true, [`Self::slot_a`] is the live (visible) stage.
    pub live_is_a: RwSignal<bool>,
    /// Host scrub gesture in flight — mirror Effect must not overwrite `playhead`.
    pub scrubbing: RwSignal<bool>,
    /// Playlist-wide volume (both slots). Facade — not per-clip.
    pub volume: RwSignal<f64>,
    /// Playlist-wide mute (both slots).
    pub muted: RwSignal<bool>,
    /// Playlist-wide playback rate (both slots). Facade — not per-clip.
    pub playback_rate: RwSignal<f64>,
    /// Resume Play after scrub that crossed clips (per-slot `then_play` does not survive promote).
    pub(crate) scrub_resume: RwSignal<bool>,
    pub slot_a: VideoRef,
    pub slot_b: VideoRef,
    /// Set by the mounted [`super::VideoPlaylist`]; ops are no-ops / `not mounted` before.
    pub(crate) mounted: StoredValue<bool, LocalStorage>,
    pub(crate) sources: StoredValue<Vec<PlaylistSource>, LocalStorage>,
    pub(crate) open_gen: StoredValue<u64, LocalStorage>,
    /// Clip index already kicked for near-EOF origin warm this lap (`None` = none).
    pub(crate) seam_warm_for: StoredValue<Option<u32>, LocalStorage>,
    /// In-flight [`super::slots::ensure_clip_on_idle`] target — coalesce Move stampede.
    pub(crate) ensure_idle_for: StoredValue<Option<u32>, LocalStorage>,
    /// In-flight cut-join Exact warm `(idle_clip, join_local)` — one-shot coalesce.
    pub(crate) cut_join_warm_for: StoredValue<Option<(u32, u32)>, LocalStorage>,
    /// Cross-clip scrub: outgoing hard-paused once per gesture (not every Move).
    pub(crate) scrub_cross_paused: StoredValue<bool, LocalStorage>,
    /// Host cut/wrap seam phase — RwSignal so EOF Effect re-runs when Idle.
    pub(crate) seam_phase: RwSignal<super::loop_seam::SeamPhase>,
    /// VT ledger — Covered sole on VT; Exact write unused (slot blit).
    pub(crate) exact_ledger:
        StoredValue<Option<crate::virtual_timeline::LedgerHandle>, LocalStorage>,
    /// Bumps when VT Covered changes (TimelineMeta Covered subscribe).
    pub exact_residency_epoch: RwSignal<u64>,
    /// Per-clip sync FrameIds (local) for VT thumb progressive — survives idle rebind.
    pub(crate) thumb_sync_keys: StoredValue<HashMap<u32, Vec<u64>>, LocalStorage>,
    /// One VT progressive fill in flight (coalesce Effect kicks).
    pub(crate) thumb_fill_inflight: StoredValue<bool, LocalStorage>,
    /// Globals skipped after admit/cap refuse (must not spin; not “bitmap ready”).
    pub(crate) thumb_fill_skip: StoredValue<HashSet<u64>, LocalStorage>,
    /// Clips whose open-slot progressive walk finished this session (idle walk gate).
    pub(crate) thumb_clips_done: StoredValue<HashSet<u32>, LocalStorage>,
    /// VT global thumb bitmap wallet (≤1 producer). Feature `thumbs`.
    #[cfg(feature = "thumbs")]
    pub(crate) thumb_cache:
        StoredValue<Option<crate::virtual_timeline::ThumbCacheHandle>, LocalStorage>,
    /// One PreferSoftware thumb producer (not play idle).
    #[cfg(feature = "thumbs")]
    pub(crate) thumb_producer:
        StoredValue<Option<crate::virtual_timeline::ProducerHandle>, LocalStorage>,
    /// Bumps when VT thumb bitmaps change (HoverThumb re-paint).
    pub thumb_residency_epoch: RwSignal<u64>,
    /// Host thumb decode box (max edge). Progressive + hover share this.
    pub thumb_max_w: RwSignal<u32>,
    pub thumb_max_h: RwSignal<u32>,
}

impl VideoPlaylistRef {
    pub fn new() -> Self {
        Self {
            ready: RwSignal::new(false),
            busy: RwSignal::new(false),
            status: RwSignal::new(String::from("Load a playlist to begin.")),
            error: RwSignal::new(None),
            ended: RwSignal::new(false),
            playing: RwSignal::new(false),
            looping: RwSignal::new(false),
            loop_in: RwSignal::new(0),
            loop_out: RwSignal::new(None),
            model: RwSignal::new(PlaylistModel::default()),
            pos: RwSignal::new(PlaylistPos::new(0, 0)),
            playhead: RwSignal::new(0),
            frame_count: RwSignal::new(0),
            live_clip: RwSignal::new(0),
            idle_clip: RwSignal::new(None),
            live_is_a: RwSignal::new(true),
            scrubbing: RwSignal::new(false),
            // HTMLMediaElement default volume is 1.0.
            volume: RwSignal::new(1.0),
            muted: RwSignal::new(false),
            playback_rate: RwSignal::new(1.0),
            scrub_resume: RwSignal::new(false),
            slot_a: VideoRef::new(),
            slot_b: VideoRef::new(),
            mounted: StoredValue::new_local(false),
            sources: StoredValue::new_local(Vec::new()),
            open_gen: StoredValue::new_local(0),
            seam_warm_for: StoredValue::new_local(None),
            ensure_idle_for: StoredValue::new_local(None),
            cut_join_warm_for: StoredValue::new_local(None),
            scrub_cross_paused: StoredValue::new_local(false),
            seam_phase: RwSignal::new(super::loop_seam::SeamPhase::Idle),
            exact_ledger: StoredValue::new_local(
                Some(crate::virtual_timeline::new_ledger_handle()),
            ),
            exact_residency_epoch: RwSignal::new(0),
            thumb_sync_keys: StoredValue::new_local(HashMap::new()),
            thumb_fill_inflight: StoredValue::new_local(false),
            thumb_fill_skip: StoredValue::new_local(HashSet::new()),
            thumb_clips_done: StoredValue::new_local(HashSet::new()),
            #[cfg(feature = "thumbs")]
            thumb_cache: StoredValue::new_local(Some(
                crate::virtual_timeline::new_thumb_cache_handle(),
            )),
            #[cfg(feature = "thumbs")]
            thumb_producer: StoredValue::new_local(Some(
                crate::virtual_timeline::new_producer_handle(),
            )),
            thumb_residency_epoch: RwSignal::new(0),
            thumb_max_w: RwSignal::new(fpmedia_web::DEFAULT_THUMB_MAX_W),
            thumb_max_h: RwSignal::new(fpmedia_web::DEFAULT_THUMB_MAX_H),
        }
    }

    /// Thumb decode max box `(max_w, max_h)` — host API (hover + Pause progressive).
    pub fn thumb_max_size(self) -> (u32, u32) {
        (
            self.thumb_max_w.get_untracked().max(1),
            self.thumb_max_h.get_untracked().max(1),
        )
    }

    /// Set thumb decode max box. Clears VT + slot thumb caches so sizes do not mix;
    /// recomputes ImageBitmap residency from MiB ÷ maxsize.
    pub fn set_thumb_max_size(self, max_w: u32, max_h: u32) {
        let w = max_w.max(1);
        let h = max_h.max(1);
        if self.thumb_max_w.get_untracked() == w && self.thumb_max_h.get_untracked() == h {
            return;
        }
        self.thumb_max_w.set(w);
        self.thumb_max_h.set(h);
        self.slot_a.set_thumb_max_size(w, h);
        self.slot_b.set_thumb_max_size(w, h);
        #[cfg(feature = "thumbs")]
        if let Some(cache) = self.thumb_cache.get_value() {
            let cap =
                fpmedia_web::thumb_cache_frame_budget(w, h, fpmedia_web::DEFAULT_THUMB_CACHE_MIB);
            cache.borrow_mut().set_max_thumbs(cap);
        }
        super::thumbs::clear_thumb_sync_keys(self);
        super::thumbs::kick_vt_thumb_fill(self);
    }

    pub fn bump_thumb_residency(self) {
        self.thumb_residency_epoch
            .update(|n| *n = n.wrapping_add(1));
    }

    /// VT ledger (Covered + Fetch sole; Exact AimWindow parked).
    pub fn exact_ledger(self) -> Option<crate::virtual_timeline::LedgerHandle> {
        self.exact_ledger.get_value()
    }

    pub fn bump_exact_residency(self) {
        self.exact_residency_epoch
            .update(|n| *n = n.wrapping_add(1));
    }

    /// Clear VT Covered only (user Scrub Off). Idle park must not call this.
    /// Does **not** clear Fetch (bytes still playable).
    pub fn clear_vt_covered(self) {
        if let Some(h) = self.exact_ledger.get_value() {
            h.borrow_mut().clear_covered();
            self.bump_exact_residency();
        }
    }

    /// Reset VT ledger on open/close — Covered + Fetch + parked Exact values.
    pub fn reset_exact_ledger(self) {
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
        #[cfg(feature = "thumbs")]
        {
            if let Some(c) = self.thumb_cache.get_value() {
                c.borrow_mut().clear();
            } else {
                self.thumb_cache
                    .set_value(Some(crate::virtual_timeline::new_thumb_cache_handle()));
            }
            if let Some(p) = self.thumb_producer.get_value() {
                p.cancel();
            } else {
                self.thumb_producer
                    .set_value(Some(crate::virtual_timeline::new_producer_handle()));
            }
            self.bump_thumb_residency();
        }
    }

    /// Live (+ optional idle) scrub capability. Off wipes **playlist** VT Covered.
    pub fn set_scrub_cache_policy(self, policy: fpmedia_web::ScrubCachePolicy) {
        if matches!(policy, fpmedia_web::ScrubCachePolicy::Off) {
            self.clear_vt_covered();
        }
        self.live_video().set_scrub_cache_policy(policy);
    }

    pub fn set_loop(self, on: bool) {
        self.looping.set(on);
        // Intra-clip zone → engine LoopSeam on live; else host wrap.
        super::slots::sync_live_loop_from_playlist(self);
        // Loop on: idle → wrap / zone-in neighbor for origin stash warm.
        if on {
            self.repreload_idle_for_loop();
        }
    }

    /// Resolved half-open LoopZone in global frames.
    pub fn loop_zone(self) -> (u32, u32) {
        resolve_zone(
            self.frame_count.get_untracked(),
            self.loop_in.get_untracked(),
            self.loop_out.get_untracked(),
        )
    }

    /// Set half-open `[in, out)` in **global** presentation frames.
    /// Intra-live zone → map onto live slot engine loop; cross-clip → host wrap.
    pub fn set_loop_zone(self, in_frame: u32, out_frame: u32) {
        let count = self.frame_count.get_untracked();
        if count == 0 {
            self.loop_in.set(0);
            self.loop_out.set(None);
            super::slots::sync_live_loop_from_playlist(self);
            return;
        }
        let zone = store_zone(count, in_frame, out_frame);
        self.loop_in.set(zone.in_frame);
        self.loop_out.set(zone.loop_out);
        super::slots::sync_live_loop_from_playlist(self);
        if self.looping.get_untracked() {
            self.repreload_idle_for_loop();
        }
    }

    fn repreload_idle_for_loop(self) {
        let model = self.model.get_untracked();
        let live = self.live_clip.get_untracked();
        let (zone_in, zone_out) = self.loop_zone();
        let want = super::slots::idle_neighbor_for_zone(&model, live, true, zone_in, zone_out);
        if let Some(want) = want
            && self.idle_clip.get_untracked() != Some(want)
        {
            let epoch = self.open_gen.get_value();
            spawn_local(async move {
                preload_idle(self, Some(want), epoch).await;
            });
        }
    }

    /// Playlist-wide volume → both slots (live + idle).
    pub fn set_volume(self, volume: f64) {
        let v = volume.clamp(0.0, 1.0);
        self.volume.set(v);
        self.slot_a.set_volume(v);
        self.slot_b.set_volume(v);
    }

    /// Playlist-wide mute → both slots.
    pub fn set_muted(self, muted: bool) {
        self.muted.set(muted);
        self.apply_mute_to_slots();
    }

    /// Playlist-wide playback rate → both slots (live + idle).
    pub fn set_playback_rate(self, rate: f64) {
        let r = fpmedia_web::normalize_playback_rate(rate);
        self.playback_rate.set(r);
        self.slot_a.set_playback_rate(r);
        self.slot_b.set_playback_rate(r);
    }

    /// Re-push volume + mute + rate onto both `<Video>` slots (after open / swap).
    pub(crate) fn apply_gain_to_slots(self) {
        let v = self.volume.get_untracked();
        self.slot_a.set_volume(v);
        self.slot_b.set_volume(v);
        self.apply_mute_to_slots();
        self.apply_rate_to_slots();
    }

    fn apply_mute_to_slots(self) {
        let m = self.muted.get_untracked();
        self.live_video().set_muted(m);
        self.idle_video().set_muted(m);
    }

    fn apply_rate_to_slots(self) {
        let r = self.playback_rate.get_untracked();
        self.slot_a.set_playback_rate(r);
        self.slot_b.set_playback_rate(r);
    }

    /// Slot by liveness — `live_is_a` read untracked.
    pub fn live_video(self) -> VideoRef {
        self.slot_by_live(self.live_is_a.get_untracked())
    }

    pub fn idle_video(self) -> VideoRef {
        self.slot_by_live(!self.live_is_a.get_untracked())
    }

    /// Like [`Self::live_video`] but subscribes to `live_is_a` (Effects / memos).
    pub fn live_video_tracked(self) -> VideoRef {
        self.slot_by_live(self.live_is_a.get())
    }

    pub fn idle_video_tracked(self) -> VideoRef {
        self.slot_by_live(!self.live_is_a.get())
    }

    /// `true` → slot A, `false` → slot B.
    fn slot_by_live(self, is_a: bool) -> VideoRef {
        if is_a { self.slot_a } else { self.slot_b }
    }

    /// Host of the mounted component, if any (ops are no-ops before mount).
    fn host(self) -> Option<PlaylistHost> {
        self.mounted
            .try_with_value(|m| *m)
            .unwrap_or(false)
            .then_some(PlaylistHost(self))
    }

    pub fn open_blobs(self, blobs: Vec<web_sys::Blob>) {
        if let Some(host) = self.host() {
            host.open_blobs(blobs);
        } else {
            self.status
                .set("VideoPlaylist not mounted — hard-refresh the page.".into());
            self.error.set(Some("VideoPlaylist not mounted".into()));
        }
    }

    pub fn open_urls(self, urls: Vec<String>) {
        if let Some(host) = self.host() {
            host.open_urls(urls);
        } else {
            self.status
                .set("VideoPlaylist not mounted — hard-refresh the page.".into());
            self.error.set(Some("VideoPlaylist not mounted".into()));
        }
    }

    pub fn play(self) {
        self.ended.set(false);
        self.playing.set(true);
        if let Some(host) = self.host() {
            host.play();
        }
    }

    pub fn pause(self) {
        self.playing.set(false);
        if let Some(host) = self.host() {
            host.pause();
        }
    }

    pub fn close(self) {
        if let Some(host) = self.host() {
            host.close();
        }
    }

    /// Internal relocate on the global scrub axis. Prefer [`Self::goto_frame`] for UI.
    pub fn seek_frame(self, global_frame: u32) {
        self.ended.set(false);
        if let Some(host) = self.host() {
            host.seek_frame(global_frame);
        }
    }

    /// Internal relocate to a clip-local pos. Prefer [`Self::goto_pos`] for UI.
    pub fn seek_pos(self, pos: PlaylistPos) {
        self.ended.set(false);
        if let Some(host) = self.host() {
            host.seek_pos(pos);
        }
    }

    /// User goto (global frame): stop Play intent, ignore LoopZone. `glossary.md` § Goto.
    pub fn goto_frame(self, global_frame: u32) {
        self.stop_play_for_goto();
        self.seek_frame(global_frame);
    }

    /// User goto (clip + local): stop Play intent, ignore LoopZone. `glossary.md` § Goto.
    pub fn goto_pos(self, pos: PlaylistPos) {
        self.stop_play_for_goto();
        self.seek_pos(pos);
    }

    /// Facade Pause + hard-stop live immediately. `seek_to_pos` may await idle
    /// open — without this the old slot keeps Play while chrome shows Pause.
    fn stop_play_for_goto(self) {
        self.playing.set(false);
        self.scrub_resume.set(false);
        let live = self.live_video();
        if live.playing.get_untracked() || live.ended.get_untracked() {
            live.pause_hard_no_densify();
        }
    }

    pub fn next_frame(self) {
        if let Some(host) = self.host() {
            host.next_frame();
        }
    }

    pub fn previous_frame(self) {
        if let Some(host) = self.host() {
            host.previous_frame();
        }
    }

    /// Idle clip for the cut nearest to `aim` (sticky = current idle).
    fn cut_idle_want(self, aim: PlaylistPos) -> Option<u32> {
        self.model.get_untracked().idle_neighbor_near_cut(
            aim.clip_index,
            aim.local_frame,
            self.looping.get_untracked(),
            self.idle_clip.get_untracked(),
        )
    }

    /// Timeline scrub units (global frames). Promotes idle when it already holds
    /// the aim clip; settle (`Up`) async-seeks if the clip still needs open/rebind.
    pub(crate) fn scrub_timeline(self, global_frame: u32, phase: ScrubPhase) {
        self.ended.set(false);
        match phase {
            ScrubPhase::Down => {
                self.scrubbing.set(true);
                self.scrub_cross_paused.set_value(false);
                // Latch playlist intent (not slot `playing` — soft-pause / seam
                // hard-stop clear the slot while facade stays true).
                // Per-slot `then_play` does not survive promote — playlist owns resume.
                self.scrub_resume.set(self.playing.get_untracked());
            }
            ScrubPhase::Move => self.scrubbing.set(true),
            // Clear scrubbing after playhead/pos aim — else mirror can run on a
            // stale slot `frame` and yank the tip before settle. **no equivalent**.
            ScrubPhase::Up => {
                self.scrub_cross_paused.set_value(false);
            }
        }
        self.playhead.set(global_frame);
        let Some(aim) = self.model.get_untracked().at_global_frame(global_frame) else {
            if matches!(phase, ScrubPhase::Up) {
                self.scrubbing.set(false);
            }
            return;
        };
        self.pos.set(aim);
        if matches!(phase, ScrubPhase::Up) {
            self.scrubbing.set(false);
        }

        let mut promoted = false;
        if aim.clip_index != self.live_clip.get_untracked() {
            // Cross-clip: hard-pause outgoing once per gesture (not every Move).
            // `PausePressed` alone is wrong: Ready → no-op; Playing → SoftPause densify.
            // **no equivalent** (playlist scrub local).
            if matches!(phase, ScrubPhase::Down | ScrubPhase::Move)
                && !self.scrub_cross_paused.get_value()
            {
                let outgoing = self.live_video();
                outgoing.pause_hard_no_densify();
                outgoing.pause();
                self.scrub_cross_paused.set_value(true);
            }
            promoted = try_promote_idle(self, aim.clip_index);
            if promoted {
                // ensure park leaves Host Off until swap On — re-assert after promote.
                let live = self.live_video();
                live.set_scrub_cache_policy(fpmedia_web::ScrubCachePolicy::On);
                // Keep the other clip of the nearest cut in idle (1|2 or 2|3).
                if let Some(want) = self.cut_idle_want(aim) {
                    let epoch = self.open_gen.get_value();
                    spawn_local(async move {
                        preload_idle(self, Some(want), epoch).await;
                    });
                }
            } else if matches!(phase, ScrubPhase::Down | ScrubPhase::Move)
                && self.idle_clip.get_untracked() != Some(aim.clip_index)
            {
                kick_ensure_idle(self, aim.clip_index);
            }
        } else if matches!(phase, ScrubPhase::Down | ScrubPhase::Move) {
            // Same clip: warm idle for nearest cut (near 1|2 → clip 1, near 2|3 → 3).
            if let Some(want) = self.cut_idle_want(aim) {
                kick_ensure_idle(self, want);
            }
        }

        // Paused scrub ignores LoopZone. Scrub-while-play Up → `plan_scrub_up`
        // (Host outside → wrap, no Exact@outside). **no equivalent**.
        if aim.clip_index == self.live_clip.get_untracked() {
            let live = self.live_video();
            match phase {
                ScrubPhase::Down => live.scrub_pointer_down(aim.local_frame),
                // After promote the new live never saw PointerDown — Move alone is a no-op.
                ScrubPhase::Move if promoted => live.scrub_pointer_down(aim.local_frame),
                ScrubPhase::Move => live.scrub_pointer_move(aim.local_frame),
                ScrubPhase::Up => {
                    let resume = self.scrub_resume.get_untracked();
                    self.scrub_resume.set(false);
                    self.run_scrub_up_plan(global_frame, resume, true, aim);
                }
            }
            return;
        }

        // Aim clip not live yet — settle via internal seek (open/swap).
        if matches!(phase, ScrubPhase::Up) {
            let resume = self.scrub_resume.get_untracked();
            self.scrub_resume.set(false);
            self.run_scrub_up_plan(global_frame, resume, false, aim);
        }
    }

    /// Execute [`plan_scrub_up`] — single owner for Up outside Host zone.
    fn run_scrub_up_plan(
        self,
        global_frame: u32,
        resume: bool,
        aim_on_live: bool,
        aim: PlaylistPos,
    ) {
        let (zone_in, zone_out) = self.loop_zone();
        let plan = plan_scrub_up(ScrubUpCtx {
            global: global_frame,
            resume,
            aim_on_live,
            owner: loop_owner(self),
            looping: self.looping.get_untracked(),
            zone_in,
            zone_out,
            total_frames: self.frame_count.get_untracked(),
            seam_busy: seam_busy(self),
        });
        match plan {
            ScrubUpPlan::HostWrap { seam_already_busy } => {
                // Leave Scrubbing without Exact@outside, then shared Host wrap.
                self.live_video().pause();
                start_host_wrap(self, seam_already_busy);
            }
            ScrubUpPlan::LiveSettle { then_play } => {
                let live = self.live_video();
                live.scrub_pointer_up(aim.local_frame);
                if then_play {
                    // Host-aware Play — not bare slot `play()` (skips wrap).
                    self.play();
                } else {
                    // Reverse-cut settle: re-assert On + clear ExactHold + densify
                    // (PresentHit/cancel storms left strip empty). **no equivalent**.
                    live.set_scrub_cache_policy(fpmedia_web::ScrubCachePolicy::On);
                    live.clear_densify_hold();
                    live.arm_scrub_densify_after_exact(aim.local_frame);
                }
                // Keep cut-pair idle (near 1|2 → other of pair); Play EOF still
                // seam-warms next when near wall.
                let neighbor = self.cut_idle_want(aim).or_else(|| {
                    idle_neighbor(
                        &self.model.get_untracked(),
                        self.live_clip.get_untracked(),
                        self.looping.get_untracked(),
                    )
                });
                let epoch = self.open_gen.get_value();
                spawn_local(async move {
                    preload_idle(self, neighbor, epoch).await;
                    kick_cut_join_exact_warm(self);
                });
                // Drag may have opened idle while scrubbing blocked warm — retry on Up.
                kick_cut_join_exact_warm(self);
            }
            ScrubUpPlan::CrossClipSeek { then_play } => {
                if then_play {
                    let epoch = self.open_gen.get_value();
                    spawn_local(async move {
                        if let Err(e) = super::slots::seek_to_pos(self, aim, epoch, true).await {
                            if self.open_gen.get_value() != epoch {
                                return;
                            }
                            if super::host_zone::is_soft_seam_cancel(&e) {
                                return;
                            }
                            self.error.set(Some(e));
                            self.status.set("Seek failed.".into());
                        }
                    });
                } else {
                    self.seek_pos(aim);
                }
            }
        }
    }

    pub(crate) fn paint_hover_thumb(
        self,
        global_frame: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) {
        if self.try_paint_hover_thumb(global_frame, dest.clone(), max_w, max_h) {
            return;
        }
        let Some(pos) = self.model.get_untracked().at_global_frame(global_frame) else {
            return;
        };
        // On-slot Pause: slot Exact refine, then import into VT.
        if let Some(video) = self.video_for_clip(pos.clip_index) {
            video.paint_hover_thumb(pos.local_frame, dest, max_w, max_h);
            #[cfg(feature = "thumbs")]
            if let Some(bmp) = video.clone_thumb_bitmap(pos.local_frame, max_w, max_h)
                && let Some(cache) = self.thumb_cache.get_value()
            {
                let tw = bmp.width().max(1);
                let th = bmp.height().max(1);
                if cache
                    .borrow_mut()
                    .put(u64::from(global_frame), bmp, tw, th, u16::MAX)
                {
                    self.bump_thumb_residency();
                }
            }
            return;
        }
        // Off-slot: producer fill (Pause only) — never steal play idle.
        #[cfg(feature = "thumbs")]
        super::thumbs::kick_hover_fill(self, global_frame, max_w, max_h);
    }

    pub(crate) fn try_paint_hover_thumb(
        self,
        global_frame: u32,
        dest: HtmlCanvasElement,
        max_w: u32,
        max_h: u32,
    ) -> bool {
        #[cfg(feature = "thumbs")]
        if let Some(cache) = self.thumb_cache.get_value() {
            let c = cache.borrow();
            // Prefer exact size; fall back to any stored size for interim.
            if crate::virtual_timeline::try_paint_vt(
                &c,
                u64::from(global_frame),
                &dest,
                max_w,
                max_h,
                0,
            )
            .is_some()
            {
                return true;
            }
        }
        let Some(pos) = self.model.get_untracked().at_global_frame(global_frame) else {
            return false;
        };
        let Some(video) = self.video_for_clip(pos.clip_index) else {
            return false;
        };
        video.try_paint_hover_thumb(pos.local_frame, dest, max_w, max_h)
    }

    pub(crate) fn cancel_hover_thumb(self) {
        self.slot_a.cancel_hover_thumb();
        self.slot_b.cancel_hover_thumb();
        #[cfg(feature = "thumbs")]
        if let Some(p) = self.thumb_producer.get_value() {
            // Do not cancel producer epoch on every leave — only clear slot hover.
            let _ = p;
        }
    }

    /// Slot that owns `clip_index`, if already open on live or idle.
    fn video_for_clip(self, clip_index: u32) -> Option<VideoRef> {
        if self.live_clip.get_untracked() == clip_index {
            return Some(self.live_video());
        }
        if self.idle_clip.get_untracked() == Some(clip_index) {
            return Some(self.idle_video());
        }
        None
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ScrubPhase {
    Down,
    Move,
    Up,
}

impl Default for VideoPlaylistRef {
    fn default() -> Self {
        Self::new()
    }
}
