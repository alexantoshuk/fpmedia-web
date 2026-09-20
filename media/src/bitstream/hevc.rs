//! H.265 / HEVC access-unit key vs delta from NAL types (HVCC length-prefixed).
//!
//! Port of mediabunny `determineVideoPacketType` for `hevc` in `codec-data.ts`:
//! scan length-prefixed NALs; type &lt; BLA_W_LP (16) ⇒ delta;
//! 16…RSV_IRAP_VCL23 (23) ⇒ key (IRAP); other NAL types keep scanning.

use super::PacketType;

/// BLA_W_LP — first IRAP VCL type (ISO/IEC 23008-2 Table 7-1).
const HEVC_NAL_BLA_W_LP: u8 = 16;
/// RSV_IRAP_VCL23 — last reserved IRAP VCL type.
const HEVC_NAL_RSV_IRAP_VCL23: u8 = 23;

/// `lengthSizeMinusOne + 1` from an `hvcC` box payload (ISO/IEC 14496-15 §8.3.3.1).
///
/// Byte 21: bottom 2 bits = length size minus one (mediabunny `iterateHevcNalUnits`).
pub fn hevc_nal_length_size(hvcc: &[u8]) -> Option<usize> {
    let b = *hvcc.get(21)?;
    let size = usize::from(b & 0b11) + 1;
    (1..=4).contains(&size).then_some(size)
}

/// HEVC NAL unit type from the first header byte (`(byte >> 1) & 0x3F`).
#[inline]
fn hevc_nal_unit_type(first_byte: u8) -> u8 {
    (first_byte >> 1) & 0x3f
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

/// Decide key/delta by inspecting HVCC (length-prefixed) NAL units.
///
/// Returns `None` if the AU is empty or truncated (cannot decide).
pub fn determine_hevc_packet_type(data: &[u8], length_size: usize) -> Option<PacketType> {
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
        let nal = &data[offset..offset + nal_len];
        let nal_type = hevc_nal_unit_type(nal[0]);
        saw_nal = true;

        // VCL below BLA_W_LP → definite delta (mediabunny stops).
        if nal_type < HEVC_NAL_BLA_W_LP {
            return Some(PacketType::Delta);
        }
        // IRAP range BLA_W_LP … RSV_IRAP_VCL23 → key.
        if nal_type <= HEVC_NAL_RSV_IRAP_VCL23 {
            return Some(PacketType::Key);
        }
        // VPS / SPS / PPS / AUD / SEI / …: keep scanning.
        offset += nal_len;
    }

    if saw_nal {
        Some(PacketType::Delta)
    } else {
        None
    }
}

/// Build a length-prefixed AU: `[len][nal…]…` with `length_size` bytes of length.
#[cfg(test)]
fn prefixed_au(length_size: usize, nals: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        let len = nal.len();
        for i in (0..length_size).rev() {
            out.push(((len >> (8 * i)) & 0xff) as u8);
        }
        out.extend_from_slice(nal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hvcc_length_size_from_byte_21() {
        let mut hvcc = vec![0u8; 22];
        hvcc[21] = 0b11; // lengthSizeMinusOne = 3 → 4
        assert_eq!(hevc_nal_length_size(&hvcc), Some(4));
        hvcc[21] = 0b01;
        assert_eq!(hevc_nal_length_size(&hvcc), Some(2));
        assert_eq!(hevc_nal_length_size(&[0u8; 20]), None);
    }

    #[test]
    fn irap_bla_is_key() {
        // type 16 (BLA_W_LP): first byte = 16 << 1 = 0x20
        let data = prefixed_au(4, &[&[0x20, 0x01]]);
        assert_eq!(determine_hevc_packet_type(&data, 4), Some(PacketType::Key));
    }

    #[test]
    fn irap_idr_n_lp_is_key() {
        // type 19 (IDR_N_LP): 19 << 1 = 0x26
        let data = prefixed_au(4, &[&[0x26, 0x01]]);
        assert_eq!(determine_hevc_packet_type(&data, 4), Some(PacketType::Key));
    }

    #[test]
    fn trail_slice_is_delta() {
        // type 1: 1 << 1 = 0x02
        let data = prefixed_au(4, &[&[0x02, 0x01]]);
        assert_eq!(
            determine_hevc_packet_type(&data, 4),
            Some(PacketType::Delta)
        );
    }

    #[test]
    fn vps_then_irap_is_key() {
        // type 32 VPS (0x40), then type 16 BLA
        let data = prefixed_au(4, &[&[0x40, 0x01], &[0x20, 0x01]]);
        assert_eq!(determine_hevc_packet_type(&data, 4), Some(PacketType::Key));
    }

    #[test]
    fn vps_only_is_delta() {
        let data = prefixed_au(4, &[&[0x40, 0x01]]);
        assert_eq!(
            determine_hevc_packet_type(&data, 4),
            Some(PacketType::Delta)
        );
    }
}
