//! Media engine errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("mp4 parse error: {0}")]
    Mp4(String),

    #[error("no video track in container")]
    NoVideoTrack,

    #[error("unsupported or missing video codec config")]
    UnsupportedCodec,

    #[error("frame {0} out of range (count={1})")]
    FrameOutOfRange(u64, u64),

    #[error("empty video track")]
    EmptyTrack,

    #[error("frame {0} has no duration")]
    MissingFrameDuration(u64),

    #[error("webcodecs: {0}")]
    WebCodecs(String),

    #[error("codec not supported by this browser: {0}")]
    CodecUnsupported(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
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
