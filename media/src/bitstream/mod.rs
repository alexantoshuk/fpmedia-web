//! Bitstream packet-type checks (mediabunny `verifyKeyPackets` / `determinePacketType`).

mod avc;

pub use avc::{
    PacketType, avc_nal_length_size, determine_avc_packet_type, sanitize_avc_packet_for_chromium,
};
