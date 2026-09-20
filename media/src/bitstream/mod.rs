//! Bitstream packet-type checks (mediabunny `verifyKeyPackets` / `determinePacketType`).

mod avc;
mod hevc;

pub use avc::{
    PacketType, accept_avc_recovery_point_sei, avc_nal_length_size, determine_avc_packet_type,
    determine_avc_packet_type_with, sanitize_avc_packet_for_chromium,
};
pub use hevc::{
    determine_hevc_packet_type, has_hevc_rasl_picture, hevc_nal_length_size,
    sanitize_hevc_packet_for_chromium,
};
