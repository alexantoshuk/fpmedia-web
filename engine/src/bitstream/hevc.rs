//! H.265 / HEVC access-unit key vs delta from NAL types (HVCC length-prefixed).
//!
//! Port of mediabunny `determineVideoPacketType` for `hevc` in `codec-data.ts`:
//! scan length-prefixed NALs; type &lt; BLA_W_LP (16) ⇒ delta;
//! 16…RSV_IRAP_VCL23 (23) ⇒ key (IRAP); other NAL types keep scanning.
//!
//! Also: Chromium first-packet NAL order sanitize (`sanitizeHevcPacketForChromium`)
//! and RASL detection after CRA (`hasHevcRaslPicture`).

use super::PacketType;
use std::collections::HashSet;

/// BLA_W_LP — first IRAP VCL type (ISO/IEC 23008-2 Table 7-1).
const HEVC_NAL_BLA_W_LP: u8 = 16;
/// RSV_IRAP_VCL23 — last reserved IRAP VCL type.
const HEVC_NAL_RSV_IRAP_VCL23: u8 = 23;
const HEVC_NAL_RASL_N: u8 = 8;
const HEVC_NAL_RASL_R: u8 = 9;
const HEVC_NAL_AUD: u8 = 35;
const HEVC_NAL_EOS: u8 = 36;
const HEVC_NAL_EOB: u8 = 37;

/// Chromium HEVC AU NAL order state (mediabunny `HevcNaluOrderState`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HevcNaluOrderState {
    AudAllowed = 0,
    BeforeFirstVcl = 1,
    AfterFirstVcl = 2,
    EoBitstreamAllowed = 3,
    NoMoreDataAllowed = 4,
}

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

/// Iterate length-prefixed NAL payloads: `(payload_offset, payload_len)`.
fn iter_hevc_nals(data: &[u8], length_size: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut offset = 0usize;
    std::iter::from_fn(move || {
        if offset + length_size > data.len() {
            return None;
        }
        let nal_len = read_nal_len(data, offset, length_size)?;
        let start = offset + length_size;
        if nal_len == 0 || start + nal_len > data.len() {
            return None;
        }
        offset = start + nal_len;
        Some((start, nal_len))
    })
}

fn concat_length_prefixed(nals: &[&[u8]], length_size: usize) -> Vec<u8> {
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

/// Decide key/delta by inspecting HVCC (length-prefixed) NAL units.
///
/// Returns `None` if the AU is empty or truncated (cannot decide).
pub fn determine_hevc_packet_type(data: &[u8], length_size: usize) -> Option<PacketType> {
    if length_size == 0 || length_size > 4 || data.is_empty() {
        return None;
    }

    let mut saw_nal = false;
    for (start, nal_len) in iter_hevc_nals(data, length_size) {
        let nal = &data[start..start + nal_len];
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
    }

    if saw_nal {
        Some(PacketType::Delta)
    } else {
        None
    }
}

/// True if the AU contains a RASL_N / RASL_R VCL (mediabunny `hasHevcRaslPicture`).
///
/// After a CRA (open-GOP) key, browsers mishandle these — drop until past them.
pub fn has_hevc_rasl_picture(data: &[u8], length_size: usize) -> bool {
    if length_size == 0 || length_size > 4 || data.is_empty() {
        return false;
    }
    for (start, nal_len) in iter_hevc_nals(data, length_size) {
        let ty = hevc_nal_unit_type(data[start]);
        if ty == HEVC_NAL_RASL_N || ty == HEVC_NAL_RASL_R {
            return true;
        }
        let _ = nal_len;
    }
    false
}

/// Strip HEVC NAL order violations that trip Chromium's `hevc.cc` validator.
///
/// mediabunny `sanitizeHevcPacketForChromium` — Chromium issue 507611247.
/// Returns `None` when the AU is already clean.
pub fn sanitize_hevc_packet_for_chromium(data: &[u8], length_size: usize) -> Option<Vec<u8>> {
    if length_size == 0 || length_size > 4 || data.is_empty() {
        return None;
    }

    let mut removed: HashSet<usize> = HashSet::new();
    let mut order = HevcNaluOrderState::AudAllowed;

    for (start, nal_len) in iter_hevc_nals(data, length_size) {
        if order == HevcNaluOrderState::NoMoreDataAllowed {
            removed.insert(start);
            continue;
        }
        let ty = hevc_nal_unit_type(data[start]);
        if order == HevcNaluOrderState::EoBitstreamAllowed && ty != HEVC_NAL_EOB {
            removed.insert(start);
            continue;
        }

        let mut remove = false;
        if ty == HEVC_NAL_AUD {
            if order > HevcNaluOrderState::AudAllowed {
                remove = true;
            } else {
                order = HevcNaluOrderState::BeforeFirstVcl;
            }
        } else if ty <= 31 {
            // VCL
            if order > HevcNaluOrderState::AfterFirstVcl {
                remove = true;
            } else {
                order = HevcNaluOrderState::AfterFirstVcl;
            }
        } else if ty == HEVC_NAL_EOS {
            if order != HevcNaluOrderState::AfterFirstVcl {
                remove = true;
            } else {
                order = HevcNaluOrderState::EoBitstreamAllowed;
            }
        } else if ty == HEVC_NAL_EOB {
            if order < HevcNaluOrderState::AfterFirstVcl {
                remove = true;
            } else {
                order = HevcNaluOrderState::NoMoreDataAllowed;
            }
        } else if matches!(ty, 32 | 33 | 34 | 39)
            || (41..=44).contains(&ty)
            || (48..=55).contains(&ty)
        {
            // VPS, SPS, PPS, PREFIX_SEI, RSV_NVCL41..44, UNSPEC48..55
            if order > HevcNaluOrderState::BeforeFirstVcl {
                remove = true;
            } else {
                order = HevcNaluOrderState::BeforeFirstVcl;
            }
        } else if matches!(ty, 38 | 40) || (45..=47).contains(&ty) || (56..=63).contains(&ty) {
            // FD, SUFFIX_SEI, RSV_NVCL45..47, UNSPEC56..63
            if order < HevcNaluOrderState::AfterFirstVcl {
                remove = true;
            }
        }
        if remove {
            removed.insert(start);
        }
        let _ = nal_len;
    }

    if removed.is_empty() {
        return None;
    }

    let kept: Vec<&[u8]> = iter_hevc_nals(data, length_size)
        .filter(|(start, _)| !removed.contains(start))
        .map(|(start, len)| &data[start..start + len])
        .collect();
    Some(concat_length_prefixed(&kept, length_size))
}

/// Build a length-prefixed AU: `[len][nal…]…` with `length_size` bytes of length.
#[cfg(test)]
fn prefixed_au(length_size: usize, nals: &[&[u8]]) -> Vec<u8> {
    concat_length_prefixed(nals, length_size)
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

    #[test]
    fn rasl_picture_detected() {
        // RASL_N type 8 → first byte 0x10
        let data = prefixed_au(4, &[&[0x10, 0x01]]);
        assert!(has_hevc_rasl_picture(&data, 4));
        let trail = prefixed_au(4, &[&[0x02, 0x01]]);
        assert!(!has_hevc_rasl_picture(&trail, 4));
    }

    #[test]
    fn sanitize_drops_suffix_sei_before_vcl() {
        // SUFFIX_SEI type 40 → 0x50 before VCL IRAP — illegal order for Chromium.
        let dirty = prefixed_au(4, &[&[0x50, 0x01], &[0x20, 0x01]]);
        let cleaned = sanitize_hevc_packet_for_chromium(&dirty, 4).expect("changed");
        assert!(
            !has_suffix_sei(&cleaned, 4),
            "suffix SEI should be stripped before VCL"
        );
        assert_eq!(
            determine_hevc_packet_type(&cleaned, 4),
            Some(PacketType::Key)
        );
    }

    #[test]
    fn sanitize_noop_on_clean_vps_idr() {
        let clean = prefixed_au(4, &[&[0x40, 0x01], &[0x28, 0x01]]); // VPS + IDR_N_LP (20→0x28)
        assert!(sanitize_hevc_packet_for_chromium(&clean, 4).is_none());
    }

    fn has_suffix_sei(data: &[u8], length_size: usize) -> bool {
        iter_hevc_nals(data, length_size).any(|(start, _)| hevc_nal_unit_type(data[start]) == 40)
    }
}
