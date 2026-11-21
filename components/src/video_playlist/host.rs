//! Mounted-`<VideoPlaylist>` host: the logic [`super::VideoPlaylistRef`] forwards to.
//!
//! Replaces the old `VideoPlaylistOps` struct of `Rc<dyn Fn>` — every op only
//! needs the (`Copy`) ref, so the host is a zero-cost newtype and the ref keeps
//! a plain `mounted` latch. `architecture.md` § Code rules → Ownership & types.

use fpmedia_web::ScrubCachePolicy;
use fpmedia_web::timer::sleep_ms;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::frame_axis::{ClipFrame, GlobalFrame};
use crate::video_playlist::{PlaylistClip, PlaylistModel, PlaylistPos};
use fpmedia_web::Ticket;

use super::host_zone::is_soft_seam_cancel;
use super::latches::HostLatches;
use super::loop_seam::{LoopOwner, end_seam, loop_owner, seam_busy};
use super::loop_seam_exec::{note_host_seam_err, start_host_wrap};
use super::plan::{
    HostOutsideCtx, HostOutsidePlan, PlayAtClipLastCtx, PlayAtClipLastPlan, SeamNextCtx, StepCtx,
    StepDir, StepPlan, plan_host_outside, plan_play_at_clip_last, plan_seam_next_clip, plan_step,
};
use super::slots::{seek_to_pos, sync_live_loop_from_playlist, wait_ready};
use super::source::PlaylistSource;
use super::video_playlist_ref::VideoPlaylistRef;

/// Host logic for one mounted playlist (see module docs).
#[derive(Clone, Copy)]
pub(crate) struct PlaylistHost(pub(super) VideoPlaylistRef);

/// Run [`StepPlan`] from Next/Prev chrome (clear intent / live step / goto).
fn run_step_plan(pl: VideoPlaylistRef, plan: StepPlan) {
    match plan {
        StepPlan::LiveStep { dir } => {
            pl.playing.set(false);
            pl.scrub_resume.set(false);
            match dir {
                StepDir::Next => pl.live_video().next_frame(),
                StepDir::Prev => pl.live_video().previous_frame(),
            }
        }
        StepPlan::GotoPos { pos } => pl.goto_pos(pos),
        StepPlan::ClearPlayIntent => {
            pl.playing.set(false);
            pl.scrub_resume.set(false);
        }
    }
}

impl PlaylistHost {
    /// Bump the open generation and clear per-gen coalesce latches.
    pub(crate) fn bump_gen(self) -> Ticket {
        let pl = self.0;
        let g = pl.open_gen.bump();
        pl.latches.update_value(HostLatches::reset_for_new_gen);
        g
    }

    pub(crate) fn close(self) {
        let pl = self.0;
        let _ = self.bump_gen();
        pl.slot_a.close();
        pl.slot_b.close();
        pl.ready.set(false);
        pl.busy.set(false);
        pl.ended.set(false);
        pl.playing.set(false);
        pl.model.set(PlaylistModel::default());
        pl.pos.set(PlaylistPos::new(0, ClipFrame::from_source(0)));
        pl.playhead.set(0);
        pl.frame_count.set(0);
        pl.loop_in.set(0);
        pl.loop_out.set(None);
        pl.live_clip.set(0);
        pl.idle_clip.set(None);
        pl.live_is_a.set(true);
        pl.scrubbing.set(false);
        pl.scrub_resume.set(false);
        // `bump_gen` above cleared the per-gen latches; `seam_warm_for` is per-lap.
        pl.latches.update_value(|l| l.seam_warm_for = None);
        end_seam(pl);
        super::thumbs::clear_thumb_sync_keys(pl);
        pl.sources.set_value(Vec::new());
        pl.error.set(None);
        pl.status.set("Closed.".into());
    }

    fn start_open(self, list: Vec<PlaylistSource>) {
        let playlist = self.0;
        let VideoPlaylistRef {
            ready,
            busy,
            status,
            error,
            ended,
            model,
            pos,
            playhead,
            frame_count,
            live_clip,
            idle_clip,
            live_is_a,
            slot_a,
            slot_b,
            sources,
            open_gen,
            ..
        } = playlist;
        if list.is_empty() {
            error.set(Some("empty playlist".into()));
            status.set("Add at least one file or URL.".into());
            return;
        }
        self.close();
        let epoch = self.bump_gen();
        sources.set_value(list.clone());
        busy.set(true);
        ready.set(false);
        error.set(None);
        status.set(format!("Probing playlist (0/{})…", list.len()));

        spawn_local(async move {
            let mut clips = Vec::with_capacity(list.len());
            // Probe every clip on slot_a (sequential open → metadata → close).
            for (i, src) in list.iter().enumerate() {
                if !open_gen.is_current(epoch) {
                    return;
                }
                status.set(format!("Probing playlist ({}/{})…", i + 1, list.len()));
                slot_a.close();
                // Let retire settle one tick.
                sleep_ms(0).await;
                if !open_gen.is_current(epoch) {
                    return;
                }
                src.open_on(slot_a);
                match wait_ready(slot_a, epoch, open_gen).await {
                    Ok(meta) => clips.push(PlaylistClip::from_meta(&meta)),
                    Err(e) => {
                        if !open_gen.is_current(epoch) {
                            return;
                        }
                        busy.set(false);
                        ready.set(false);
                        error.set(Some(e.clone()));
                        status.set(format!("Probe failed at clip {}.", i + 1));
                        slot_a.close();
                        return;
                    }
                }
            }

            if !open_gen.is_current(epoch) {
                return;
            }

            let built = PlaylistModel::new(clips);
            let total_frames = built.total_frame_count();
            model.set(built);
            frame_count.set(total_frames);
            playlist.loop_in.set(0);
            playlist.loop_out.set(None);
            playhead.set(0);
            pos.set(PlaylistPos::new(0, ClipFrame::from_source(0)));
            live_is_a.set(true);
            live_clip.set(0);
            idle_clip.set(if list.len() > 1 { Some(1) } else { None });

            slot_a.close();
            slot_b.close();
            sleep_ms(0).await;
            if !open_gen.is_current(epoch) {
                return;
            }

            playlist.reset_exact_ledger();
            super::thumbs::clear_thumb_sync_keys(playlist);
            status.set("Opening live clip…".into());
            list[0].open_on(slot_a);
            slot_a.set_scrub_cache_policy(ScrubCachePolicy::On);
            // ThumbPolicy: host Effect applies to both slots (do not force idle Off).

            if list.len() > 1 {
                status.set("Opening idle clip…".into());
                list[1].open_on(slot_b);
                slot_b.park_scrub_cache();
            }
            slot_a.set_vt_densify_authority(false);
            slot_b.set_vt_densify_authority(false);
            slot_a.set_vt_thumb_authority(true);
            slot_b.set_vt_thumb_authority(true);

            match wait_ready(slot_a, epoch, open_gen).await {
                Ok(_) => {
                    if !open_gen.is_current(epoch) {
                        return;
                    }
                    // Idle may still be opening — do not block ready on it.
                    busy.set(false);
                    ready.set(true);
                    ended.set(false);
                    playlist.apply_gain_to_slots();
                    sync_live_loop_from_playlist(playlist);
                    status.set(format!("Playlist ready · {} clip(s).", list.len()));
                    error.set(None);
                }
                Err(e) => {
                    if !open_gen.is_current(epoch) {
                        return;
                    }
                    busy.set(false);
                    ready.set(false);
                    error.set(Some(e));
                    status.set("Live open failed.".into());
                }
            }
        });
    }

    pub(crate) fn open_blobs(self, blobs: Vec<web_sys::Blob>) {
        self.start_open(blobs.into_iter().map(PlaylistSource::Blob).collect());
    }

    pub(crate) fn open_urls(self, urls: Vec<String>) {
        let list = urls
            .into_iter()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
            .map(PlaylistSource::Url)
            .collect::<Vec<_>>();
        self.start_open(list);
    }

    pub(crate) fn play(self) {
        let playlist = self.0;
        let VideoPlaylistRef {
            ended,
            looping,
            playhead,
            frame_count,
            live_clip,
            open_gen,
            ..
        } = playlist;
        ended.set(false);
        playlist.playing.set(true);
        let live = playlist.live_video();
        sync_live_loop_from_playlist(playlist);
        // Host keeps slot `looping=false` — bare `live.play()` at clip/zone end
        // snaps PlayPrime → local 0. Outside → `plan_host_outside`. **no equivalent**.
        if looping.get_untracked() {
            let (zone_in, zone_out) = playlist.loop_zone();
            let g = playhead.get_untracked();
            let total = frame_count.get_untracked();
            match plan_host_outside(HostOutsideCtx {
                owner: loop_owner(playlist),
                looping: true,
                zone_in,
                zone_out,
                total_frames: total,
                global: g,
                seam_busy: seam_busy(playlist),
                past_out_needs_wall_on_live: false,
                wall_on_live: false,
            }) {
                HostOutsidePlan::HostWrap { seam_already_busy } => {
                    start_host_wrap(playlist, seam_already_busy);
                    return;
                }
                HostOutsidePlan::EngineSlotPlay => {
                    live.play();
                    return;
                }
                HostOutsidePlan::NotOutside => {}
            }
            let clip_frames = live.frame_count.get_untracked();
            let at_clip_last =
                clip_frames > 0 && live.playhead.get_untracked() >= clip_frames.saturating_sub(1);
            if at_clip_last && matches!(loop_owner(playlist), LoopOwner::Host) {
                // Still inside zone but clip last — same next/mid-in law as EOF.
                let cur = live_clip.get_untracked();
                let model = playlist.model.get_untracked();
                let zone_full = zone_in == 0 && zone_out == total;
                let next_clip = model.next_clip_index(cur);
                let next_start = next_clip
                    .and_then(|n| model.clip_start_frame(n).map(GlobalFrame::get))
                    .unwrap_or(0);
                let zone_in_aim = model
                    .at_global_frame(GlobalFrame::new(zone_in))
                    .map(|p| (p.clip_index, p.frame.source()));
                let seam_next = plan_seam_next_clip(SeamNextCtx {
                    cur,
                    next_clip,
                    next_start,
                    zone_in_clip: zone_in_aim.map(|(c, _)| c),
                    looping: true,
                    zone_full,
                    zone_out,
                    clip_count: model.len() as u32,
                });
                match plan_play_at_clip_last(PlayAtClipLastCtx {
                    zone_full,
                    zone_in_aim,
                    seam_next,
                    seam_busy: seam_busy(playlist),
                }) {
                    PlayAtClipLastPlan::PromoteSeek { clip_index } => {
                        let epoch = open_gen.ticket();
                        let pl = playlist;
                        let aim = PlaylistPos::new(clip_index, ClipFrame::from_source(0));
                        spawn_local(async move {
                            if let Err(e) = seek_to_pos(pl, aim, epoch, true).await {
                                note_host_seam_err(pl, epoch, e, "Playlist seam failed.");
                            }
                        });
                    }
                    PlayAtClipLastPlan::HostWrap { seam_already_busy } => {
                        start_host_wrap(playlist, seam_already_busy);
                    }
                }
                return;
            }
        }
        live.play();
    }

    pub(crate) fn pause(self) {
        let playlist = self.0;
        playlist.playing.set(false);
        playlist.live_video().pause();
    }

    // Internal relocate ops — no facade stop. User chrome uses `goto_*` on the ref.

    pub(crate) fn seek_frame(self, global_frame: u32) {
        let pl = self.0;
        if !pl.ready.get_untracked() {
            return;
        }
        let Some(aim) = pl
            .model
            .get_untracked()
            .at_global_frame(GlobalFrame::new(global_frame))
        else {
            return;
        };
        let epoch = pl.open_gen.ticket();
        spawn_local(async move {
            if let Err(e) = seek_to_pos(pl, aim, epoch, false).await {
                if !pl.open_gen.is_current(epoch) {
                    return;
                }
                // Promote/ensure race cancel — not a sticky UI fault.
                if is_soft_seam_cancel(&e) {
                    return;
                }
                pl.error.set(Some(e));
                pl.status.set("Seek failed.".into());
            }
        });
    }

    pub(crate) fn seek_pos(self, aim: PlaylistPos) {
        let pl = self.0;
        if !pl.ready.get_untracked() {
            return;
        }
        let epoch = pl.open_gen.ticket();
        spawn_local(async move {
            if let Err(e) = seek_to_pos(pl, aim, epoch, false).await {
                if !pl.open_gen.is_current(epoch) {
                    return;
                }
                if is_soft_seam_cancel(&e) {
                    return;
                }
                pl.error.set(Some(e));
                pl.status.set("Seek failed.".into());
            }
        });
    }

    // Goto/Next/Prev/paused scrub ignore LoopZone; scrub-while-play resume is
    // Play (outside → loop_in). Zone is not a nav clamp. **no equivalent**.
    // Next/Prev: [`plan_step`] → LiveStep / GotoPos / ClearPlayIntent.

    pub(crate) fn next_frame(self) {
        let pl = self.0;
        if !pl.ready.get_untracked() {
            return;
        }
        pl.ended.set(false);
        let model = pl.model.get_untracked();
        let clip_i = pl.live_clip.get_untracked();
        let local = pl.live_video().frame.get_untracked();
        let Some(clip) = model.clip(clip_i as usize) else {
            return;
        };
        let plan = plan_step(StepCtx {
            dir: StepDir::Next,
            clip_index: clip_i,
            frame: ClipFrame::from_source(local),
            clip_last_frame: ClipFrame::new(clip.last_frame()),
            clip_count: model.len() as u32,
            looping: pl.looping.get_untracked(),
            edge_target_local: ClipFrame::new(0),
        });
        run_step_plan(pl, plan);
    }

    pub(crate) fn previous_frame(self) {
        let pl = self.0;
        if !pl.ready.get_untracked() {
            return;
        }
        pl.ended.set(false);
        let model = pl.model.get_untracked();
        let clip_i = pl.live_clip.get_untracked();
        let local = pl.live_video().frame.get_untracked();
        let Some(clip) = model.clip(clip_i as usize) else {
            return;
        };
        let edge_target_local = if local > 0 {
            0
        } else if clip_i > 0 {
            model
                .clip((clip_i - 1) as usize)
                .map(|c| c.last_frame())
                .unwrap_or(0)
        } else {
            let last_i = model.len().saturating_sub(1);
            model.clip(last_i).map(|c| c.last_frame()).unwrap_or(0)
        };
        let plan = plan_step(StepCtx {
            dir: StepDir::Prev,
            clip_index: clip_i,
            frame: ClipFrame::from_source(local),
            clip_last_frame: ClipFrame::new(clip.last_frame()),
            clip_count: model.len() as u32,
            looping: pl.looping.get_untracked(),
            edge_target_local: ClipFrame::new(edge_target_local),
        });
        run_step_plan(pl, plan);
    }
}
