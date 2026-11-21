//! Playlist Pause progressive thumbs on the VT bitmap wallet.
//!
//! One PreferSoftware producer (`ThumbProducer`) fills [`VtThumbCache`] by
//! global FrameId. Never `ensure_clip_on_idle` (that stole Play idle). On-slot
//! clips reuse slot `fill_thumb_key_at` + clone into VT; off-slot → dedicated
//! `open_mp4` track. Ready = VT cache hit. **no equivalent** (filmstrip local).
//! Decode: `mediabunny:` CanvasSink.

use leptos::prelude::*;
#[cfg(feature = "thumbs")]
use wasm_bindgen_futures::spawn_local;

#[cfg(feature = "thumbs")]
use crate::frame_axis::GlobalFrame;
#[cfg(feature = "thumbs")]
use crate::virtual_timeline::{
    ThumbFillPlanInput, merge_global_sync_keys, plan_next_thumb_fill, rank_for_global_key,
    resolve_thumb_fill_playlist,
};
#[cfg(feature = "thumbs")]
use fpmedia_web::FrameId;

use super::latches::ThumbFillState;
use super::video_playlist_ref::VideoPlaylistRef;

/// Host thumb decode box (API [`VideoPlaylistRef::thumb_max_size`]).
#[cfg(feature = "thumbs")]
fn thumb_box(pl: VideoPlaylistRef) -> (u32, u32) {
    pl.thumb_max_size()
}

fn fill_inflight(pl: VideoPlaylistRef) -> bool {
    pl.thumb_fill.with_value(|s| s.inflight)
}

#[cfg(feature = "thumbs")]
fn set_fill_inflight(pl: VideoPlaylistRef, inflight: bool) {
    pl.thumb_fill.update_value(|s| s.inflight = inflight);
}

/// Refresh per-clip sync key cache from open live/idle **and** producer clip.
#[cfg(feature = "thumbs")]
pub(crate) fn refresh_thumb_sync_keys(pl: VideoPlaylistRef) {
    let mut cache = pl.thumb_fill.with_value(|s| s.sync_keys.clone());
    let live_clip = pl.live_clip.get_untracked();
    let live = pl.live_video();
    if live.ready.get_untracked() {
        let keys = live.sync_frame_ids();
        if !keys.is_empty() {
            cache.insert(live_clip, keys);
        }
    }
    if let Some(idle_idx) = pl.idle_clip.get_untracked() {
        let idle = pl.idle_video();
        if idle.ready.get_untracked() {
            let keys = idle.sync_frame_ids();
            if !keys.is_empty() {
                cache.insert(idle_idx, keys);
            }
        }
    }
    #[cfg(feature = "thumbs")]
    if let Some(prod) = pl.thumb_producer.get_value()
        && let Some(clip) = prod.current_clip()
    {
        let keys = prod.sync_frame_ids();
        if !keys.is_empty() {
            cache.insert(clip, keys);
        }
    }
    pl.thumb_fill.update_value(|s| s.sync_keys = cache);
}

/// Kick one VT progressive step (any clip via producer). No-op while Play/scrub.
pub(crate) fn kick_vt_thumb_fill(pl: VideoPlaylistRef) {
    if !pl.ready.get_untracked() {
        return;
    }
    if pl.playing.get_untracked() || pl.scrubbing.get_untracked() {
        return;
    }
    if fill_inflight(pl) {
        return;
    }
    if seam_busy_for_thumbs(pl) {
        return;
    }
    #[cfg(not(feature = "thumbs"))]
    {
        let _ = pl;
    }
    #[cfg(feature = "thumbs")]
    {
        let Some(cache_h) = pl.thumb_cache.get_value() else {
            return;
        };
        let Some(prod_h) = pl.thumb_producer.get_value() else {
            return;
        };

        refresh_thumb_sync_keys(pl);

        let model = pl.model.get_untracked();
        if model.is_empty() {
            return;
        }

        // Discover sync keys for clips we have not opened yet (one clip per kick).
        let key_cache = pl.thumb_fill.with_value(|s| s.sync_keys.clone());
        let clip_count = model.len() as u32;
        if let Some(missing) = (0..clip_count).find(|i| !key_cache.contains_key(i)) {
            set_fill_inflight(pl, true);
            let epoch = pl.open_gen.ticket();
            spawn_local(async move {
                let ok = prod_h.ensure_clip(pl, missing).await.is_ok();
                if !pl.open_gen.is_current(epoch) {
                    set_fill_inflight(pl, false);
                    return;
                }
                if ok {
                    refresh_thumb_sync_keys(pl);
                } else {
                    pl.thumb_fill.update_value(|s| {
                        s.clips_done.insert(missing);
                    });
                }
                set_fill_inflight(pl, false);
                kick_vt_thumb_fill(pl);
            });
            return;
        }

        let mut open_per_clip: Vec<(u32, Vec<FrameId>)> = Vec::with_capacity(key_cache.len());
        for (clip, keys) in &key_cache {
            open_per_clip.push((*clip, keys.clone()));
        }
        open_per_clip.sort_by_key(|(c, _)| *c);
        let global_keys = merge_global_sync_keys(&model, &open_per_clip);
        if global_keys.is_empty() {
            return;
        }

        let max_thumbs = cache_h.borrow().max_thumbs();
        let skip = pl.thumb_fill.with_value(|s| s.skip.clone());
        let ready = |g: FrameId| skip.contains(&g) || cache_h.borrow().has_any(GlobalFrame::new(g));

        let input = ThumbFillPlanInput {
            global_sync_keys: &global_keys,
            duration_secs: model.total_duration_secs(),
            max_thumbs,
        };

        let Some(global) = plan_next_thumb_fill(&input, ready) else {
            return;
        };
        let Some(aim) = resolve_thumb_fill_playlist(&model, GlobalFrame::new(global)) else {
            return;
        };

        let rank = rank_for_global_key(
            &global_keys,
            global,
            model.total_duration_secs(),
            max_thumbs,
        );
        let (max_w, max_h) = thumb_box(pl);
        set_fill_inflight(pl, true);
        let epoch = pl.open_gen.ticket();
        spawn_local(async move {
            let filled = fill_one(FillOne {
                pl,
                cache_h,
                prod_h,
                clip_index: aim.clip_index,
                local: aim.frame.source(),
                global,
                max_w,
                max_h,
                rank,
            })
            .await;
            if !pl.open_gen.is_current(epoch) {
                set_fill_inflight(pl, false);
                return;
            }
            if filled {
                if let Some(h) = pl.exact_ledger() {
                    h.borrow_mut().note_thumb(GlobalFrame::new(global));
                }
                pl.bump_thumb_residency();
            } else {
                pl.thumb_fill.update_value(|s| {
                    s.skip.insert(global);
                });
            }
            set_fill_inflight(pl, false);
            kick_vt_thumb_fill(pl);
        });
    }
}

#[cfg(feature = "thumbs")]
struct FillOne {
    pl: VideoPlaylistRef,
    cache_h: crate::virtual_timeline::ThumbCacheHandle,
    prod_h: crate::virtual_timeline::ProducerHandle,
    clip_index: u32,
    local: u32,
    global: FrameId,
    max_w: u32,
    max_h: u32,
    rank: u16,
}

#[cfg(feature = "thumbs")]
async fn fill_one(args: FillOne) -> bool {
    let FillOne {
        pl,
        cache_h,
        prod_h,
        clip_index,
        local,
        global,
        max_w,
        max_h,
        rank,
    } = args;
    // On-slot: reuse slot PreferSoftware sink, then clone into VT.
    if let Some(video) = video_for_clip(pl, clip_index)
        && video.ready.get_untracked()
    {
        video.fill_thumb_key_at(local).await;
        if let Some(bmp) = video.clone_thumb_bitmap(local, max_w, max_h) {
            let (tw, th) = (bmp.width().max(1), bmp.height().max(1));
            return cache_h
                .borrow_mut()
                .put(GlobalFrame::new(global), bmp, tw, th, rank);
        }
    }
    // Off-slot (or slot miss): dedicated producer open.
    if prod_h.ensure_clip(pl, clip_index).await.is_err() {
        return false;
    }
    matches!(
        prod_h
            .decode_into_vt(&cache_h, global, FrameId::from(local), max_w, max_h, rank)
            .await,
        Ok(true)
    )
}

#[cfg(feature = "thumbs")]
fn video_for_clip(pl: VideoPlaylistRef, clip_index: u32) -> Option<crate::VideoRef> {
    if pl.live_clip.get_untracked() == clip_index {
        return Some(pl.live_video());
    }
    if pl.idle_clip.get_untracked() == Some(clip_index) {
        return Some(pl.idle_video());
    }
    None
}

fn seam_busy_for_thumbs(pl: VideoPlaylistRef) -> bool {
    !matches!(
        pl.seam_phase.get_untracked(),
        super::loop_seam::SeamPhase::Idle
    )
}

/// Wire Pause progressive: Effect kick when Pause / slots ready.
pub(crate) fn install_vt_thumb_effects(pl: VideoPlaylistRef) {
    Effect::new(move |_| {
        let ready = pl.ready.get();
        let playing = pl.playing.get();
        let scrubbing = pl.scrubbing.get();
        let _live = pl.live_clip.get();
        let _idle = pl.idle_clip.get();
        let _live_is_a = pl.live_is_a.get();
        let _seam = pl.seam_phase.get();
        let _a = pl.slot_a.ready.get();
        let _b = pl.slot_b.ready.get();
        if !ready || playing || scrubbing {
            return;
        }
        kick_vt_thumb_fill(pl);
    });
}

/// Clear sync-key cache + VT thumb wallet on playlist close/open.
pub(crate) fn clear_thumb_sync_keys(pl: VideoPlaylistRef) {
    pl.thumb_fill.set_value(ThumbFillState::default());
    if let Some(h) = pl.exact_ledger() {
        h.borrow_mut().clear_thumbs();
    }
    #[cfg(feature = "thumbs")]
    {
        if let Some(c) = pl.thumb_cache.get_value() {
            c.borrow_mut().clear();
        }
        if let Some(p) = pl.thumb_producer.get_value() {
            p.cancel();
        }
        pl.bump_thumb_residency();
    }
}

/// Hover: schedule producer fill for off-slot aim (Pause only). Never steals idle.
#[cfg(feature = "thumbs")]
pub(crate) fn kick_hover_fill(pl: VideoPlaylistRef, global_frame: u32, max_w: u32, max_h: u32) {
    if pl.playing.get_untracked() || pl.scrubbing.get_untracked() {
        return;
    }
    if fill_inflight(pl) {
        return;
    }
    let Some(pos) = pl
        .model
        .get_untracked()
        .at_global_frame(GlobalFrame::new(global_frame))
    else {
        return;
    };
    let Some(cache_h) = pl.thumb_cache.get_value() else {
        return;
    };
    let Some(prod_h) = pl.thumb_producer.get_value() else {
        return;
    };
    if cache_h.borrow().has_any(GlobalFrame::new(global_frame)) {
        return;
    }
    set_fill_inflight(pl, true);
    let epoch = pl.open_gen.ticket();
    let global = FrameId::from(global_frame);
    let local = pos.frame.source();
    let clip = pos.clip_index;
    spawn_local(async move {
        let rank = u16::MAX; // hover Exact refine band
        let filled = fill_one(FillOne {
            pl,
            cache_h,
            prod_h,
            clip_index: clip,
            local,
            global,
            max_w,
            max_h,
            rank,
        })
        .await;
        if pl.open_gen.is_current(epoch) && filled {
            if let Some(h) = pl.exact_ledger() {
                h.borrow_mut().note_thumb(GlobalFrame::new(global));
            }
            pl.bump_thumb_residency();
        }
        if pl.open_gen.is_current(epoch) {
            set_fill_inflight(pl, false);
        }
    });
}
