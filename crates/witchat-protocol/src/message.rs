//! Public chat payload from `BitchatMessage.toBinaryPayload()`.

use crate::packet::now_millis;
use uuid::Uuid;

const FLAG_RELAY: u8 = 0x01;
const FLAG_PRIVATE: u8 = 0x02;
const FLAG_ORIGINAL_SENDER: u8 = 0x04;
const FLAG_RECIPIENT_NICK: u8 = 0x08;
const FLAG_SENDER_PEER: u8 = 0x10;
const FLAG_MENTIONS: u8 = 0x20;
const FLAG_BRIDGED: u8 = 0x40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: String,
    pub sender: String,
    pub content: String,
    pub timestamp_ms: u64,
    pub is_relay: bool,
    pub original_sender: Option<String>,
    pub is_private: bool,
    pub recipient_nickname: Option<String>,
    pub sender_peer_id: Option<String>,
    pub mentions: Option<Vec<String>>,
    pub is_bridged: bool,
}

impl ChatMessage {
    pub fn public(sender: impl Into<String>, content: impl Into<String>, sender_peer_id: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            sender: sender.into(),
            content: content.into(),
            timestamp_ms: now_millis(),
            is_relay: false,
            original_sender: None,
            is_private: false,
            recipient_nickname: None,
            sender_peer_id,
            mentions: None,
            is_bridged: false,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut flags = 0u8;
        if self.is_relay {
            flags |= FLAG_RELAY;
        }
        if self.is_private {
            flags |= FLAG_PRIVATE;
        }
        if self.original_sender.is_some() {
            flags |= FLAG_ORIGINAL_SENDER;
        }
        if self.recipient_nickname.is_some() {
            flags |= FLAG_RECIPIENT_NICK;
        }
        if self.sender_peer_id.is_some() {
            flags |= FLAG_SENDER_PEER;
        }
        if self.mentions.as_ref().is_some_and(|m| !m.is_empty()) {
            flags |= FLAG_MENTIONS;
        }
        if self.is_bridged {
            flags |= FLAG_BRIDGED;
        }

        let mut data = vec![flags];
        data.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        push_len_str(&mut data, &self.id);
        push_len_str(&mut data, &self.sender);
        let content = self.content.as_bytes();
        let len = (content.len().min(65535) as u16).to_be_bytes();
        data.extend_from_slice(&len);
        data.extend_from_slice(&content[..content.len().min(65535)]);

        if let Some(orig) = &self.original_sender {
            push_len_str(&mut data, orig);
        }
        if let Some(recip) = &self.recipient_nickname {
            push_len_str(&mut data, recip);
        }
        if let Some(peer) = &self.sender_peer_id {
            push_len_str(&mut data, peer);
        }
        if let Some(mentions) = &self.mentions {
            data.push(mentions.len().min(255) as u8);
            for mention in mentions.iter().take(255) {
                push_len_str(&mut data, mention);
            }
        }
        data
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < 13 {
            return None;
        }
        let mut offset = 0;
        let flags = data[offset];
        offset += 1;
        let is_relay = flags & FLAG_RELAY != 0;
        let is_private = flags & FLAG_PRIVATE != 0;
        let has_original = flags & FLAG_ORIGINAL_SENDER != 0;
        let has_recip = flags & FLAG_RECIPIENT_NICK != 0;
        let has_peer = flags & FLAG_SENDER_PEER != 0;
        let has_mentions = flags & FLAG_MENTIONS != 0;
        let is_bridged = flags & FLAG_BRIDGED != 0;

        if offset + 8 > data.len() {
            return None;
        }
        let mut ts = [0u8; 8];
        ts.copy_from_slice(&data[offset..offset + 8]);
        let timestamp_ms = u64::from_be_bytes(ts);
        offset += 8;

        let id = read_len_str(data, &mut offset)?;
        let sender = read_len_str(data, &mut offset)?;
        if offset + 2 > data.len() {
            return None;
        }
        let content_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + content_len > data.len() {
            return None;
        }
        let content = String::from_utf8_lossy(&data[offset..offset + content_len]).into_owned();
        offset += content_len;

        let original_sender = if has_original { read_len_str(data, &mut offset) } else { None };
        let recipient_nickname = if has_recip { read_len_str(data, &mut offset) } else { None };
        let sender_peer_id = if has_peer { read_len_str(data, &mut offset) } else { None };
        let mentions = if has_mentions && offset < data.len() {
            let count = data[offset] as usize;
            offset += 1;
            let mut list = Vec::new();
            for _ in 0..count {
                if let Some(m) = read_len_str(data, &mut offset) {
                    list.push(m);
                }
            }
            Some(list)
        } else {
            None
        };

        Some(Self {
            id,
            sender,
            content,
            timestamp_ms,
            is_relay,
            original_sender,
            is_private,
            recipient_nickname,
            sender_peer_id,
            mentions,
            is_bridged,
        })
    }
}

fn push_len_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(255);
    out.push(n as u8);
    out.extend_from_slice(&bytes[..n]);
}

fn read_len_str(data: &[u8], offset: &mut usize) -> Option<String> {
    if *offset >= data.len() {
        return None;
    }
    let len = data[*offset] as usize;
    *offset += 1;
    if *offset + len > data.len() {
        return None;
    }
    let s = String::from_utf8_lossy(&data[*offset..*offset + len]).into_owned();
    *offset += len;
    Some(s)
}
