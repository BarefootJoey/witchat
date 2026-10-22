//! Wire types from `BitFoundation/MessageType.swift`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageType {
    Announce = 0x01,
    Message = 0x02,
    Leave = 0x03,
    CourierEnvelope = 0x04,
    NoiseHandshake = 0x10,
    NoiseEncrypted = 0x11,
    Fragment = 0x20,
    RequestSync = 0x21,
    FileTransfer = 0x22,
    BoardPost = 0x23,
    PrekeyBundle = 0x24,
    GroupMessage = 0x25,
    Ping = 0x26,
    Pong = 0x27,
    NostrCarrier = 0x28,
    VoiceFrame = 0x29,
    AnnounceV2 = 0x2C,
}

impl MessageType {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x01 => Self::Announce,
            0x02 => Self::Message,
            0x03 => Self::Leave,
            0x04 => Self::CourierEnvelope,
            0x10 => Self::NoiseHandshake,
            0x11 => Self::NoiseEncrypted,
            0x20 => Self::Fragment,
            0x21 => Self::RequestSync,
            0x22 => Self::FileTransfer,
            0x23 => Self::BoardPost,
            0x24 => Self::PrekeyBundle,
            0x25 => Self::GroupMessage,
            0x26 => Self::Ping,
            0x27 => Self::Pong,
            0x28 => Self::NostrCarrier,
            0x29 => Self::VoiceFrame,
            0x2C => Self::AnnounceV2,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Announce => "announce",
            Self::AnnounceV2 => "announceV2",
            Self::Message => "message",
            Self::Leave => "leave",
            Self::CourierEnvelope => "courierEnvelope",
            Self::RequestSync => "requestSync",
            Self::NoiseHandshake => "noiseHandshake",
            Self::NoiseEncrypted => "noiseEncrypted",
            Self::Fragment => "fragment",
            Self::FileTransfer => "fileTransfer",
            Self::BoardPost => "boardPost",
            Self::PrekeyBundle => "prekeyBundle",
            Self::GroupMessage => "groupMessage",
            Self::Ping => "ping",
            Self::Pong => "pong",
            Self::NostrCarrier => "nostrCarrier",
            Self::VoiceFrame => "voiceFrame",
        }
    }
}

/// First byte of a decrypted Noise payload (`BitchatProtocol.swift`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NoisePayloadType {
    PrivateMessage = 0x01,
    ReadReceipt = 0x02,
    Delivered = 0x03,
    GroupInvite = 0x06,
    GroupKeyUpdate = 0x07,
    VoiceFrame = 0x08,
    VerifyChallenge = 0x10,
    VerifyResponse = 0x11,
    Vouch = 0x12,
    PrivateFile = 0x20,
    AuthenticatedPeerState = 0x21,
}

impl NoisePayloadType {
    pub fn from_u8(value: u8) -> Option<Self> {
        // 0x09 was a prerelease private-file alias.
        if value == 0x09 {
            return Some(Self::PrivateFile);
        }
        Some(match value {
            0x01 => Self::PrivateMessage,
            0x02 => Self::ReadReceipt,
            0x03 => Self::Delivered,
            0x06 => Self::GroupInvite,
            0x07 => Self::GroupKeyUpdate,
            0x08 => Self::VoiceFrame,
            0x10 => Self::VerifyChallenge,
            0x11 => Self::VerifyResponse,
            0x12 => Self::Vouch,
            0x20 => Self::PrivateFile,
            0x21 => Self::AuthenticatedPeerState,
            _ => return None,
        })
    }
}
