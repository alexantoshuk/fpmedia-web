//! QT / ISOBMFF PCM sound sample-description from raw `moov`.
//!
//! `re_mp4` keeps PCM as [`re_mp4::StsdBoxContent::Unknown`] (FourCC only) and
//! drops the body — same gap as `tmcd`. Walk raw moov like `demux/timecode.rs`.
//!
//! Field layout + FourCC→codec map: `mediabunny: src/isobmff/isobmff-demuxer.ts`
//! `stsd` audio branch (`in24` / `sowt` / `twos` / `enda` / `pcmC` / `lpcm`).

use re_mp4::{FourCC, is_pcm_sample_entry};

use crate::audio::pcm::{PcmDataType, PcmFormat};
use crate::error::MediaError;

/// Parsed PCM sample-entry fields needed to open an audio track.
#[derive(Clone, Debug, PartialEq)]
pub struct PcmSoundDescription {
    pub track_id: u32,
    pub fourcc: [u8; 4],
    pub sample_rate: u32,
    pub number_of_channels: u32,
    pub format: PcmFormat,
}

/// Find PCM sound description for `track_id` inside a raw `moov` box (with header).
pub fn find_pcm_in_moov(moov: &[u8], track_id: u32) -> Option<PcmSoundDescription> {
    let body = box_payload(moov, b"moov")?;
    for trak in child_boxes(body) {
        if &trak[4..8] != b"trak" {
            continue;
        }
        let trak_body = box_payload(trak, b"trak")?;
        let id = trak_track_id(trak_body)?;
        if id != track_id {
            continue;
        }
        let mdia = child_boxes(trak_body)
            .into_iter()
            .find(|b| b.len() >= 8 && &b[4..8] == b"mdia")?;
        let mdia_body = box_payload(mdia, b"mdia")?;
        let minf = child_boxes(mdia_body)
            .into_iter()
            .find(|b| b.len() >= 8 && &b[4..8] == b"minf")?;
        let minf_body = box_payload(minf, b"minf")?;
        let stbl = child_boxes(minf_body)
            .into_iter()
            .find(|b| b.len() >= 8 && &b[4..8] == b"stbl")?;
        let stbl_body = box_payload(stbl, b"stbl")?;
        let stsd = child_boxes(stbl_body)
            .into_iter()
            .find(|b| b.len() >= 8 && &b[4..8] == b"stsd")?;
        return parse_pcm_from_stsd(stsd, track_id);
    }
    None
}

fn parse_pcm_from_stsd(stsd: &[u8], track_id: u32) -> Option<PcmSoundDescription> {
    // stsd: hdr(8) + ver/flags(4) + entry_count(4) + entries
    if stsd.len() < 16 {
        return None;
    }
    let stsd_version = stsd[8];
    let mut p = 16usize;
    while p + 8 <= stsd.len() {
        let (size, typ, header_len) = read_box_header(&stsd[p..])?;
        if size < header_len || p + size > stsd.len() {
            break;
        }
        let fourcc = FourCC { value: typ };
        if is_pcm_sample_entry(&fourcc) {
            let entry = &stsd[p..p + size];
            return parse_pcm_sample_entry(entry, stsd_version, track_id);
        }
        p += size;
    }
    None
}

fn parse_pcm_sample_entry(
    entry: &[u8],
    stsd_version: u8,
    track_id: u32,
) -> Option<PcmSoundDescription> {
    // Sample entry: size(4)+type(4)+reserved(6)+data_ref(2) = 16, then QT audio fields.
    if entry.len() < 16 + 20 {
        return None;
    }
    let fourcc = [entry[4], entry[5], entry[6], entry[7]];
    let mut o = 16usize;
    let version = read_u16_be(&entry[o..])?;
    o += 2;
    o += 6; // revision + vendor
    let mut channel_count = u32::from(read_u16_be(&entry[o..])?);
    o += 2;
    let mut sample_size_bits = u32::from(read_u16_be(&entry[o..])?);
    o += 2;
    o += 4; // compression_id + packet_size
    let mut sample_rate = (read_u32_be(&entry[o..])? / 0x1_0000).max(1);
    o += 4;

    let mut lpcm_flags: Option<u32> = None;
    if stsd_version == 0 && version > 0 {
        if version == 1 {
            if entry.len() < o + 16 {
                return None;
            }
            o += 4; // samples_per_packet
            sample_size_bits = 8 * read_u32_be(&entry[o..])?;
            o += 4;
            o += 8; // bytes_per_packet + bytes_per_frame
        } else if version == 2 {
            if entry.len() < o + 4 + 8 + 4 + 4 + 4 + 4 + 8 {
                return None;
            }
            o += 4; // sizeOfStructOnly
            sample_rate = read_f64_be(&entry[o..])?.round().max(1.0) as u32;
            o += 8;
            channel_count = read_u32_be(&entry[o..])?.max(1);
            o += 4;
            o += 4; // 0x7f000000
            sample_size_bits = read_u32_be(&entry[o..])?;
            o += 4;
            lpcm_flags = Some(read_u32_be(&entry[o..])?);
            o += 4;
            o += 8; // constBytesPerAudioPacket + constLPCMFramesPerAudioPacket
        }
    }

    let mut little_endian = false;
    let mut pcm_sample_size: Option<u32> = None;
    // Child boxes after fixed fields. QT often nests `enda` under `wave`
    // (`mediabunny: isobmff-demuxer.ts` `case 'wave'` → `readContiguousBoxes`).
    if o < entry.len() {
        scan_pcm_side_boxes(&entry[o..], &mut little_endian, &mut pcm_sample_size);
    }

    let format = map_fourcc_to_format(
        &fourcc,
        sample_size_bits,
        little_endian,
        pcm_sample_size,
        lpcm_flags,
    )?;

    Some(PcmSoundDescription {
        track_id,
        fourcc,
        sample_rate,
        number_of_channels: channel_count.max(1),
        format,
    })
}

fn map_fourcc_to_format(
    fourcc: &[u8; 4],
    sample_size_bits: u32,
    little_endian: bool,
    pcm_sample_size: Option<u32>,
    lpcm_flags: Option<u32>,
) -> Option<PcmFormat> {
    match fourcc {
        b"ulaw" => Some(PcmFormat {
            data_type: PcmDataType::Ulaw,
            sample_size: 1,
            little_endian: true,
        }),
        b"alaw" => Some(PcmFormat {
            data_type: PcmDataType::Alaw,
            sample_size: 1,
            little_endian: true,
        }),
        b"raw " => Some(PcmFormat {
            data_type: PcmDataType::Unsigned,
            sample_size: 1,
            little_endian: true,
        }),
        b"twos" => match sample_size_bits {
            8 => Some(PcmFormat {
                data_type: PcmDataType::Signed,
                sample_size: 1,
                little_endian: true,
            }),
            16 => Some(PcmFormat {
                data_type: PcmDataType::Signed,
                sample_size: 2,
                little_endian,
            }),
            _ => None,
        },
        b"sowt" => match sample_size_bits {
            8 => Some(PcmFormat {
                data_type: PcmDataType::Signed,
                sample_size: 1,
                little_endian: true,
            }),
            16 => Some(PcmFormat {
                data_type: PcmDataType::Signed,
                sample_size: 2,
                little_endian: true,
            }),
            _ => None,
        },
        b"in24" => Some(PcmFormat {
            data_type: PcmDataType::Signed,
            sample_size: 3,
            little_endian,
        }),
        b"in32" => Some(PcmFormat {
            data_type: PcmDataType::Signed,
            sample_size: 4,
            little_endian,
        }),
        b"fl32" => Some(PcmFormat {
            data_type: PcmDataType::Float,
            sample_size: 4,
            little_endian,
        }),
        b"fl64" => Some(PcmFormat {
            data_type: PcmDataType::Float,
            sample_size: 8,
            little_endian,
        }),
        b"ipcm" => {
            let bits = pcm_sample_size?;
            let sample_size = match bits {
                16 => 2,
                24 => 3,
                32 => 4,
                _ => return None,
            };
            Some(PcmFormat {
                data_type: PcmDataType::Signed,
                sample_size,
                little_endian,
            })
        }
        b"fpcm" => {
            let bits = pcm_sample_size?;
            let sample_size = match bits {
                32 => 4,
                64 => 8,
                _ => return None,
            };
            Some(PcmFormat {
                data_type: PcmDataType::Float,
                sample_size,
                little_endian,
            })
        }
        b"lpcm" => {
            let flags = lpcm_flags?;
            let bytes = ((sample_size_bits + 7) >> 3) as u8;
            let is_float = flags & 1 != 0;
            let is_be = flags & 2 != 0;
            if is_float {
                if sample_size_bits == 32 {
                    Some(PcmFormat {
                        data_type: PcmDataType::Float,
                        sample_size: 4,
                        little_endian: !is_be,
                    })
                } else if sample_size_bits == 64 {
                    Some(PcmFormat {
                        data_type: PcmDataType::Float,
                        sample_size: 8,
                        little_endian: !is_be,
                    })
                } else {
                    None
                }
            } else if matches!(bytes, 1..=4) {
                Some(PcmFormat {
                    data_type: PcmDataType::Signed,
                    sample_size: bytes,
                    little_endian: !is_be,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Walk sample-entry / `wave` children for `enda` + `pcmC`.
///
/// `mediabunny: src/isobmff/isobmff-demuxer.ts` `wave` / `enda` / `pcmC`.
fn scan_pcm_side_boxes(body: &[u8], little_endian: &mut bool, pcm_sample_size: &mut Option<u32>) {
    for child in child_boxes(body) {
        if child.len() < 8 {
            continue;
        }
        let typ = &child[4..8];
        if typ == b"enda" && child.len() >= 10 {
            // mediabunny: pcmLittleEndian = !!(u16be & 0xff) — FFmpeg/QT convention.
            *little_endian = read_u16_be(&child[8..]).unwrap_or(0) & 0xff != 0;
        } else if typ == b"pcmC" && child.len() >= 14 {
            // ver/flags(4) + formatFlags(1) + pcmSampleSize(1)
            let format_flags = child[12];
            *little_endian = format_flags & 0x01 != 0;
            *pcm_sample_size = Some(u32::from(child[13]));
        } else if typ == b"wave"
            && let Some(inner) = box_payload(child, b"wave")
        {
            scan_pcm_side_boxes(inner, little_endian, pcm_sample_size);
        }
    }
}

/// Map a PCM FourCC without moov body — only unambiguous codecs (`ulaw`/`alaw`).
pub fn pcm_from_fourcc_only(fourcc: &FourCC) -> Result<Option<PcmFormat>, MediaError> {
    if !is_pcm_sample_entry(fourcc) {
        return Ok(None);
    }
    Ok(re_mp4::pcm_codec_string(fourcc).and_then(PcmFormat::from_codec_string))
}

fn trak_track_id(trak_body: &[u8]) -> Option<u32> {
    let tkhd = child_boxes(trak_body)
        .into_iter()
        .find(|b| b.len() >= 8 && &b[4..8] == b"tkhd")?;
    if tkhd.len() < 12 {
        return None;
    }
    let version = tkhd[8];
    let id_off = match version {
        0 => 8 + 4 + 8,
        1 => 8 + 4 + 16,
        _ => return None,
    };
    if tkhd.len() < id_off + 4 {
        return None;
    }
    Some(u32::from_be_bytes([
        tkhd[id_off],
        tkhd[id_off + 1],
        tkhd[id_off + 2],
        tkhd[id_off + 3],
    ]))
}

fn box_payload<'a>(box_bytes: &'a [u8], expect_typ: &[u8; 4]) -> Option<&'a [u8]> {
    if box_bytes.len() < 8 || &box_bytes[4..8] != expect_typ {
        return None;
    }
    let (size, _, header_len) = read_box_header(box_bytes)?;
    if size < header_len || size > box_bytes.len() {
        return None;
    }
    Some(&box_bytes[header_len..size])
}

fn child_boxes(parent_body: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 8 <= parent_body.len() {
        let Some((size, _, header_len)) = read_box_header(&parent_body[p..]) else {
            break;
        };
        if size < header_len || p + size > parent_body.len() {
            break;
        }
        out.push(&parent_body[p..p + size]);
        p += size;
    }
    out
}

fn read_box_header(data: &[u8]) -> Option<(usize, [u8; 4], usize)> {
    if data.len() < 8 {
        return None;
    }
    let size32 = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let typ = [data[4], data[5], data[6], data[7]];
    if size32 == 1 {
        if data.len() < 16 {
            return None;
        }
        let size64 = u64::from_be_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]);
        let size = usize::try_from(size64).ok()?;
        Some((size, typ, 16))
    } else if size32 == 0 {
        None
    } else {
        Some((size32 as usize, typ, 8))
    }
}

fn read_u16_be(data: &[u8]) -> Option<u16> {
    if data.len() < 2 {
        return None;
    }
    Some(u16::from_be_bytes([data[0], data[1]]))
}

fn read_u32_be(data: &[u8]) -> Option<u32> {
    if data.len() < 4 {
        return None;
    }
    Some(u32::from_be_bytes([data[0], data[1], data[2], data[3]]))
}

fn read_f64_be(data: &[u8]) -> Option<f64> {
    if data.len() < 8 {
        return None;
    }
    Some(f64::from_be_bytes([
        data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_in24_be_default() {
        let f = map_fourcc_to_format(b"in24", 24, false, None, None).unwrap();
        assert_eq!(f.codec_string(), "pcm-s24be");
    }

    #[test]
    fn map_in24_le() {
        let f = map_fourcc_to_format(b"in24", 24, true, None, None).unwrap();
        assert_eq!(f.codec_string(), "pcm-s24");
    }

    #[test]
    fn map_sowt_s16() {
        let f = map_fourcc_to_format(b"sowt", 16, false, None, None).unwrap();
        assert_eq!(f.codec_string(), "pcm-s16");
    }

    #[test]
    fn enda_inside_wave_sets_le() {
        // Minimal: wave { enda = 1 } — QT nesting from ffmpeg in24 plates.
        let mut wave = Vec::new();
        // enda box: size=10, 'enda', u16be=1
        wave.extend_from_slice(&10u32.to_be_bytes());
        wave.extend_from_slice(b"enda");
        wave.extend_from_slice(&1u16.to_be_bytes());
        let mut body = Vec::new();
        let wave_size = (8 + wave.len()) as u32;
        body.extend_from_slice(&wave_size.to_be_bytes());
        body.extend_from_slice(b"wave");
        body.extend_from_slice(&wave);
        let mut le = false;
        let mut sz = None;
        scan_pcm_side_boxes(&body, &mut le, &mut sz);
        assert!(le);
    }
}
