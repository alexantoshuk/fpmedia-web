//! [`VideoPlaylist`] — two [`crate::Video`] slots (live + idle) over a playlist model.

use std::rc::Rc;

use fpmedia_web::timer::sleep_ms;
use fpmedia_web::{ScrubCachePolicy, ThumbPolicy, VideoMetadata};
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::Video;
use crate::VideoRef;
use crate::video_playlist::{PlaylistClip, PlaylistModel, PlaylistPos};

use super::source::PlaylistSource;
use super::video_playlist_ref::{VideoPlaylistOps, VideoPlaylistRef};

const PROBE_TIMEOUT_MS: f64 = 120_000.0;
const PROBE_POLL_MS: i32 = 32;

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
        status,
        error,
        ended,
        looping,
        model,
        pos,
        playhead_ms,
        duration_ms,
        live_clip,
        idle_clip,
        live_is_a,
        slot_a,
        slot_b,
        ops,
        sources,
        open_gen,
        ..
    } = playlist;

    // Idle: no densify / thumbs while standby.
    Effect::new(move |_| {
        let idle = if live_is_a.get() { slot_b } else { slot_a };
        idle.set_thumb_policy(ThumbPolicy::Off);
        idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    });

    // Mirror live presentation → playlist pos + ms playhead.
    Effect::new(move |_| {
        if !ready.get() {
            return;
        }
        let clip = live_clip.get();
        let live = if live_is_a.get() { slot_a } else { slot_b };
        let local = live.frame.get();
        let p = PlaylistPos::new(clip, local);
        pos.set(p);
        if let Some(ms) = model.get_untracked().ms_at(p) {
            playhead_ms.set(ms);
        }
    });

    // Clip EOF → advance playlist (swap idle / loop / ended).
    Effect::new(move |_| {
        if !ready.get() {
            return;
        }
        let live = if live_is_a.get() { slot_a } else { slot_b };
        if !live.ended.get() {
            return;
        }
        live.ended.set(false);
        let epoch = open_gen.get_value();
        let pl = playlist;
        spawn_local(async move {
            if let Err(e) = advance_after_ended(pl, epoch).await {
                if pl.open_gen.get_value() != epoch {
                    return;
                }
                pl.error.set(Some(e.clone()));
                pl.status.set("Playlist seam failed.".into());
            }
        });
    });

    let bump_gen = Rc::new({
        move || {
            let g = open_gen.get_value().wrapping_add(1);
            open_gen.set_value(g);
            g
        }
    }) as Rc<dyn Fn() -> u64>;

    let close_all = {
        let bump_gen = Rc::clone(&bump_gen);
        Rc::new(move || {
            let _ = bump_gen();
            slot_a.close();
            slot_b.close();
            ready.set(false);
            busy.set(false);
            ended.set(false);
            model.set(PlaylistModel::default());
            pos.set(PlaylistPos::new(0, 0));
            playhead_ms.set(0);
            duration_ms.set(0);
            live_clip.set(0);
            idle_clip.set(None);
            live_is_a.set(true);
            sources.set_value(Vec::new());
            error.set(None);
            status.set("Closed.".into());
        }) as Rc<dyn Fn()>
    };

    let start_open = {
        let close_all = Rc::clone(&close_all);
        let bump_gen = Rc::clone(&bump_gen);
        Rc::new(move |list: Vec<PlaylistSource>| {
            if list.is_empty() {
                error.set(Some("empty playlist".into()));
                status.set("Add at least one file or URL.".into());
                return;
            }
            close_all();
            let epoch = bump_gen();
            sources.set_value(list.clone());
            busy.set(true);
            ready.set(false);
            error.set(None);
            status.set(format!("Probing playlist (0/{})…", list.len()));

            spawn_local(async move {
                let mut clips = Vec::with_capacity(list.len());
                // Probe every clip on slot_a (sequential open → metadata → close).
                for (i, src) in list.iter().enumerate() {
                    if open_gen.get_value() != epoch {
                        return;
                    }
                    status.set(format!("Probing playlist ({}/{})…", i + 1, list.len()));
                    slot_a.close();
                    // Let retire settle one tick.
                    sleep_ms(0).await;
                    if open_gen.get_value() != epoch {
                        return;
                    }
                    src.open_on(slot_a);
                    match wait_ready(slot_a, epoch, open_gen).await {
                        Ok(meta) => clips.push(PlaylistClip::from_meta(&meta)),
                        Err(e) => {
                            if open_gen.get_value() != epoch {
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

                if open_gen.get_value() != epoch {
                    return;
                }

                let built = PlaylistModel::new(clips);
                let total_ms = built.total_duration_ms();
                model.set(built);
                duration_ms.set(total_ms);
                playhead_ms.set(0);
                pos.set(PlaylistPos::new(0, 0));
                live_is_a.set(true);
                live_clip.set(0);
                idle_clip.set(if list.len() > 1 { Some(1) } else { None });

                slot_a.close();
                slot_b.close();
                sleep_ms(0).await;
                if open_gen.get_value() != epoch {
                    return;
                }

                status.set("Opening live clip…".into());
                list[0].open_on(slot_a);
                slot_a.set_loop(false);
                slot_a.set_thumb_policy(ThumbPolicy::On);
                slot_a.set_scrub_cache_policy(ScrubCachePolicy::On);

                if list.len() > 1 {
                    status.set("Opening idle clip…".into());
                    list[1].open_on(slot_b);
                    slot_b.set_loop(false);
                    slot_b.set_thumb_policy(ThumbPolicy::Off);
                    slot_b.set_scrub_cache_policy(ScrubCachePolicy::Off);
                }

                match wait_ready(slot_a, epoch, open_gen).await {
                    Ok(_) => {
                        if open_gen.get_value() != epoch {
                            return;
                        }
                        // Idle may still be opening — do not block ready on it.
                        busy.set(false);
                        ready.set(true);
                        ended.set(false);
                        status.set(format!("Playlist ready · {} clip(s).", list.len()));
                        error.set(None);
                    }
                    Err(e) => {
                        if open_gen.get_value() != epoch {
                            return;
                        }
                        busy.set(false);
                        ready.set(false);
                        error.set(Some(e));
                        status.set("Live open failed.".into());
                    }
                }
            });
        }) as Rc<dyn Fn(Vec<PlaylistSource>)>
    };

    let open_blobs = {
        let start_open = Rc::clone(&start_open);
        Rc::new(move |blobs: Vec<web_sys::Blob>| {
            let list = blobs.into_iter().map(PlaylistSource::Blob).collect();
            start_open(list);
        }) as Rc<dyn Fn(Vec<web_sys::Blob>)>
    };

    let open_urls = {
        let start_open = Rc::clone(&start_open);
        Rc::new(move |urls: Vec<String>| {
            let list = urls
                .into_iter()
                .map(|u| u.trim().to_string())
                .filter(|u| !u.is_empty())
                .map(PlaylistSource::Url)
                .collect::<Vec<_>>();
            start_open(list);
        }) as Rc<dyn Fn(Vec<String>)>
    };

    let play_fn = Rc::new(move || {
        ended.set(false);
        let live = if live_is_a.get_untracked() {
            slot_a
        } else {
            slot_b
        };
        live.set_loop(false);
        live.play();
    }) as Rc<dyn Fn()>;

    let pause_fn = Rc::new(move || {
        let live = if live_is_a.get_untracked() {
            slot_a
        } else {
            slot_b
        };
        live.pause();
    }) as Rc<dyn Fn()>;

    ops.set_value(Some(VideoPlaylistOps {
        open_blobs,
        open_urls,
        play: play_fn,
        pause: pause_fn,
        close: Rc::clone(&close_all),
    }));

    on_cleanup(move || {
        let close = ops
            .try_with_value(|o| o.as_ref().map(|v| Rc::clone(&v.close)))
            .flatten();
        if let Some(close) = close {
            close();
        }
        ops.set_value(None);
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

async fn wait_ready(
    video: VideoRef,
    epoch: u64,
    open_gen: StoredValue<u64, LocalStorage>,
) -> Result<VideoMetadata, String> {
    let start = fpmedia_web::timer::now_ms().unwrap_or(0.0);
    loop {
        if open_gen.get_value() != epoch {
            return Err("cancelled".into());
        }
        if let Some(err) = video.error.get_untracked() {
            return Err(err);
        }
        if video.ready.get_untracked()
            && let Some(meta) = video.metadata.get_untracked()
        {
            return Ok(meta);
        }
        let now = fpmedia_web::timer::now_ms().unwrap_or(start);
        if now - start > PROBE_TIMEOUT_MS {
            return Err("open timed out".into());
        }
        sleep_ms(PROBE_POLL_MS).await;
    }
}

async fn ensure_clip_on_idle(
    pl: VideoPlaylistRef,
    clip_index: u32,
    epoch: u64,
) -> Result<(), String> {
    if pl.open_gen.get_value() != epoch {
        return Err("cancelled".into());
    }
    if pl.idle_clip.get_untracked() == Some(clip_index) && pl.idle_video().ready.get_untracked() {
        return Ok(());
    }
    let src = pl
        .sources
        .try_with_value(|s| s.get(clip_index as usize).cloned())
        .flatten()
        .ok_or_else(|| format!("missing source for clip {}", clip_index + 1))?;
    let idle = pl.idle_video();
    idle.set_loop(false);
    idle.set_thumb_policy(ThumbPolicy::Off);
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    idle.close();
    sleep_ms(0).await;
    if pl.open_gen.get_value() != epoch {
        return Err("cancelled".into());
    }
    src.open_on(idle);
    pl.idle_clip.set(Some(clip_index));
    wait_ready(idle, epoch, pl.open_gen).await?;
    Ok(())
}

fn swap_live_idle(pl: VideoPlaylistRef, new_live_clip: u32, new_idle_clip: Option<u32>) {
    pl.live_is_a.set(!pl.live_is_a.get_untracked());
    pl.live_clip.set(new_live_clip);
    pl.idle_clip.set(new_idle_clip);
    let live = pl.live_video();
    live.set_loop(false);
    live.set_thumb_policy(ThumbPolicy::On);
    live.set_scrub_cache_policy(ScrubCachePolicy::On);
    live.ended.set(false);
    let idle = pl.idle_video();
    idle.set_thumb_policy(ThumbPolicy::Off);
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    idle.set_loop(false);
}

async fn preload_idle(pl: VideoPlaylistRef, clip_index: Option<u32>, epoch: u64) {
    let Some(i) = clip_index else {
        return;
    };
    if pl.open_gen.get_value() != epoch {
        return;
    }
    let Some(src) = pl
        .sources
        .try_with_value(|s| s.get(i as usize).cloned())
        .flatten()
    else {
        return;
    };
    let idle = pl.idle_video();
    idle.set_loop(false);
    idle.set_thumb_policy(ThumbPolicy::Off);
    idle.set_scrub_cache_policy(ScrubCachePolicy::Off);
    src.open_on(idle);
    pl.idle_clip.set(Some(i));
    let _ = wait_ready(idle, epoch, pl.open_gen).await;
}

/// After live clip EOF: play next, wrap on playlist loop, or mark playlist ended.
async fn advance_after_ended(pl: VideoPlaylistRef, epoch: u64) -> Result<(), String> {
    if pl.open_gen.get_value() != epoch {
        return Ok(());
    }
    let model = pl.model.get_untracked();
    let cur = pl.live_clip.get_untracked();

    if let Some(next_i) = model.next_clip_index(cur) {
        pl.status.set(format!("Seam → clip {}…", next_i + 1));
        ensure_clip_on_idle(pl, next_i, epoch).await?;
        if pl.open_gen.get_value() != epoch {
            return Ok(());
        }
        let following = model.next_clip_index(next_i);
        swap_live_idle(pl, next_i, following);
        pl.live_video().seek_frame(0);
        pl.live_video().play();
        pl.ended.set(false);
        pl.status.set(format!("Playing clip {}/{}", next_i + 1, model.len()));
        preload_idle(pl, following, epoch).await;
        return Ok(());
    }

    if pl.looping.get_untracked() {
        pl.status.set("Loop → clip 1…".into());
        if model.len() <= 1 {
            let live = pl.live_video();
            live.ended.set(false);
            live.seek_frame(0);
            live.play();
            pl.ended.set(false);
            return Ok(());
        }
        ensure_clip_on_idle(pl, 0, epoch).await?;
        if pl.open_gen.get_value() != epoch {
            return Ok(());
        }
        let following = model.next_clip_index(0);
        swap_live_idle(pl, 0, following);
        pl.live_video().seek_frame(0);
        pl.live_video().play();
        pl.ended.set(false);
        pl.status.set(format!("Playing clip 1/{}", model.len()));
        preload_idle(pl, following, epoch).await;
        return Ok(());
    }

    pl.ended.set(true);
    pl.status.set("Ended.".into());
    Ok(())
}
