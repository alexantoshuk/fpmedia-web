//! QuickTime / MP4 embedded start timecode (`tmcd` track).
//!
//! **mediabunny:** no `tmcd` equivalent — container parse is ours.
//! **re_mp4:** `StsdBoxContent::Unknown` keeps the `tmcd` FourCC but drops the
//! sample-description body, so we walk raw `moov` bytes for timing flags / fps
//! and read sample 0 from `mdat` for the start value (QTFF pp. 106–108).
//!
//! Layout of the `tmcd` sample-description body (after the 16-byte universal
//! sample-entry header) and the 4-byte sample payload match OxideAV/oxideav-mov
//! (`parse_tmcd_sample_description` / `Tmcd::decode_sample`).

use re_mp4::Mp4;
use timecode::{FrameRate, Timecode, TimecodeError};

use crate::error::MediaError;
use crate::source::MediaSource;
use crate::util::block_on;

const TMCD_FLAG_DROP_FRAME: u32 = 0x0000_0001;
const TMCD_FLAG_COUNTER: u32 = 0x0000_0008;

/// Parsed `tmcd` sample-description fields (QTFF p. 106).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmcdDescription {
    pub flags: u32,
    pub time_scale: u32,
    pub frame_duration: u32,
    pub number_of_frames: u8,
}

impl TmcdDescription {
    pub fn is_drop_frame(self) -> bool {
        self.flags & TMCD_FLAG_DROP_FRAME != 0
    }

    pub fn is_counter(self) -> bool {
        self.flags & TMCD_FLAG_COUNTER != 0
    }

    /// Map QTFF timing fields onto our [`FrameRate`].
    pub fn frame_rate(self) -> Option<FrameRate> {
        let playback = if self.frame_duration > 0 {
            self.time_scale as f64 / self.frame_duration as f64
        } else {
            self.number_of_frames as f64
        };
        if let Some(rate) = FrameRate::from_float(playback, self.is_drop_frame()) {
            return Some(rate);
        }
        // Nominal fallback when writers put integer fps in number_of_frames only.
        match (self.number_of_frames, self.is_drop_frame()) {
            (24, _) => Some(FrameRate::Fps24),
            (25, _) => Some(FrameRate::Fps25),
            (30, true) => Some(FrameRate::Fps29_97Df),
            (30, false) => Some(FrameRate::Fps30),
            (50, _) => Some(FrameRate::Fps50),
            (60, true) => Some(FrameRate::Fps59_94Df),
            (60, false) => Some(FrameRate::Fps60),
            _ => None,
        }
    }
}

/// `tmcd` track found in `moov` (description + track id for sample lookup).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmcdTrackInfo {
    pub track_id: u32,
    pub description: TmcdDescription,
}

/// Parse the `tmcd` body after the 16-byte sample-entry header.
pub fn parse_tmcd_sample_description(body: &[u8]) -> Result<TmcdDescription, MediaError> {
    if body.len() < 20 {
        return Err(MediaError::Mp4(
            "tmcd sample description shorter than 20 bytes".into(),
        ));
    }
    let flags = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
    let time_scale = u32::from_be_bytes([body[8], body[9], body[10], body[11]]);
    let frame_duration = u32::from_be_bytes([body[12], body[13], body[14], body[15]]);
    let number_of_frames = body[16];
    Ok(TmcdDescription {
        flags,
        time_scale,
        frame_duration,
        number_of_frames,
    })
}

/// Decode a 4-byte timecode sample into [`Timecode`].
pub fn timecode_from_sample(
    desc: &TmcdDescription,
    payload: &[u8],
) -> Result<Timecode, MediaError> {
    if payload.len() < 4 {
        return Err(MediaError::Mp4(
            "timecode sample payload shorter than 4 bytes".into(),
        ));
    }
    let rate = desc.frame_rate().ok_or_else(|| {
        MediaError::Mp4(format!(
            "unsupported tmcd rate (frames={}, drop={}, scale={}/{})",
            desc.number_of_frames,
            desc.is_drop_frame(),
            desc.time_scale,
            desc.frame_duration
        ))
    })?;

    if desc.is_counter() {
        let counter = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
        return Ok(Timecode::from_frames(i64::from(counter), rate));
    }

    let hours = payload[0];
    let negative = payload[1] & 0x80 != 0;
    let minutes = payload[1] & 0x7f;
    let seconds = payload[2];
    let frames = payload[3];
    if negative {
        return Err(MediaError::Mp4(
            "negative embedded timecode is not supported".into(),
        ));
    }
    Timecode::new(hours, minutes, seconds, frames, rate).map_err(tc_err)
}

fn tc_err(err: TimecodeError) -> MediaError {
    MediaError::Mp4(format!("embedded timecode: {err}"))
}

/// Find the first `tmcd` track description inside a raw `moov` box (with header).
pub fn find_tmcd_in_moov(moov: &[u8]) -> Option<TmcdTrackInfo> {
    let body = box_payload(moov, b"moov")?;
    for trak in child_boxes(body) {
        if &trak[4..8] != b"trak" {
            continue;
        }
        let trak_body = box_payload(trak, b"trak")?;
        let track_id = trak_track_id(trak_body)?;
        let mdia = child_boxes(trak_body)
            .into_iter()
            .find(|b| b.len() >= 8 && &b[4..8] == b"mdia")?;
        let mdia_body = box_payload(mdia, b"mdia")?;
        if !mdia_handler_is_tmcd(mdia_body) {
            continue;
        }
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
        let desc = parse_tmcd_from_stsd(stsd)?;
        return Some(TmcdTrackInfo {
            track_id,
            description: desc,
        });
    }
    None
}

/// Locate a top-level `moov` box inside a full file (or concatenated top-level boxes).
pub fn find_moov_box(file: &[u8]) -> Option<&[u8]> {
    let mut p = 0usize;
    while p + 8 <= file.len() {
        let (size, typ, header_len) = read_box_header(&file[p..])?;
        if size < header_len || p + size > file.len() {
            break;
        }
        let box_bytes = &file[p..p + size];
        if &typ == b"moov" {
            return Some(box_bytes);
        }
        p += size;
    }
    None
}

fn read_tmcd_start_from_info(
    mp4: &Mp4,
    moov: &[u8],
    read_payload: impl FnOnce(u64, usize) -> Result<Vec<u8>, MediaError>,
) -> Result<Option<Timecode>, MediaError> {
    let Some(info) = find_tmcd_in_moov(moov) else {
        return Ok(None);
    };
    let Some(track) = mp4.tracks().get(&info.track_id) else {
        return Ok(None);
    };
    let Some(sample) = track.samples.first() else {
        return Ok(None);
    };
    if sample.size == 0 || sample.size > 64 {
        return Err(MediaError::Mp4(format!(
            "unexpected tmcd sample size {}",
            sample.size
        )));
    }
    let payload = read_payload(sample.offset, sample.size as usize)?;
    Ok(Some(timecode_from_sample(&info.description, &payload)?))
}

/// Read sample 0 of the `tmcd` track (sync-ready sources only).
pub fn read_tmcd_start_sample_blocking(
    mp4: &Mp4,
    moov: &[u8],
    source: &dyn MediaSource,
) -> Result<Option<Timecode>, MediaError> {
    read_tmcd_start_from_info(mp4, moov, |offset, len| block_on(source.read(offset, len)))
}

/// Async variant for progressive / Blob / URL opens.
pub async fn read_tmcd_start_sample(
    mp4: &Mp4,
    moov: &[u8],
    source: &dyn MediaSource,
) -> Result<Option<Timecode>, MediaError> {
    let Some(info) = find_tmcd_in_moov(moov) else {
        return Ok(None);
    };
    let Some(track) = mp4.tracks().get(&info.track_id) else {
        return Ok(None);
    };
    let Some(sample) = track.samples.first() else {
        return Ok(None);
    };
    if sample.size == 0 || sample.size > 64 {
        return Err(MediaError::Mp4(format!(
            "unexpected tmcd sample size {}",
            sample.size
        )));
    }
    let payload = source.read(sample.offset, sample.size as usize).await?;
    Ok(Some(timecode_from_sample(&info.description, &payload)?))
}

fn parse_tmcd_from_stsd(stsd: &[u8]) -> Option<TmcdDescription> {
    // stsd: [size][stsd][ver+flags:4][entry_count:4][entries...]
    if stsd.len() < 16 {
        return None;
    }
    let mut p = 8 + 8; // skip header + ver/flags + entry_count
    while p + 8 <= stsd.len() {
        let (size, typ, header_len) = read_box_header(&stsd[p..])?;
        if size < header_len || p + size > stsd.len() {
            break;
        }
        if &typ == b"tmcd" {
            // Universal sample-entry header is 8 (box hdr already in size) + 8 reserved/data_ref
            // = 16 bytes from start of entry, then tmcd-specific body.
            if size < 16 + 20 {
                return None;
            }
            let entry = &stsd[p..p + size];
            let body = &entry[16..];
            return parse_tmcd_sample_description(body).ok();
        }
        p += size;
    }
    None
}

fn mdia_handler_is_tmcd(mdia_body: &[u8]) -> bool {
    for child in child_boxes(mdia_body) {
        if child.len() < 8 || &child[4..8] != b"hdlr" {
            continue;
        }
        // hdlr: hdr(8) + ver/flags(4) + pre_defined(4) + handler(4)
        if child.len() < 20 {
            return false;
        }
        return &child[16..20] == b"tmcd";
    }
    false
}

fn trak_track_id(trak_body: &[u8]) -> Option<u32> {
    let tkhd = child_boxes(trak_body)
        .into_iter()
        .find(|b| b.len() >= 8 && &b[4..8] == b"tkhd")?;
    // tkhd: hdr(8) + ver/flags(4) + creation/modification + track_id
    if tkhd.len() < 12 {
        return None;
    }
    let version = tkhd[8];
    let id_off = match version {
        0 => 8 + 4 + 8,  // + creation(4) + modification(4)
        1 => 8 + 4 + 16, // + creation(8) + modification(8)
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
        // to EOF — not meaningful inside a parent body walk
        None
    } else {
        Some((size32 as usize, typ, 8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmcd_body_29_97_df() -> Vec<u8> {
        let mut p = vec![0u8; 20];
        p[4..8].copy_from_slice(&TMCD_FLAG_DROP_FRAME.to_be_bytes());
        p[8..12].copy_from_slice(&30_000u32.to_be_bytes());
        p[12..16].copy_from_slice(&1001u32.to_be_bytes());
        p[16] = 30;
        p
    }

    #[test]
    fn parse_description_drop_frame() {
        let d = parse_tmcd_sample_description(&tmcd_body_29_97_df()).unwrap();
        assert!(d.is_drop_frame());
        assert!(!d.is_counter());
        assert_eq!(d.frame_rate(), Some(FrameRate::Fps29_97Df));
    }

    #[test]
    fn sample_record_to_timecode() {
        let d = parse_tmcd_sample_description(&tmcd_body_29_97_df()).unwrap();
        // 01:00:00;00 — valid at 10-minute boundary (no drop)
        let tc = timecode_from_sample(&d, &[1, 0, 0, 0]).unwrap();
        assert_eq!(tc.to_string(), "01:00:00;00");
        assert_eq!(tc.rate(), FrameRate::Fps29_97Df);
    }

    #[test]
    fn sample_counter_to_timecode() {
        let mut body = vec![0u8; 20];
        body[4..8].copy_from_slice(&TMCD_FLAG_COUNTER.to_be_bytes());
        body[8..12].copy_from_slice(&30u32.to_be_bytes());
        body[12..16].copy_from_slice(&1u32.to_be_bytes());
        body[16] = 30;
        let d = parse_tmcd_sample_description(&body).unwrap();
        let tc = timecode_from_sample(&d, &86400u32.to_be_bytes()).unwrap();
        assert_eq!(tc.to_string(), "00:48:00:00"); // 86400 / 30 = 2880s
    }

    #[test]
    fn find_tmcd_in_synthetic_moov() {
        // Minimal: moov/trak/tkhd + mdia/hdlr(tmcd) + minf/stbl/stsd/tmcd
        let tmcd_entry = {
            let mut entry = Vec::new();
            let body = tmcd_body_29_97_df();
            let size = (16 + body.len()) as u32;
            entry.extend_from_slice(&size.to_be_bytes());
            entry.extend_from_slice(b"tmcd");
            entry.extend_from_slice(&[0u8; 6]); // reserved
            entry.extend_from_slice(&1u16.to_be_bytes()); // data_ref_index
            entry.extend_from_slice(&body);
            entry
        };
        let stsd = {
            let mut b = Vec::new();
            let size = (16 + tmcd_entry.len()) as u32;
            b.extend_from_slice(&size.to_be_bytes());
            b.extend_from_slice(b"stsd");
            b.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
            b.extend_from_slice(&1u32.to_be_bytes()); // entry_count
            b.extend_from_slice(&tmcd_entry);
            b
        };
        let stbl = wrap(b"stbl", &stsd);
        let minf = wrap(b"minf", &stbl);
        let hdlr = {
            let mut b = Vec::new();
            // size filled later
            let mut body = Vec::new();
            body.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
            body.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
            body.extend_from_slice(b"tmcd");
            body.extend_from_slice(&[0u8; 12]); // reserved
            body.push(0); // name
            let size = (8 + body.len()) as u32;
            b.extend_from_slice(&size.to_be_bytes());
            b.extend_from_slice(b"hdlr");
            b.extend_from_slice(&body);
            b
        };
        let mdia = wrap(b"mdia", &[hdlr, minf].concat());
        let tkhd = {
            let mut body = Vec::new();
            body.extend_from_slice(&0u32.to_be_bytes()); // ver 0 + flags
            body.extend_from_slice(&0u32.to_be_bytes()); // creation
            body.extend_from_slice(&0u32.to_be_bytes()); // modification
            body.extend_from_slice(&7u32.to_be_bytes()); // track_id = 7
            // pad enough for a plausible tkhd (re_mp4 not used here)
            body.extend_from_slice(&[0u8; 52]);
            let mut b = Vec::new();
            let size = (8 + body.len()) as u32;
            b.extend_from_slice(&size.to_be_bytes());
            b.extend_from_slice(b"tkhd");
            b.extend_from_slice(&body);
            b
        };
        let trak = wrap(b"trak", &[tkhd, mdia].concat());
        let moov = wrap(b"moov", &trak);

        let info = find_tmcd_in_moov(&moov).expect("tmcd track");
        assert_eq!(info.track_id, 7);
        assert!(info.description.is_drop_frame());
        assert_eq!(info.description.number_of_frames, 30);
    }

    fn wrap(typ: &[u8; 4], contents: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        let size = (8 + contents.len()) as u32;
        b.extend_from_slice(&size.to_be_bytes());
        b.extend_from_slice(typ);
        b.extend_from_slice(contents);
        b
    }
}
