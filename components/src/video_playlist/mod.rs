//! Playlist model + `[i]` helpers + dual-slot [`VideoPlaylist`] host (composition local).
//!
//! UX: `.ai/product.md` § Playlist. Design: `.ai/architecture.md` § Playlist composition.
//! Vocab: `.ai/glossary.md` § PlaylistPos.

mod model;
mod position;

#[cfg(target_arch = "wasm32")]
mod component;
#[cfg(target_arch = "wasm32")]
mod slots;
#[cfg(target_arch = "wasm32")]
mod source;
#[cfg(target_arch = "wasm32")]
pub(crate) mod video_playlist_ref;

pub use model::{PlaylistClip, PlaylistModel};
pub use position::{
    PlaylistPos, format_playlist_frames, format_playlist_goto_frames,
    format_playlist_goto_timecode, format_playlist_timecode, parse_playlist_goto,
    split_playlist_goto,
};

#[cfg(target_arch = "wasm32")]
pub use component::VideoPlaylist;
#[cfg(target_arch = "wasm32")]
pub use video_playlist_ref::VideoPlaylistRef;
