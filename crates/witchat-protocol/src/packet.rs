use crate::binary;
use crate::constants::{RECIPIENT_ID_SIZE, SENDER_ID_SIZE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub version: u8,
    pub type_id: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub recipient_id: Option<[u8; RECIPIENT_ID_SIZE]>,
    pub timestamp: u64,
    pub payload: Vec<u8>,
    pub signature: Option<[u8; 64]>,
    pub ttl: u8,
    pub route: Option<Vec<[u8; SENDER_ID_SIZE]>>,
    pub is_rsr: bool,
}

impl Packet {
    pub fn new(type_id: u8, sender_id: [u8; SENDER_ID_SIZE], payload: Vec<u8>) -> Self {
        Self {
            version: 1,
            type_id,
            sender_id,
            recipient_id: None,
            timestamp: now_millis(),
            payload,
            signature: None,
            ttl: crate::constants::DEFAULT_TTL,
            route: None,
            is_rsr: false,
        }
    }

    pub fn encode(&self, padding: bool) -> Option<Vec<u8>> {
        binary::encode(self, padding)
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        binary::decode(data)
    }

    /// Signing canonical form: TTL=0, no signature, is_rsr=false, padded encode.
    pub fn encode_for_signing(&self) -> Option<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.signature = None;
        unsigned.ttl = 0;
        unsigned.is_rsr = false;
        binary::encode(&unsigned, true)
    }

    pub fn with_recipient(mut self, id: [u8; RECIPIENT_ID_SIZE]) -> Self {
        self.recipient_id = Some(id);
        self
    }

    pub fn with_ttl(mut self, ttl: u8) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn decrement_ttl(&mut self) -> bool {
        if self.ttl == 0 {
            return false;
        }
        self.ttl -= 1;
        self.ttl > 0
    }
}

pub fn now_millis() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn pad_id(bytes: &[u8], size: usize) -> Vec<u8> {
    let mut out = bytes.iter().take(size).copied().collect::<Vec<_>>();
    if out.len() < size {
        out.resize(size, 0);
    }
    out
}

pub fn trim_nulls(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().rposition(|b| *b != 0).map(|i| i + 1).unwrap_or(0);
    &bytes[..end]
}
