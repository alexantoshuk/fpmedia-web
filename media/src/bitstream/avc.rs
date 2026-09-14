//! H.264 / AVC access-unit key vs delta from NAL types (AVCC length-prefixed).
//!
//! Port of mediabunny `determineVideoPacketType` for `avc` in `codec-data.ts`:
//! scan length-prefixed NALs; types 1–4 ⇒ delta; type 5 (IDR) ⇒ key;
//! type 6 SEI Recovery Point (`recovery_frame_cnt==0` && `exact_match_flag==1`) ⇒ key
//! when the runtime accepts SEI keys (not Chromium &lt; 145).

/// Packet type from bitstream inspection (not container sync flags).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketType {
    Key,
    Delta,
}

/// Access unit delimiter (ISO/IEC 14496-10).
const NAL_AUD: u8 = 9;
/// Supplemental enhancement information.
const NAL_SEI: u8 = 6;
/// ITU-T H.264 Annex D Table D-1 — `recovery_point` SEI payload type.
const SEI_PAYLOAD_RECOVERY_POINT: u32 = 6;

/// `lengthSizeMinusOne + 1` from an `avcC` box payload (ISO/IEC 14496-15).
///
/// Byte 4: top 6 bits reserved (`111111`), bottom 2 bits = length size minus one.
pub fn avc_nal_length_size(avcc: &[u8]) -> Option<usize> {
    let b = *avcc.get(4)?;
    let size = usize::from(b & 0b11) + 1;
    (1..=4).contains(&size).then_some(size)
}

/// Whether `verifyKeyPackets` may keep Recovery Point SEI AUs as sync.
///
/// Mediabunny (`codec-data.ts` `determineVideoPacketType`) skips SEI keys on
/// Chromium; we additionally accept Chromium ≥ 145 where WebCodecs now treats
/// `recovery_frame_cnt==0 && exact_match_flag=1` as a key (w3c/webcodecs#650).
pub fn accept_avc_recovery_point_sei() -> bool {
    #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
    {
        // Older Chromium bricks on non-IDR keys; ≥145 accepts exact SEI recovery.
        const CHROMIUM_SEI_RECOVERY_KEY_MAJOR: u32 = 145;
        match chromium_major_version() {
            Some(major) => major >= CHROMIUM_SEI_RECOVERY_KEY_MAJOR,
            None => true, // Firefox / Safari / non-Chromium
        }
    }
    #[cfg(not(all(target_arch = "wasm32", feature = "webcodecs")))]
    {
        true
    }
}

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
fn chromium_major_version() -> Option<u32> {
    let window = web_sys::window()?;
    let nav = js_sys::Reflect::get(&window, &"navigator".into()).ok()?;
    let ua = js_sys::Reflect::get(&nav, &"userAgent".into())
        .ok()?
        .as_string()?;
    // mediabunny `isChromium`: Chrome / Chromium / Edge.
    if !(ua.contains("Chrome") || ua.contains("Chromium") || ua.contains("Edg/")) {
        return None;
    }
    // Prefer `Chrome/NNN` (Blink); fall back to `Chromium/NNN`.
    parse_ua_major(&ua, "Chrome/").or_else(|| parse_ua_major(&ua, "Chromium/"))
}

#[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
fn parse_ua_major(ua: &str, marker: &str) -> Option<u32> {
    let rest = ua.split_once(marker)?.1;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
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

/// Strip H.264 emulation-prevention `0x000003` bytes (mediabunny
/// `removeEmulationPreventionBytes`).
fn remove_emulation_prevention_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 3 {
            out.push(0);
            out.push(0);
            i += 3;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

/// MSB-first bit reader over RBSP bytes (mediabunny `shared/bitstream.ts`).
struct BitReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn from_byte_offset(bytes: &'a [u8], byte_offset: usize) -> Self {
        Self {
            bytes,
            pos: byte_offset.saturating_mul(8),
        }
    }

    fn read_bit(&mut self) -> Option<u32> {
        let byte_index = self.pos / 8;
        let byte = *self.bytes.get(byte_index)?;
        let bit_index = 7 - (self.pos & 7);
        let bit = u32::from((byte >> bit_index) & 1);
        self.pos += 1;
        Some(bit)
    }

    fn read_bits(&mut self, n: usize) -> Option<u32> {
        let mut result = 0u32;
        for _ in 0..n {
            result = (result << 1) | self.read_bit()?;
        }
        Some(result)
    }

    /// Unsigned Exp-Golomb `ue(v)` (mediabunny `readExpGolomb`).
    fn read_exp_golomb(&mut self) -> Option<u32> {
        let mut leading_zeros = 0u32;
        while self.read_bit()? == 0 {
            leading_zeros += 1;
            if leading_zeros >= 32 {
                return None;
            }
        }
        let rest = self.read_bits(leading_zeros as usize)?;
        Some(((1u32 << leading_zeros) - 1).saturating_add(rest))
    }
}

/// True when an SEI NAL (type 6) contains Recovery Point with
/// `recovery_frame_cnt == 0` and `exact_match_flag == 1`
/// (w3c/webcodecs#650 / PR #910; mediabunny `codec-data.ts`).
fn sei_nal_is_exact_recovery_point(nal: &[u8]) -> bool {
    if nal.is_empty() || (nal[0] & 0x1f) != NAL_SEI {
        return false;
    }
    let bytes = remove_emulation_prevention_bytes(nal);
    let mut pos = 1; // skip NAL header
    while pos + 1 < bytes.len() {
        // sei_message(): payloadType / payloadSize as 0xFF-extended sums.
        let mut payload_type = 0u32;
        loop {
            let Some(&b) = bytes.get(pos) else {
                return false;
            };
            pos += 1;
            payload_type = payload_type.saturating_add(u32::from(b));
            if b < 255 {
                break;
            }
        }
        let mut payload_size = 0u32;
        loop {
            let Some(&b) = bytes.get(pos) else {
                return false;
            };
            pos += 1;
            payload_size = payload_size.saturating_add(u32::from(b));
            if b < 255 {
                break;
            }
        }
        if payload_type == SEI_PAYLOAD_RECOVERY_POINT {
            let mut bits = BitReader::from_byte_offset(&bytes, pos);
            let Some(recovery_frame_cnt) = bits.read_exp_golomb() else {
                return false;
            };
            let Some(exact_match_flag) = bits.read_bits(1) else {
                return false;
            };
            if recovery_frame_cnt == 0 && exact_match_flag == 1 {
                return true;
            }
        }
        pos = pos.saturating_add(payload_size as usize);
    }
    false
}

/// Decide key/delta by inspecting AVCC (length-prefixed) NAL units.
///
/// Uses [`accept_avc_recovery_point_sei`] for Recovery Point SEI keys.
/// Returns `None` if the AU is empty or truncated (cannot decide).
pub fn determine_avc_packet_type(data: &[u8], length_size: usize) -> Option<PacketType> {
    determine_avc_packet_type_with(data, length_size, accept_avc_recovery_point_sei())
}

/// Like [`determine_avc_packet_type`] with an explicit Recovery Point SEI policy.
pub fn determine_avc_packet_type_with(
    data: &[u8],
    length_size: usize,
    accept_recovery_point_sei: bool,
) -> Option<PacketType> {
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
        let nal_header = nal[0];
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
        // 6 SEI — Recovery Point may count as key (before a following I-slice).
        if nal_type == NAL_SEI
            && accept_recovery_point_sei
            && sei_nal_is_exact_recovery_point(nal)
        {
            return Some(PacketType::Key);
        }
        // Other SEI / SPS / PPS / AUD / …: keep scanning.
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

    /// SEI NAL: recovery_point, recovery_frame_cnt=0, exact_match=1.
    /// Payload byte `0xC0` = bits `1 1 0 00 …` (ue(0), exact=1, broken=0, idc=0).
    fn recovery_point_sei_exact() -> Vec<u8> {
        vec![0x06, 0x06, 0x01, 0xC0, 0x80]
    }

    /// Same shape but exact_match_flag=0 (`0x80` = `1 0 …`).
    fn recovery_point_sei_inexact() -> Vec<u8> {
        vec![0x06, 0x06, 0x01, 0x80, 0x80]
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
    fn recovery_point_sei_exact_is_key_when_accepted() {
        let sei = recovery_point_sei_exact();
        let i_slice = &[0x41, 0x00]; // non-IDR I/P slice
        let data = au(&[&sei, i_slice], 4);
        assert_eq!(
            determine_avc_packet_type_with(&data, 4, true),
            Some(PacketType::Key)
        );
        // Chromium &lt;145 policy: SEI ignored → following slice ⇒ delta (demote).
        assert_eq!(
            determine_avc_packet_type_with(&data, 4, false),
            Some(PacketType::Delta)
        );
    }

    #[test]
    fn recovery_point_sei_inexact_is_delta() {
        let sei = recovery_point_sei_inexact();
        let i_slice = &[0x41, 0x00];
        let data = au(&[&sei, i_slice], 4);
        assert_eq!(
            determine_avc_packet_type_with(&data, 4, true),
            Some(PacketType::Delta)
        );
    }

    #[test]
    fn recovery_point_sei_alone_is_key_when_accepted() {
        let sei = recovery_point_sei_exact();
        let data = au(&[&sei], 4);
        assert_eq!(
            determine_avc_packet_type_with(&data, 4, true),
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

    #[test]
    fn exp_golomb_zero() {
        // Single bit `1` → ue(0).
        let bytes = [0b1000_0000];
        let mut bits = BitReader::from_byte_offset(&bytes, 0);
        assert_eq!(bits.read_exp_golomb(), Some(0));
    }

    #[test]
    fn remove_emulation_prevention() {
        let raw = [0x00, 0x00, 0x03, 0x01];
        assert_eq!(remove_emulation_prevention_bytes(&raw), vec![0x00, 0x00, 0x01]);
    }
}
