use crate::constants::{RECIPIENT_ID_SIZE, SENDER_ID_SIZE, SIGNATURE_SIZE, V2_HEADER_SIZE};
use crate::message_type::MessageType;

const FRAGMENT_BYTES: usize = 1_280;
const CONTROL_BYTES: usize = 4 * 1024;
const REQUEST_SYNC_BYTES: usize = 8 * 1024;
const ENVELOPE_BYTES: usize = 64 * 1024;
const TWICE_V1_FRAME_BYTES: usize = 128 * 1024;

/// `FileTransferLimits.maxFramedFileBytes` — 1 MiB file + TLV + v2 envelope.
pub const MAX_FRAMED_FILE_BYTES: usize = {
    let max_metadata = 65535 * 2;
    let tlv_overhead = 18 + max_metadata;
    let binary_overhead = V2_HEADER_SIZE + SENDER_ID_SIZE + RECIPIENT_ID_SIZE + SIGNATURE_SIZE;
    1024 * 1024 + tlv_overhead + binary_overhead
};

pub fn max_payload_bytes(type_id: u8) -> usize {
    match MessageType::from_u8(type_id) {
        Some(MessageType::FileTransfer | MessageType::NoiseEncrypted) => MAX_FRAMED_FILE_BYTES,
        Some(MessageType::Message | MessageType::GroupMessage) => TWICE_V1_FRAME_BYTES,
        Some(MessageType::CourierEnvelope | MessageType::NostrCarrier) => ENVELOPE_BYTES,
        Some(MessageType::RequestSync) => REQUEST_SYNC_BYTES,
        Some(MessageType::Fragment) => FRAGMENT_BYTES,
        Some(
            MessageType::Announce
            | MessageType::AnnounceV2
            | MessageType::Leave
            | MessageType::BoardPost
            | MessageType::PrekeyBundle
            | MessageType::VoiceFrame
            | MessageType::NoiseHandshake
            | MessageType::Ping
            | MessageType::Pong,
        ) => CONTROL_BYTES,
        None => TWICE_V1_FRAME_BYTES,
    }
}

pub fn max_frame_bytes(type_id: u8) -> usize {
    let overhead =
        V2_HEADER_SIZE + SENDER_ID_SIZE + RECIPIENT_ID_SIZE + 1 + 255 * SENDER_ID_SIZE + 4 + SIGNATURE_SIZE + 255;
    max_payload_bytes(type_id) + overhead
}
