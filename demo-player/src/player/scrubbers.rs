use super::position::{HoverPlaylistThumbLabel, HoverThumbFrameLabel, PositionFormat};
use super::zebra::{FRAME_ZEBRA_CONTRAST, FrameZebra};
use fpmedia_web_components::helpers::{playhead_tip_css, playhead_width_css};
use fpmedia_web_components::{
    HoverThumb, Scrubber, ScrubberCtx, TimelineMeta, TimelineMetaKind, VideoPlaylistRef, VideoRef,
};
use leptos::prelude::*;

/// Opaque tip — continuous `left` + `max(2px, 100%/n)` width (track clips EOF).
#[component]
pub(super) fn ScrubPlayheadTip() -> impl IntoView {
    let ctx =
        use_context::<ScrubberCtx>().expect("ScrubPlayheadTip must be used inside <Scrubber>");
    let timeline = ctx.timeline;
    let tip =
        Memo::new(move |_| playhead_tip_css(timeline.playhead.get(), timeline.frame_count.get()));
    view! {
        <div
            class="wm-scrubber-playhead"
            data-testid="scrubber-playhead"
            aria-hidden="true"
            style:left=move || tip.get().0
            style:width=move || tip.get().1
        />
    }
}

/// Progress fill to tip left — continuous `calc(100% * f / n)`.
#[component]
pub(super) fn ScrubProgress() -> impl IntoView {
    let ctx = use_context::<ScrubberCtx>().expect("ScrubProgress must be used inside <Scrubber>");
    let timeline = ctx.timeline;
    let width =
        Memo::new(move |_| playhead_width_css(timeline.playhead.get(), timeline.frame_count.get()));
    view! {
        <div
            class="wm-scrubber-progress"
            data-testid="scrubber-progress"
            style:width=move || width.get()
        />
    }
}

/// Demo scrubber chrome over headless [`Scrubber`] / [`TimelineMeta`] / [`HoverThumb`].
#[component]
pub(super) fn DemoScrubber(
    video: VideoRef,
    thumbs_on: RwSignal<bool>,
    no_hover: Signal<bool>,
    position_format: RwSignal<PositionFormat>,
) -> impl IntoView {
    view! {
        <Scrubber video=video class="wm-scrubber h-3.5".to_string()>
            <Show when=move || thumbs_on.get() && !no_hover.get()>
                <HoverThumb class="fpvideo-thumb-preview".to_string()>
                    <HoverThumbFrameLabel video=video format=position_format />
                </HoverThumb>
            </Show>
            <LoopZoneFade
                looping=Signal::derive(move || video.looping.get())
                loop_in=Signal::derive(move || video.loop_in.get())
                loop_out=Signal::derive(move || video.loop_out.get())
                frame_count=Signal::derive(move || video.frame_count.get())
            >
                <div class="wm-scrubber-track h-3">
                    <FrameZebra contrast=FRAME_ZEBRA_CONTRAST />
                    <TimelineMeta
                        kind=TimelineMetaKind::Fetch
                        class="wm-scrubber-fetched".to_string()
                        test_id="scrubber-fetched"
                    />
                    <ScrubProgress />
                    <ScrubPlayheadTip />
                </div>
                // Keyframes strip (yellow sync ticks) is debug-only — off by default
                // (distracting). Re-enable:
                //   <TimelineMeta kind=TimelineMetaKind::Keyframes class="wm-ready-strip is-keyframes".to_string() />
                // under Covered in the ready row.
                <div
                    class="wm-scrubber-ready h-0.5"
                    aria-hidden="true"
                    title="Decode densify: dim=markers (kept on Play), bright=blit ImageBitmap"
                >
                    <TimelineMeta
                        kind=TimelineMetaKind::Covered
                        class="wm-ready-strip is-covered".to_string()
                    />
                    <TimelineMeta
                        kind=TimelineMetaKind::Exact
                        class="wm-ready-strip is-exact".to_string()
                    />
                </div>
            </LoopZoneFade>
        </Scrubber>
    }
}

/// Fade track+progress+ready outside LoopZone via CSS mask (not a black overlay).
#[component]
pub(super) fn LoopZoneFade(
    looping: Signal<bool>,
    loop_in: Signal<u32>,
    loop_out: Signal<Option<u32>>,
    frame_count: Signal<u32>,
    children: Children,
) -> impl IntoView {
    let faded = Memo::new(move |_| {
        if !looping.get() {
            return false;
        }
        let n = frame_count.get();
        if n == 0 {
            return false;
        }
        let in_f = loop_in.get();
        let out_f = loop_out.get().unwrap_or(n);
        !(in_f == 0 && out_f >= n)
    });
    let style = Memo::new(move |_| {
        if !faded.get() {
            return String::new();
        }
        let n = frame_count.get().max(1) as f64;
        let in_pct = (loop_in.get() as f64 / n * 100.0).clamp(0.0, 100.0);
        let out_f = loop_out.get().unwrap_or(frame_count.get()) as f64;
        let out_pct = (out_f / n * 100.0).clamp(0.0, 100.0);
        format!("--loop-in-pct:{in_pct:.4}%;--loop-out-pct:{out_pct:.4}%;")
    });
    view! {
        <div
            class="wm-scrubber-body"
            class:is-loop-faded=move || faded.get()
            style=move || style.get()
        >
            {children()}
        </div>
    }
}

/// Playlist scrubber: `timeline=` only — strips/hover follow [`TimelineRef::strip_video`].
#[component]
pub(super) fn DemoPlaylistScrubber(
    playlist: VideoPlaylistRef,
    thumbs_on: RwSignal<bool>,
    no_hover: Signal<bool>,
    position_format: RwSignal<PositionFormat>,
) -> impl IntoView {
    let timeline = playlist.as_timeline();
    view! {
        <Scrubber timeline=timeline class="wm-scrubber h-3.5".to_string()>
            <Show when=move || thumbs_on.get() && !no_hover.get()>
                <HoverThumb class="fpvideo-thumb-preview".to_string()>
                    <HoverPlaylistThumbLabel
                        playlist=playlist
                        format=position_format
                    />
                </HoverThumb>
            </Show>
            <LoopZoneFade
                looping=Signal::derive(move || playlist.looping.get())
                loop_in=Signal::derive(move || playlist.loop_in.get())
                loop_out=Signal::derive(move || playlist.loop_out.get())
                frame_count=Signal::derive(move || playlist.frame_count.get())
            >
                <div class="wm-scrubber-track h-3">
                    <FrameZebra contrast=FRAME_ZEBRA_CONTRAST />
                    <TimelineMeta
                        kind=TimelineMetaKind::Fetch
                        class="wm-scrubber-fetched".to_string()
                        test_id="scrubber-fetched"
                    />
                    <ScrubProgress />
                    <ScrubPlayheadTip />
                </div>
                <div
                    class="wm-scrubber-ready h-0.5"
                    aria-hidden="true"
                    title="Decode densify: dim=markers (kept on Play), bright=blit ImageBitmap"
                >
                    <TimelineMeta
                        kind=TimelineMetaKind::Covered
                        class="wm-ready-strip is-covered".to_string()
                    />
                    <TimelineMeta
                        kind=TimelineMetaKind::Exact
                        class="wm-ready-strip is-exact".to_string()
                    />
                </div>
            </LoopZoneFade>
        </Scrubber>
    }
}
