//! [`VideoPlaylist`] — two [`crate::Video`] slots (live + idle) over a playlist model.

use fpmedia_web::ScrubCachePolicy;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::Video;
use crate::frame_axis::{ClipFrame, GlobalFrame};
use crate::video_playlist::PlaylistPos;

use super::host::PlaylistHost;
use super::loop_seam::{loop_owner, seam_busy, seam_trace, try_begin_seam};
use super::loop_seam_exec::{advance_after_ended, note_host_seam_err, start_host_wrap};
use super::plan::{HostOutsideCtx, HostOutsidePlan, plan_host_outside};
use super::slots::maybe_kick_idle_seam_warm;
use super::video_playlist_ref::VideoPlaylistRef;

/// Dual-slot playlist stage. Drive via [`VideoPlaylistRef`].
///
/// Mounts two `<Video>` elements; CSS shows the live slot. Probe fills
/// [`PlaylistModel`], then opens clip 0 on live and clip 1 on idle (when present).
#[component]
pub fn VideoPlaylist(
    playlist: VideoPlaylistRef,
    #[prop(optional, into)] class: Option<String>,
) -> impl IntoView {
    let VideoPlaylistRef {
        ready,
        busy,
        ended,
        looping,
        model,
        pos,
        playhead,
        frame_count,
        live_clip,
        idle_clip,
        live_is_a,
        slot_a,
        slot_b,
        open_gen,
        ..
    } = playlist;

    // Live densify On (engine next_gap); idle park (keep Exact). Exact = slot blit.
    // VT = axis/settle/remap only. **no equivalent** (VT local).
    Effect::new(move |_| {
        let live = playlist.live_video_tracked();
        let idle = playlist.idle_video_tracked();
        idle.park_scrub_cache();
        // Re-assert after each slot open (open_source resets thumb authority false).
        idle.set_vt_thumb_authority(ready.get());
        live.set_vt_thumb_authority(ready.get());
        if ready.get() {
            live.set_scrub_cache_policy(ScrubCachePolicy::On);
        }
    });

    super::thumbs::install_vt_thumb_effects(playlist);

    // Mirror live presentation → playlist pos + global frame.
    // Playing + paused step/seek. Not while scrubbing (gesture owns aim).
    // Not while seam_busy — wrap owns global aim (else mirror fights zone_in
    // snap → wall re-wrap / dual tip). After Up, slot `frame` lags settle —
    // use playhead when it diverges so the tip does not yank to a stale local
    // then snap back. Cross-clip Up/seek: facade `pos` aims the new clip while
    // live is still old — skip until promote (else tip jumps to old global).
    // **no equivalent**.
    Effect::new(move |_| {
        if !ready.get() || playlist.scrubbing.get() {
            return;
        }
        let clip = live_clip.get();
        // Pending rebind owns aim — do not map old live over CrossClipSeek.
        if pos.get_untracked().clip_index != clip {
            return;
        }
        let live = playlist.live_video_tracked();
        let committed = live.frame.get();
        let aim = live.playhead.get();
        let local = if aim != committed { aim } else { committed };
        let p = PlaylistPos::new(clip, ClipFrame::from_source(local));
        if seam_busy(playlist) {
            if let Some(would) = model
                .get_untracked()
                .global_frame_at(p)
                .map(GlobalFrame::get)
            {
                let cur = playhead.get_untracked();
                let zone = playlist.loop_zone();
                let zone_last = zone.out_frame.saturating_sub(1);
                if would != cur {
                    seam_trace(&format!(
                        "mirror blocked busy cur={cur} would={would} zone_in={} local={local} aim={aim} frame={committed}",
                        zone.in_frame
                    ));
                }
                if would >= zone_last {
                    seam_trace(&format!(
                        "mirror blocked past-wall would={would} zone_last={zone_last} local={local}"
                    ));
                }
            }
            return;
        }
        pos.set(p);
        if let Some(g) = model
            .get_untracked()
            .global_frame_at(p)
            .map(GlobalFrame::get)
        {
            let cur = playhead.get_untracked();
            let zone_last = playlist.loop_zone().out_frame.saturating_sub(1);
            let jump = cur.abs_diff(g);
            if g != cur && (g >= zone_last || jump > 1) {
                seam_trace(&format!(
                    "mirror write cur={cur} → {g} jump={jump} zone_last={zone_last} clip={clip} local={local} aim={aim} frame={committed}"
                ));
            }
            playhead.set(g);
        }
    });

    // Host seam dispatch: mid-wall wrap (LoopOwner::Host) or clip-EOF advance.
    // Intra-clip zone → engine `LoopSeam` (owner Engine). One Effect — SeamPhase
    // makes dual wrap unrepresentable. **no equivalent**.
    Effect::new(move |_| {
        // Track phase so Idle-after-busy re-runs while slot `ended` is still set.
        let _ = playlist.seam_phase.get();
        if !ready.get() || playlist.scrubbing.get() || seam_busy(playlist) {
            return;
        }
        let live = playlist.live_video_tracked();
        let _ = live_clip.get();
        let _ = playlist.loop_in.get();
        let _ = playlist.loop_out.get();
        let owner = loop_owner(playlist);
        let g = playhead.get();
        let zone = playlist.loop_zone();
        let zone_in = zone.in_frame;
        let zone_out = zone.out_frame;
        let total = frame_count.get_untracked();

        // Host LoopZone outside while Playing → wrap (`plan_host_outside`).
        // Past-out only when `zone_last` is on **live** — else clip-EOF advance.
        // Goto / paused scrub do not hit this (`playing` false). **no equivalent**.
        let zone_last = zone_out.saturating_sub(1);
        let wall_on_live = playlist
            .model
            .get_untracked()
            .at_global_frame(GlobalFrame::new(zone_last))
            .is_some_and(|p| p.clip_index == live_clip.get_untracked());
        let wall = plan_host_outside(HostOutsideCtx {
            owner,
            looping: looping.get(),
            zone_in,
            zone_out,
            total_frames: total,
            global: g,
            seam_busy: false, // Effect gated on `seam_busy` above
            past_out_needs_wall_on_live: true,
            wall_on_live,
        });
        if matches!(wall, HostOutsidePlan::HostWrap { .. })
            && live.playing.get()
            && !live.ended.get()
        {
            seam_trace(&format!(
                "outside-play fire g={g} zone_in={zone_in} zone_last={zone_last} live_clip={} phase={:?} live_frame={} live_ph={} live_playing={} live_ended={}",
                live_clip.get_untracked(),
                playlist.seam_phase.get_untracked(),
                live.frame.get_untracked(),
                live.playhead.get_untracked(),
                live.playing.get_untracked(),
                live.ended.get_untracked(),
            ));
            start_host_wrap(playlist, false);
            return;
        }

        if !live.ended.get() {
            return;
        }
        // Latch seam before clearing `ended` — else a busy SeamPhase drops the
        // EOF forever (stop at clip join; next Play snaps Host slot → 0).
        let from_clip = live_clip.get_untracked();
        if !try_begin_seam(playlist) {
            return;
        }
        live.ended.set(false);
        let epoch = open_gen.ticket();
        let pl = playlist;
        spawn_local(async move {
            if let Err(e) = advance_after_ended(pl, epoch, from_clip).await {
                note_host_seam_err(pl, epoch, e, "Playlist seam failed.");
            }
        });
    });

    // Near-EOF: byte + PreferSoftware origin stash on idle (PlayPrime stash hit).
    Effect::new(move |_| {
        if !ready.get() {
            return;
        }
        let live = playlist.live_video_tracked();
        if !live.playing.get() {
            return;
        }
        let _ = live.timestamp.get();
        let _ = live_clip.get();
        let _ = idle_clip.get();
        let _ = looping.get();
        let _ = playlist.loop_in.get();
        let _ = playlist.loop_out.get();
        maybe_kick_idle_seam_warm(playlist);
    });

    playlist.mounted.set_value(true);
    on_cleanup(move || {
        if playlist.mounted.try_with_value(|m| *m).unwrap_or(false) {
            PlaylistHost(playlist).close();
        }
        playlist.mounted.set_value(false);
    });
    let root_class = class.unwrap_or_else(|| "fpvideo-playlist".into());

    view! {
        <div
            class=root_class
            data-testid="playlist"
            data-ready=move || ready.get().to_string()
            data-busy=move || busy.get().to_string()
            data-live-clip=move || live_clip.get().to_string()
            data-ended=move || ended.get().to_string()
            data-looping=move || looping.get().to_string()
        >
            <div
                class="fpvideo-playlist-slot"
                class:is-live=move || live_is_a.get()
                class:is-idle=move || !live_is_a.get()
                aria-hidden=move || (!live_is_a.get()).to_string()
            >
                <Video video=slot_a />
            </div>
            <div
                class="fpvideo-playlist-slot"
                class:is-live=move || !live_is_a.get()
                class:is-idle=move || live_is_a.get()
                aria-hidden=move || live_is_a.get().to_string()
            >
                <Video video=slot_b />
            </div>
        </div>
    }
}
