//! Uncompressed / G.711 PCM → f32 planar for [`super::AudioOutput`].
//!
//! No WebCodecs `AudioDecoder` — mediabunny ships a built-in converter.
//! `mediabunny: src/codec.ts` `parsePcmCodec`; `src/media-sink.ts`
//! `PcmAudioDecoderWrapper`; `src/pcm.ts` G.711 tables.

#![allow(clippy::unusual_byte_groupings)]

/// Sample format derived from a mediabunny-style codec string (`pcm-s24be`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmFormat {
    pub data_type: PcmDataType,
    /// Bytes per sample (1, 2, 3, 4, or 8).
    pub sample_size: u8,
    pub little_endian: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmDataType {
    Unsigned,
    Signed,
    Float,
    Ulaw,
    Alaw,
}

impl PcmFormat {
    /// Parse mediabunny / WebCodecs PCM codec strings.
    ///
    /// `mediabunny: src/codec.ts` `parsePcmCodec`.
    pub fn from_codec_string(codec: &str) -> Option<Self> {
        if codec == "ulaw" {
            return Some(Self {
                data_type: PcmDataType::Ulaw,
                sample_size: 1,
                little_endian: true,
            });
        }
        if codec == "alaw" {
            return Some(Self {
                data_type: PcmDataType::Alaw,
                sample_size: 1,
                little_endian: true,
            });
        }
        // pcm-(u|s|f)(8|16|24|32|64)(be)?
        let rest = codec.strip_prefix("pcm-")?;
        let (data_type, rest) = match rest.as_bytes().first()? {
            b'u' => (PcmDataType::Unsigned, &rest[1..]),
            b's' => (PcmDataType::Signed, &rest[1..]),
            b'f' => (PcmDataType::Float, &rest[1..]),
            _ => return None,
        };
        let (bits, be) = if let Some(n) = rest.strip_suffix("be") {
            (n, true)
        } else {
            (rest, false)
        };
        let bits: u16 = bits.parse().ok()?;
        let sample_size = match bits {
            8 => 1,
            16 => 2,
            24 => 3,
            32 => 4,
            64 => 8,
            _ => return None,
        };
        if matches!(data_type, PcmDataType::Float) && !matches!(bits, 32 | 64) {
            return None;
        }
        Some(Self {
            data_type,
            sample_size,
            little_endian: !be,
        })
    }

    pub fn codec_string(self) -> String {
        match self.data_type {
            PcmDataType::Ulaw => return "ulaw".into(),
            PcmDataType::Alaw => return "alaw".into(),
            _ => {}
        }
        let prefix = match self.data_type {
            PcmDataType::Unsigned => "u",
            PcmDataType::Signed => "s",
            PcmDataType::Float => "f",
            PcmDataType::Ulaw | PcmDataType::Alaw => unreachable!(),
        };
        let bits = u16::from(self.sample_size) * 8;
        if self.little_endian {
            format!("pcm-{prefix}{bits}")
        } else {
            format!("pcm-{prefix}{bits}be")
        }
    }

    /// Interleaved PCM packet → one `f32` plane per channel.
    ///
    /// Output is always f32 planar for `AudioBuffer` scheduling (simpler than
    /// mediabunny's u8/s16/s32 intermediate for `AudioData`).
    pub fn decode_interleaved_f32(
        self,
        data: &[u8],
        channels: u32,
    ) -> Result<Vec<Vec<f32>>, crate::error::MediaError> {
        let channels = channels.max(1) as usize;
        let step = self.sample_size as usize * channels;
        if step == 0 || !data.len().is_multiple_of(step) {
            return Err(crate::error::MediaError::Mp4(format!(
                "PCM packet len {} not divisible by frame size {}",
                data.len(),
                step
            )));
        }
        let frames = data.len() / step;
        let mut planar = vec![vec![0.0f32; frames]; channels];
        let le = self.little_endian;
        for (fi, frame) in data.chunks_exact(step).enumerate() {
            for (ch, plane) in planar.iter_mut().enumerate() {
                let off = ch * self.sample_size as usize;
                let sample = &frame[off..off + self.sample_size as usize];
                plane[fi] = self.read_sample_f32(sample, le);
            }
        }
        Ok(planar)
    }

    fn read_sample_f32(self, bytes: &[u8], le: bool) -> f32 {
        match (self.data_type, self.sample_size) {
            (PcmDataType::Unsigned, 1) => {
                let v = i16::from(bytes[0]) - 128;
                f32::from(v) / 128.0
            }
            (PcmDataType::Signed, 1) => f32::from(bytes[0] as i8) / 128.0,
            (PcmDataType::Ulaw, 1) => f32::from(from_ulaw(bytes[0])) / 32768.0,
            (PcmDataType::Alaw, 1) => f32::from(from_alaw(bytes[0])) / 32768.0,
            (PcmDataType::Unsigned, 2) => {
                let v = read_u16(bytes, le) as i32 - 32_768;
                v as f32 / 32_768.0
            }
            (PcmDataType::Signed, 2) => f32::from(read_i16(bytes, le)) / 32_768.0,
            (PcmDataType::Unsigned, 3) => {
                let v = read_u24(bytes, le) as i32 - (1 << 23);
                v as f32 / (1 << 23) as f32
            }
            (PcmDataType::Signed, 3) => read_i24(bytes, le) as f32 / (1 << 23) as f32,
            (PcmDataType::Unsigned, 4) => {
                let v = read_u32(bytes, le) as i64 - (1_i64 << 31);
                v as f32 / (1_i64 << 31) as f32
            }
            (PcmDataType::Signed, 4) => read_i32(bytes, le) as f32 / (1_i64 << 31) as f32,
            (PcmDataType::Float, 4) => read_f32(bytes, le),
            (PcmDataType::Float, 8) => read_f64(bytes, le) as f32,
            _ => 0.0,
        }
    }
}

fn read_u16(b: &[u8], le: bool) -> u16 {
    let a = [b[0], b[1]];
    if le {
        u16::from_le_bytes(a)
    } else {
        u16::from_be_bytes(a)
    }
}

fn read_i16(b: &[u8], le: bool) -> i16 {
    let a = [b[0], b[1]];
    if le {
        i16::from_le_bytes(a)
    } else {
        i16::from_be_bytes(a)
    }
}

/// Slice planar PCM into ~`chunk_frames` for WSOLA (rate ≠ 1).
///
/// QT packets can be 0.5–1 s; feeding them whole into `TimeStretcher` stalls the
/// main thread → audio underruns even at tiny rate deltas. AAC-sized chunks
/// (~1024) keep stretch incremental. `mediabunny: no equivalent` (no rate API).
/// Wired from wasm `audio::output` only — keep available for native unit tests.
#[cfg_attr(
    not(all(target_arch = "wasm32", feature = "webcodecs")),
    allow(dead_code)
)]
pub fn slice_planar_for_stretch(
    planar: Vec<Vec<f32>>,
    base_ts: f64,
    sample_rate: f64,
    chunk_frames: usize,
) -> Vec<(f64, Vec<Vec<f32>>)> {
    let chunk_frames = chunk_frames.max(1);
    let sr = sample_rate.max(1.0);
    if planar.is_empty() || planar[0].is_empty() {
        return Vec::new();
    }
    let frames = planar[0].len();
    if frames <= chunk_frames {
        return vec![(base_ts, planar)];
    }
    let mut out = Vec::with_capacity(frames.div_ceil(chunk_frames));
    let mut offset = 0usize;
    while offset < frames {
        let end = (offset + chunk_frames).min(frames);
        let slice: Vec<Vec<f32>> = planar.iter().map(|ch| ch[offset..end].to_vec()).collect();
        let ts = base_ts + offset as f64 / sr;
        out.push((ts, slice));
        offset = end;
    }
    out
}

fn read_u24(b: &[u8], le: bool) -> u32 {
    if le {
        u32::from(b[0]) | (u32::from(b[1]) << 8) | (u32::from(b[2]) << 16)
    } else {
        (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2])
    }
}

fn read_i24(b: &[u8], le: bool) -> i32 {
    let u = read_u24(b, le);
    if u & 0x80_0000 != 0 {
        (u | 0xFF00_0000) as i32
    } else {
        u as i32
    }
}

fn read_u32(b: &[u8], le: bool) -> u32 {
    let a = [b[0], b[1], b[2], b[3]];
    if le {
        u32::from_le_bytes(a)
    } else {
        u32::from_be_bytes(a)
    }
}

fn read_i32(b: &[u8], le: bool) -> i32 {
    let a = [b[0], b[1], b[2], b[3]];
    if le {
        i32::from_le_bytes(a)
    } else {
        i32::from_be_bytes(a)
    }
}

fn read_f32(b: &[u8], le: bool) -> f32 {
    let a = [b[0], b[1], b[2], b[3]];
    if le {
        f32::from_le_bytes(a)
    } else {
        f32::from_be_bytes(a)
    }
}

fn read_f64(b: &[u8], le: bool) -> f64 {
    let a = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
    if le {
        f64::from_le_bytes(a)
    } else {
        f64::from_be_bytes(a)
    }
}

/// `mediabunny: src/pcm.ts` `fromUlaw` (returns ~s16 range).
fn from_ulaw(u8: u8) -> i16 {
    const MULAW_BIAS: i32 = 33;
    let mut number = (!u8) as i32;
    let sign = if number & 0x80 != 0 {
        number &= !(1 << 7);
        -1
    } else {
        0
    };
    let position = ((number & 0xF0) >> 4) + 5;
    let decoded = ((1 << position) | ((number & 0x0F) << (position - 4)) | (1 << (position - 5)))
        - MULAW_BIAS;
    let sample = if sign == 0 { decoded } else { -decoded };
    (sample << 2) as i16
}

/// `mediabunny: src/pcm.ts` `fromAlaw`.
fn from_alaw(u8: u8) -> i16 {
    let mut number = (u8 ^ 0x55) as i32;
    let sign = if number & 0x80 != 0 {
        number &= !(1 << 7);
        -1
    } else {
        0
    };
    let position = ((number & 0xF0) >> 4) + 4;
    let decoded = if position != 4 {
        (1 << position) | ((number & 0x0F) << (position - 4)) | (1 << (position - 5))
    } else {
        (number << 1) | 1
    };
    let sample = if sign == 0 { -decoded } else { decoded };
    (sample << 3) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_codec_strings() {
        let s24be = PcmFormat::from_codec_string("pcm-s24be").unwrap();
        assert_eq!(s24be.sample_size, 3);
        assert!(!s24be.little_endian);
        assert_eq!(s24be.codec_string(), "pcm-s24be");

        let s16 = PcmFormat::from_codec_string("pcm-s16").unwrap();
        assert!(s16.little_endian);
        assert_eq!(s16.codec_string(), "pcm-s16");

        assert!(PcmFormat::from_codec_string("ulaw").is_some());
        assert!(PcmFormat::from_codec_string("mp4a.40.2").is_none());
    }

    #[test]
    fn decode_s16le_stereo() {
        let fmt = PcmFormat::from_codec_string("pcm-s16").unwrap();
        // One frame: L=0x0100 (256), R=0x0000
        let data = [0x00, 0x01, 0x00, 0x00];
        let planar = fmt.decode_interleaved_f32(&data, 2).unwrap();
        assert_eq!(planar.len(), 2);
        assert_eq!(planar[0].len(), 1);
        assert!((planar[0][0] - (256.0 / 32768.0)).abs() < 1e-6);
        assert_eq!(planar[1][0], 0.0);
    }

    #[test]
    fn decode_s24be_mono() {
        let fmt = PcmFormat::from_codec_string("pcm-s24be").unwrap();
        // 0x7FFFFF ≈ full scale positive
        let data = [0x7F, 0xFF, 0xFF];
        let planar = fmt.decode_interleaved_f32(&data, 1).unwrap();
        assert!((planar[0][0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn slice_planar_for_stretch_splits_and_stamps() {
        let planar = vec![vec![0.0f32; 2500], vec![1.0f32; 2500]];
        let chunks = slice_planar_for_stretch(planar, 1.0, 1000.0, 1024);
        assert_eq!(chunks.len(), 3);
        assert!((chunks[0].0 - 1.0).abs() < 1e-12);
        assert_eq!(chunks[0].1[0].len(), 1024);
        assert!((chunks[1].0 - (1.0 + 1024.0 / 1000.0)).abs() < 1e-12);
        assert_eq!(chunks[1].1[0].len(), 1024);
        assert_eq!(chunks[2].1[0].len(), 2500 - 2048);
        assert_eq!(chunks[2].1[1][0], 1.0);
    }

    #[test]
    fn slice_planar_for_stretch_keeps_short() {
        let planar = vec![vec![0.5f32; 100]];
        let chunks = slice_planar_for_stretch(planar, 0.0, 48_000.0, 1024);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].1[0].len(), 100);
    }
}
