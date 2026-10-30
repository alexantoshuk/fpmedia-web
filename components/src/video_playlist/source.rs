//! Playlist open sources (Blob / URL).

#[derive(Clone)]
pub(crate) enum PlaylistSource {
    Blob(web_sys::Blob),
    Url(String),
}

impl PlaylistSource {
    pub(crate) fn open_on(&self, video: crate::VideoRef) {
        match self {
            Self::Blob(blob) => video.open_blob(blob.clone()),
            Self::Url(url) => video.open_url(url.clone()),
        }
    }
}
