use fpmedia_web_components::helpers::{format_metadata, format_timecode_at};
use fpmedia_web_components::{PipelineStats, ReadyFrameKind, VideoRef};
use leptos::prelude::*;

pub(super) fn demo_meta_line(video: &VideoRef, clip_name: Option<String>) -> String {
    let mut s = String::new();
    if let Some(name) = clip_name {
        s.push_str(&name);
    }
    if let Some(meta) = video.metadata.get() {
        let m = format_metadata(&meta);
        if !m.is_empty() {
            if !s.is_empty() {
                s.push_str(" · ");
            }
            s.push_str(&m);
        }
    }
    if video.has_audio.get() {
        if !s.is_empty() {
            s.push_str(" · ");
        }
        s.push_str("audio");
    }
    s
}

pub(super) fn demo_status_line(video: &VideoRef) -> String {
    if video.busy.get() || !video.ready.get() {
        return video.status.get();
    }
    let fps_part = match video.fps.get() {
        Some(v) => format!(" · {v:.1} fps"),
        None => String::new(),
    };
    let ph = video.playhead.get();
    let eng = video.frame.get();
    let frame_part = if ph == eng {
        format!("frame {ph}")
    } else {
        format!("frame {ph} (eng {eng})")
    };
    let tc_part = video
        .metadata
        .get()
        .and_then(|m| format_timecode_at(&m, ph))
        .map(|tc| format!(" · {tc}"))
        .unwrap_or_default();
    format!(
        "{frame_part} / {}{tc_part} · {} · {:.3}s{}{}{}",
        video.frame_count.get().saturating_sub(1),
        video
            .metadata
            .get()
            .map(|m| m.gop_profile.label())
            .unwrap_or_else(|| "—".into()),
        video.timestamp.get(),
        fps_part,
        if video.playing.get() {
            " · playing"
        } else {
            ""
        },
        if video.looping.get() { " · loop" } else { "" }
    )
}

pub(super) fn demo_pipeline_line(stats: Option<PipelineStats>) -> String {
    let Some(s) = stats else {
        return String::new();
    };
    let range = match (s.presentation_from, s.presentation_to) {
        (Some(a), Some(b)) if a == b => format!(" [{a}]"),
        (Some(a), Some(b)) => format!(" [{a}–{b}]"),
        _ => String::new(),
    };
    let lag = match s.last_behind_ms {
        Some(ms) => format!(" · lag {ms:.0}ms"),
        None => String::new(),
    };
    let holds = if s.underrun_holds > 0 || s.underrun_hold_ms > 0.0 {
        format!(" · holds {}/{:.0}ms", s.underrun_holds, s.underrun_hold_ms)
    } else {
        String::new()
    };
    let dense = if s.scrub_busy { " · dense" } else { "" };
    let waits = match s.play_waits {
        Some(w) => format!(
            "\nwait/s rd {:.0} (max {:.0}) · out {:.0} · cap {:.0} · raf>50 {} (max {:.0})",
            w.read_ms, w.read_max_ms, w.out_wait_ms, w.cap_wait_ms, w.raf_stalls, w.raf_max_ms
        ),
        None => String::new(),
    };
    format!(
        "q {}/{} · held {} · decodeQ {} · skipped {}{}{}{}{}{}",
        s.presentation_len,
        s.presentation_cap,
        if s.held_len == usize::MAX {
            "busy".into()
        } else {
            s.held_len.to_string()
        },
        if s.decode_queue_size == u32::MAX {
            "busy".into()
        } else {
            s.decode_queue_size.to_string()
        },
        s.frames_skipped,
        range,
        lag,
        holds,
        dense,
        waits,
    )
}

/// Snapshot for mobile paste into chat (meta / status / pipeline / error).
pub(super) fn demo_stats_clipboard(video: &VideoRef, clip_name: Option<String>) -> String {
    let mut lines = Vec::new();
    let meta = demo_meta_line(video, clip_name);
    if !meta.is_empty() {
        lines.push(meta);
    }
    lines.push(demo_status_line(video));
    let pipe = demo_pipeline_line(video.pipeline.get());
    if !pipe.is_empty() {
        lines.push(pipe);
    }
    if let Some(err) = video.error.get()
        && !err.is_empty()
    {
        lines.push(err);
    }
    lines.join("\n")
}

/// Compact JSON for e2e (`data-testid="pipeline-json"`). Manual — app has no serde.
pub(super) fn pipeline_stats_json(stats: Option<PipelineStats>) -> String {
    let Some(s) = stats else {
        return "{}".into();
    };
    let ranges = s
        .fetch_file_ranges
        .iter()
        .map(|(a, b)| format!("[{a:.6},{b:.6}]"))
        .collect::<Vec<_>>()
        .join(",");
    let playable = s
        .fetch_playable_runs
        .iter()
        .map(|r| format!("[{},{}]", r.lo, r.hi))
        .collect::<Vec<_>>()
        .join(",");
    let ready = s
        .ready_frames
        .iter()
        .map(|m| {
            let kind = match m.kind {
                ReadyFrameKind::Keyframes => "Keyframes",
                ReadyFrameKind::Covered => "Covered",
                ReadyFrameKind::Exact => "Exact",
            };
            format!(
                "{{\"frame\":{},\"end\":{},\"kind\":\"{kind}\"}}",
                m.frame, m.end
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let behind = match s.last_behind_ms {
        Some(ms) => format!("{ms:.3}"),
        None => "null".into(),
    };
    let held = if s.held_len == usize::MAX {
        "null".into()
    } else {
        s.held_len.to_string()
    };
    let dq = if s.decode_queue_size == u32::MAX {
        "null".into()
    } else {
        s.decode_queue_size.to_string()
    };
    let win_from = match s.scrub_window_from {
        Some(v) => v.to_string(),
        None => "null".into(),
    };
    let win_to = match s.scrub_window_to {
        Some(v) => v.to_string(),
        None => "null".into(),
    };
    format!(
        "{{\"fetch_file_ranges\":[{ranges}],\"fetch_playable_runs\":[{playable}],\"ready_frames\":[{ready}],\"last_behind_ms\":{behind},\"underrun_holds\":{},\"underrun_hold_ms\":{:.3},\"frames_skipped\":{},\"held_len\":{held},\"decode_queue_size\":{dq},\"scrub_busy\":{},\"cache_frames\":{},\"cache_chunks\":{},\"cache_cap\":{},\"cache_no_gaps\":{},\"scrub_window_from\":{win_from},\"scrub_window_to\":{win_to},\"presentation_len\":{},\"presentation_cap\":{}}}",
        s.underrun_holds,
        s.underrun_hold_ms,
        s.frames_skipped,
        s.scrub_busy,
        s.cache_frames,
        s.cache_chunks,
        s.cache_cap,
        s.cache_no_gaps,
        s.presentation_len,
        s.presentation_cap,
    )
}
