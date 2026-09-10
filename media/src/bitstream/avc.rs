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

/// `lengthSizeMinusOne + 1` from an `avcC` box payload (ISO/IEC 14496-15).
///
/// Byte 4: top 6 bits reserved (`111111`), bottom 2 bits = length size minus one.
pub fn avc_nal_length_size(avcc: &[u8]) -> Option<usize> {
    let b = *avcc.get(4)?;
    let size = usize::from(b & 0b11) + 1;
    (1..=4).contains(&size).then_some(size)
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
        let mut nal_len: usize = 0;
        for i in 0..length_size {
            nal_len = (nal_len << 8) | usize::from(data[offset + i]);
        }
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
}
