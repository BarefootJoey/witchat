//! Official BitChat constants, pinned to permissionlesstech/bitchat
//! (`BLEService.swift`, `TransportConfig.swift`, `BinaryProtocol.swift`).

use uuid::Uuid;

/// Mainnet BLE service UUID from `BLEService.swift` (release builds).
pub const SERVICE_UUID_MAINNET: Uuid =
    Uuid::from_bytes([0xF4, 0x7B, 0x5E, 0x2D, 0x4A, 0x9E, 0x4C, 0x5A, 0x9B, 0x3F, 0x8E, 0x1D, 0x2C, 0x3A, 0x4B, 0x5C]);

/// Debug/testnet BLE service UUID from `BLEService.swift`.
pub const SERVICE_UUID_TESTNET: Uuid =
    Uuid::from_bytes([0xF4, 0x7B, 0x5E, 0x2D, 0x4A, 0x9E, 0x4C, 0x5A, 0x9B, 0x3F, 0x8E, 0x1D, 0x2C, 0x3A, 0x4B, 0x5A]);

/// GATT characteristic used for all mesh frames.
pub const CHARACTERISTIC_UUID: Uuid =
    Uuid::from_bytes([0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x4A, 0x5B, 0x8C, 0x9D, 0x0E, 0x1F, 0x2A, 0x3B, 0x4C, 0x5D]);

pub const V1_HEADER_SIZE: usize = 14;
pub const V2_HEADER_SIZE: usize = 16;
pub const SENDER_ID_SIZE: usize = 8;
pub const RECIPIENT_ID_SIZE: usize = 8;
pub const SIGNATURE_SIZE: usize = 64;
pub const FLAGS_OFFSET: usize = 11;

pub const FLAG_HAS_RECIPIENT: u8 = 0x01;
pub const FLAG_HAS_SIGNATURE: u8 = 0x02;
pub const FLAG_IS_COMPRESSED: u8 = 0x04;
pub const FLAG_HAS_ROUTE: u8 = 0x08;
pub const FLAG_IS_RSR: u8 = 0x10;

/// Default mesh TTL (`TransportConfig.messageTTLDefault`).
pub const DEFAULT_TTL: u8 = 7;

/// Default fragment chunk size (`TransportConfig.bleDefaultFragmentSize`).
pub const DEFAULT_FRAGMENT_SIZE: usize = 469;

/// Compress payloads at or above this size (`Constants.compressionThresholdBytes`).
pub const COMPRESSION_THRESHOLD: usize = 100;

/// Official Noise protocol name.
pub const NOISE_PROTOCOL_NAME: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

pub const MAX_DEFLATE_RATIO: usize = 1032;
pub const MAX_NICKNAME_LEN: usize = 255;
pub const MAX_ANNOUNCE_NEIGHBORS: usize = 10;
