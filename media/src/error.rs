//! Media engine errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("mp4 parse error: {0}")]
    Mp4(String),

    #[error("no video track in container")]
    NoVideoTrack,

    #[error("no audio or video track in container")]
    NoAvTrack,

    #[error("unsupported or missing codec config")]
    UnsupportedCodec,

    #[error("frame {0} out of range (count={1})")]
    FrameOutOfRange(u64, u64),

    #[error("audio sample {0} out of range (count={1})")]
    AudioSampleOutOfRange(u64, u64),

    #[error("empty video track")]
    EmptyTrack,

    #[error("frame {0} has no duration")]
    MissingFrameDuration(u64),

    #[error("webcodecs: {0}")]
    WebCodecs(String),

    /// Browser `VideoFrame.clone` failed. Prefer moving the frame (cache insert)
    /// or borrowing via [`crate::decode::AsVideoFrame`] for draw.
    #[error("video frame clone failed: {0}")]
    VideoFrameClone(String),

    #[error("codec not supported by this browser: {0}")]
    CodecUnsupported(String),

    /// WebCodecs needs a secure context (HTTPS or localhost). LAN `http://IP`
    /// (typical Android phone → desktop `cargo leptos watch`) has no `VideoDecoder`.
    #[error(
        "WebCodecs unavailable: open via https:// or http://localhost (not http://LAN-IP). On Android: adb reverse tcp:3000 tcp:3000 then http://127.0.0.1:3000"
    )]
    WebCodecsInsecureContext,

    /// In-flight decode/seek superseded by pause, a newer seek, or teardown
    /// (mediabunny iterator `return` / `Input.dispose`).
    #[error("operation cancelled")]
    Cancelled,

    #[error("unsupported: {0}")]
    Unsupported(&'static str),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("media source: {0}")]
    Source(String),

    #[error("read past end: offset={offset} len={len} size={size}")]
    ReadOutOfRange { offset: u64, len: usize, size: u64 },
}

impl MediaError {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl From<re_mp4::Error> for MediaError {
    fn from(value: re_mp4::Error) -> Self {
        Self::Mp4(value.to_string())
    }
}

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
impl From<wasm_bindgen::JsValue> for MediaError {
    fn from(value: wasm_bindgen::JsValue) -> Self {
        let msg = value
            .as_string()
            .or_else(|| {
                js_sys::Reflect::get(&value, &"message".into())
                    .ok()
                    .and_then(|v| v.as_string())
            })
            .unwrap_or_else(|| format!("{value:?}"));
        Self::WebCodecs(msg)
    }
}
