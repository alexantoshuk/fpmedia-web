//! H.264 / AVC access-unit key vs delta from NAL types (AVCC length-prefixed).
//!
//! Port of mediabunny `determineVideoPacketType` for `avc` in `codec-data.ts`:
//! scan length-prefixed NALs; types 1–4 ⇒ delta; type 5 (IDR) ⇒ key.

/// Packet type from bitstream inspection (not container sync flags).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketType {
    Key,
    Delta,
}

/// Access unit delimiter (ISO/IEC 14496-10).
const NAL_AUD: u8 = 9;

/// `lengthSizeMinusOne + 1` from an `avcC` box payload (ISO/IEC 14496-15).
///
/// Byte 4: top 6 bits reserved (`111111`), bottom 2 bits = length size minus one.
pub fn avc_nal_length_size(avcc: &[u8]) -> Option<usize> {
    let b = *avcc.get(4)?;
    let size = usize::from(b & 0b11) + 1;
    (1..=4).contains(&size).then_some(size)
}

fn read_nal_len(data: &[u8], offset: usize, length_size: usize) -> Option<usize> {
    if offset + length_size > data.len() {
        return None;
    }
    let mut nal_len: usize = 0;
    for i in 0..length_size {
        nal_len = (nal_len << 8) | usize::from(data[offset + i]);
    }
    Some(nal_len)
}

/// Decide key/delta by inspecting AVCC (length-prefixed) NAL units.
///
/// Returns `None` if the AU is empty or truncated (cannot decide).
pub fn determine_avc_packet_type(data: &[u8], length_size: usize) -> Option<PacketType> {
    if length_size == 0 || length_size > 4 || data.is_empty() {
        return None;
    }

    let mut offset = 0;
    let mut saw_nal = false;
    while offset + length_size <= data.len() {
        let Some(nal_len) = read_nal_len(data, offset, length_size) else {
            break;
        };
        offset += length_size;
        if nal_len == 0 || offset + nal_len > data.len() {
            return if saw_nal {
                Some(PacketType::Delta)
            } else {
                None
            };
        }
        let nal_header = data[offset];
        let nal_type = nal_header & 0x1f;
        saw_nal = true;

        // 1–4: non-IDR slice / data partition → definite delta (stop).
        if (1..=4).contains(&nal_type) {
            return Some(PacketType::Delta);
        }
        // 5: IDR → key.
        if nal_type == 5 {
            return Some(PacketType::Key);
        }
        // 6 SEI / 7+ (SPS, PPS, AUD, …): keep scanning (Recovery Point SEI not yet).
        offset += nal_len;
    }

    if saw_nal {
        Some(PacketType::Delta)
    } else {
        None
    }
}

/// Chromium first-packet workaround (mediabunny `VideoDecoderWrapper.decode`).
///
/// Strips AUD NALs that confuse Chromium's keyframe detector
/// (<https://issues.chromium.org/issues/470109459>, mediabunny #396) and drops
/// reserved NAL types 20–31. Returns `None` when the AU is already clean.
pub fn sanitize_avc_packet_for_chromium(data: &[u8], length_size: usize) -> Option<Vec<u8>> {
    if length_size == 0 || length_size > 4 || data.is_empty() {
        return None;
    }

    let mut kept: Vec<&[u8]> = Vec::new();
    let mut has_frame_data = false;
    let mut changed = false;
    let mut offset = 0;

    while offset + length_size <= data.len() {
        let Some(nal_len) = read_nal_len(data, offset, length_size) else {
            break;
        };
        let nal_start = offset + length_size;
        if nal_len == 0 || nal_start + nal_len > data.len() {
            break;
        }
        let nal = &data[nal_start..nal_start + nal_len];
        let nal_type = nal.first().copied().unwrap_or(0) & 0x1f;
        has_frame_data |= (1..=5).contains(&nal_type);

        if nal_type == NAL_AUD {
            if has_frame_data {
                // AUD after VCL → treat as end of AU (do not keep the AUD).
                changed = true;
                break;
            }
            // AUD with leading NALs before it: drop those leading units (keep AUD).
            if !kept.is_empty() {
                kept.clear();
                changed = true;
            }
        } else if (20..=31).contains(&nal_type) {
            changed = true;
            offset = nal_start + nal_len;
            continue;
        }

        kept.push(nal);
        offset = nal_start + nal_len;
    }

    if !changed {
        return None;
    }

    let mut out = Vec::with_capacity(data.len());
    for nal in kept {
        match length_size {
            1 => out.push(nal.len() as u8),
            2 => out.extend_from_slice(&(nal.len() as u16).to_be_bytes()),
            3 => {
                let len = nal.len() as u32;
                out.push(((len >> 16) & 0xff) as u8);
                out.push(((len >> 8) & 0xff) as u8);
                out.push((len & 0xff) as u8);
            }
            4 => out.extend_from_slice(&(nal.len() as u32).to_be_bytes()),
            _ => return None,
        }
        out.extend_from_slice(nal);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn au(nals: &[&[u8]], length_size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            let len = nal.len();
            match length_size {
                1 => out.push(len as u8),
                2 => out.extend_from_slice(&(len as u16).to_be_bytes()),
                4 => out.extend_from_slice(&(len as u32).to_be_bytes()),
                _ => panic!("bad length_size"),
            }
            out.extend_from_slice(nal);
        }
        out
    }

    #[test]
    fn avcc_length_size_from_byte4() {
        // Typical avcC: lengthSizeMinusOne = 3 → 4-byte lengths.
        let mut avcc = vec![0u8; 5];
        avcc[4] = 0b1111_1100 | 0b11;
        assert_eq!(avc_nal_length_size(&avcc), Some(4));
        avcc[4] = 0b1111_1100 | 0b01;
        assert_eq!(avc_nal_length_size(&avcc), Some(2));
    }

    #[test]
    fn idr_is_key() {
        let data = au(&[&[0x65, 0x88]], 4); // nal_type 5
        assert_eq!(determine_avc_packet_type(&data, 4), Some(PacketType::Key));
    }

    #[test]
    fn non_idr_slice_is_delta() {
        let data = au(&[&[0x41, 0x9a]], 4); // nal_type 1
        assert_eq!(determine_avc_packet_type(&data, 4), Some(PacketType::Delta));
    }

    #[test]
    fn sps_then_idr_is_key() {
        let sps = &[0x67, 0x42]; // type 7
        let idr = &[0x65, 0x00]; // type 5
        let data = au(&[sps, idr], 4);
        assert_eq!(determine_avc_packet_type(&data, 4), Some(PacketType::Key));
    }

    #[test]
    fn sps_then_p_slice_is_delta() {
        let sps = &[0x67, 0x42];
        let p = &[0x41, 0x00];
        let data = au(&[sps, p], 2);
        assert_eq!(determine_avc_packet_type(&data, 2), Some(PacketType::Delta));
    }

    #[test]
    fn lying_sync_demoted_by_type() {
        // Container would say sync; bitstream is P-slice.
        let data = au(&[&[0x01, 0xff]], 4);
        assert_ne!(
            determine_avc_packet_type(&data, 4),
            Some(PacketType::Key)
        );
    }

    #[test]
    fn chromium_strips_nals_before_aud() {
        let sei = &[0x06, 0x00];
        let aud = &[0x09, 0xf0];
        let idr = &[0x65, 0x88];
        let data = au(&[sei, aud, idr], 4);
        let cleaned = sanitize_avc_packet_for_chromium(&data, 4).expect("changed");
        // Leading SEI before AUD dropped; AUD kept (mediabunny).
        assert_eq!(cleaned, au(&[aud, idr], 4));
        assert_eq!(
            determine_avc_packet_type(&cleaned, 4),
            Some(PacketType::Key)
        );
    }

    #[test]
    fn chromium_drops_reserved_nal_types() {
        let reserved = &[0x14, 0x00]; // type 20
        let idr = &[0x65, 0x00];
        let data = au(&[reserved, idr], 4);
        let cleaned = sanitize_avc_packet_for_chromium(&data, 4).expect("changed");
        assert_eq!(cleaned, au(&[idr], 4));
    }

    #[test]
    fn chromium_noop_when_clean() {
        let data = au(&[&[0x65, 0x88]], 4);
        assert!(sanitize_avc_packet_for_chromium(&data, 4).is_none());
    }
}
