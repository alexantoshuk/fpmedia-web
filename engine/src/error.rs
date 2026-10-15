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

    /// Our decode pump gave up on a target (one hard reset + retry).
    #[error("webcodecs: {detail}")]
    DecodeStall { kind: DecodeStall, detail: String },

    /// Browser codec fault after densify/settle abort races (closed codec,
    /// `Decoding error`, key frame required) — Exact/PlayPrime recreate once.
    #[error("webcodecs: {0}")]
    DecoderPoison(String),

    /// Browser `VideoFrame.clone` failed. Prefer moving the frame (cache insert)
    /// or borrowing via [`crate::decode::AsVideoFrame`] for draw.
    #[error("video frame clone failed: {0}")]
    VideoFrameClone(String),

    #[error("codec not supported by this browser: {0}")]
    CodecUnsupported(String),

    /// WebCodecs needs a secure context (HTTPS or localhost). LAN `http://IP`
    /// (typical Android phone → desktop `trunk serve`) has no `VideoDecoder`.
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

/// Why the decode pump stopped waiting for a target frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeStall {
    /// Packets ran out and flush/idle never produced the target PTS.
    TargetMissing,
    /// No pump progress for the consumer stall window.
    PumpStalled,
    /// `decodeQueueSize` never drained (wedged decoder, `q≈41,held=0`).
    Backpressure,
}

impl MediaError {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// Browser codec error text → [`Self::DecoderPoison`] or [`Self::WebCodecs`].
    ///
    /// Only browser-originated messages pass through here; our own decode errors
    /// are typed at the producer.
    pub fn from_codec_message(msg: String) -> Self {
        let m = msg.to_ascii_lowercase();
        let poison = m.contains("decoding error")
            || m.contains("closed")
            || m.contains("cannot call")
            || m.contains("key frame required");
        if poison {
            Self::DecoderPoison(msg)
        } else {
            Self::WebCodecs(msg)
        }
    }

    /// Recreate-once predicate (named recovery, not a `SettlePolicy` knob):
    /// browser poison, or a wedged-backpressure stall that survived the
    /// reset + retry in `decode_frame_inner`.
    pub fn is_decoder_poison(&self) -> bool {
        matches!(
            self,
            Self::DecoderPoison(_)
                | Self::DecodeStall {
                    kind: DecodeStall::Backpressure,
                    ..
                }
        )
    }

    /// Pump stall worth one hard decoder reset + retry (orphan PTS after a raced
    /// invalidate).
    pub fn is_retryable_stall(&self) -> bool {
        matches!(self, Self::DecodeStall { .. })
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
        Self::from_codec_message(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_poison_messages_classify() {
        for msg in [
            "Decoding error.",
            "Cannot call 'decode' on a closed codec.",
            "A key frame required after configure() or flush().",
        ] {
            let e = MediaError::from_codec_message(msg.into());
            assert!(e.is_decoder_poison(), "{msg}");
            assert!(!e.is_retryable_stall(), "{msg}");
            assert_eq!(e.to_string(), format!("webcodecs: {msg}"));
        }
        let e = MediaError::from_codec_message("Unsupported configuration".into());
        assert!(matches!(e, MediaError::WebCodecs(_)));
        assert!(!e.is_decoder_poison());
    }

    #[test]
    fn stalls_retry_and_only_backpressure_poisons() {
        let stall = |kind| MediaError::DecodeStall {
            kind,
            detail: "x".into(),
        };
        for kind in [
            DecodeStall::TargetMissing,
            DecodeStall::PumpStalled,
            DecodeStall::Backpressure,
        ] {
            assert!(stall(kind).is_retryable_stall());
            assert_eq!(
                stall(kind).is_decoder_poison(),
                kind == DecodeStall::Backpressure
            );
        }
        assert!(!MediaError::Cancelled.is_retryable_stall());
    }
}
