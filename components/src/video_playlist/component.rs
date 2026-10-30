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
                slot_a.set_thumb_policy(ThumbPolicy::On);
                slot_a.set_scrub_cache_policy(ScrubCachePolicy::On);

                if list.len() > 1 {
                    status.set("Opening idle clip…".into());
                    list[1].open_on(slot_b);
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
        let live = if live_is_a.get_untracked() {
            slot_a
        } else {
            slot_b
        };
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
