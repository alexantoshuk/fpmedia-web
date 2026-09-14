//! Bitstream packet-type checks (mediabunny `verifyKeyPackets` / `determinePacketType`).

mod avc;

pub use avc::{
    PacketType, accept_avc_recovery_point_sei, avc_nal_length_size, determine_avc_packet_type,
    determine_avc_packet_type_with, sanitize_avc_packet_for_chromium,
};
