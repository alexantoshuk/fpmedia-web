use fpmedia_web_components::{VideoPlaylistRef, VideoRef};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Blob, window};

/// Must match `demo-player/Trunk.toml` `[[proxy]]` backend (testvideo Release tag).
pub(super) const TESTVIDEO_DOWNLOAD_BASE: &str =
    "https://github.com/alexantoshuk/testvideo/releases/download/v0.3.0/";

/// Map known testvideo Release URLs / root-relative `/media/…` to absolute http(s) URLs.
/// `VideoRef::open_url` requires `http:`, `https:`, or `blob:` (not bare paths).
pub(super) fn resolve_demo_open_url(raw: &str) -> String {
    let trimmed = raw.trim();
    let path = if let Some(rest) = trimmed.strip_prefix(TESTVIDEO_DOWNLOAD_BASE) {
        format!("/media/{rest}")
    } else {
        trimmed.to_string()
    };
    if path.starts_with('/')
        && let Some(origin) = window().and_then(|w| w.location().origin().ok())
    {
        return format!("{origin}{path}");
    }
    path
}

/// Split demo URL field / prompt on `;` (trim, drop empties).
pub(super) fn split_demo_urls(raw: &str) -> Vec<String> {
    raw.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub(super) fn open_demo_single_url(
    video: VideoRef,
    playlist: VideoPlaylistRef,
    use_playlist: RwSignal<bool>,
    raw: String,
    clip_label: RwSignal<Option<String>>,
) {
    let trimmed = raw.trim().to_string();
    let resolved = resolve_demo_open_url(&trimmed);
    clip_label.set(if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    });
    playlist.close();
    use_playlist.set(false);
    video.open_url(resolved);
}

pub(super) fn open_demo_urls(
    video: VideoRef,
    playlist: VideoPlaylistRef,
    use_playlist: RwSignal<bool>,
    raw: String,
    clip_label: RwSignal<Option<String>>,
) {
    let parts = split_demo_urls(&raw);
    if parts.is_empty() {
        // Empty field: let the component report `empty URL` (it returns before
        // retiring the open clip) — keep clip label / playlist as they are.
        video.open_url(raw.trim().to_string());
        return;
    }
    if parts.len() == 1 {
        open_demo_single_url(video, playlist, use_playlist, parts[0].clone(), clip_label);
        return;
    }
    clip_label.set(Some(parts.join("; ")));
    // Carry single-video A/V into playlist-wide volume/mute/rate.
    playlist.set_volume(video.volume.get_untracked());
    playlist.set_muted(video.muted.get_untracked());
    playlist.set_playback_rate(video.playback_rate.get_untracked());
    video.close();
    use_playlist.set(true);
    let resolved = parts
        .into_iter()
        .map(|u| resolve_demo_open_url(&u))
        .collect::<Vec<_>>();
    playlist.open_urls(resolved);
}

pub(super) fn open_demo_files(
    video: VideoRef,
    playlist: VideoPlaylistRef,
    use_playlist: RwSignal<bool>,
    files: web_sys::FileList,
    clip_label: RwSignal<Option<String>>,
) {
    let n = files.length();
    if n == 0 {
        return;
    }
    if n == 1 {
        let Some(file) = files.get(0) else {
            return;
        };
        let name = file.name();
        clip_label.set(Some(name.clone()));
        video.status.set(format!("Selected {name}…"));
        playlist.close();
        use_playlist.set(false);
        let blob: Blob = file.unchecked_into();
        video.open_blob(blob);
        return;
    }
    let mut blobs = Vec::with_capacity(n as usize);
    let mut names = Vec::with_capacity(n as usize);
    for i in 0..n {
        let Some(file) = files.get(i) else {
            continue;
        };
        names.push(file.name());
        blobs.push(file.unchecked_into::<Blob>());
    }
    if blobs.is_empty() {
        return;
    }
    clip_label.set(Some(names.join(", ")));
    playlist.set_volume(video.volume.get_untracked());
    playlist.set_muted(video.muted.get_untracked());
    playlist.set_playback_rate(video.playback_rate.get_untracked());
    video.close();
    use_playlist.set(true);
    playlist
        .status
        .set(format!("Selected {} files…", blobs.len()));
    playlist.open_blobs(blobs);
}
