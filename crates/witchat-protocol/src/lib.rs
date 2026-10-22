//! BitChat-compatible wire protocol.
//!
//! Implemented from the official iOS/macOS Unlicense sources
//! (`permissionlesstech/bitchat`, BitFoundation + Protocols).

pub mod announce;
pub mod binary;
pub mod capabilities;
pub mod compression;
pub mod constants;
pub mod courier;
pub mod file_packet;
pub mod fragment;
pub mod gcs;
pub mod hexutil;
pub mod message;
pub mod message_type;
pub mod notify_stream;
pub mod packet;
pub mod padding;
pub mod payload_limits;
pub mod peer_id;
pub mod peer_state;
pub mod prekey_bundle;
pub mod private;
pub mod request_sync;
pub mod verify;
pub mod voice_burst;

pub use announce::Announcement;
pub use capabilities::PeerCapabilities;
pub use constants::*;
pub use courier::CourierEnvelope;
pub use file_packet::{FilePacket, MediaKind};
pub use fragment::{split_packet, Fragment, FragmentAssembler};
pub use message::ChatMessage;
pub use message_type::{MessageType, NoisePayloadType};
pub use notify_stream::NotificationStreamAssembler;
pub use packet::Packet;
pub use peer_id::{fingerprint_hex, format_fingerprint, PeerId};
pub use peer_state::AuthenticatedPeerState;
pub use prekey_bundle::{Prekey, PrekeyBundle};
pub use private::{split_private_content, PrivateMessage};
pub use request_sync::RequestSync;
pub use verify::VerificationQr;
pub use voice_burst::{BurstKind, VoiceBurst};
