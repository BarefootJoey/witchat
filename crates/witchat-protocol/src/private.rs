//! Private-message TLV from `PrivateMessagePacket`.

const TLV_MESSAGE_ID: u8 = 0x00;
const TLV_CONTENT: u8 = 0x01;
/// One-byte TLV length; official clients reject longer DM bodies.
pub const MAX_PRIVATE_CONTENT_BYTES: usize = 255;

/// Split DM text into official-size chunks on UTF-8 boundaries, preferring
/// to break after whitespace so words stay whole.
pub fn split_private_content(content: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut rest = content;
    while rest.len() > MAX_PRIVATE_CONTENT_BYTES {
        let mut cut = MAX_PRIVATE_CONTENT_BYTES;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        if let Some(space) = rest[..cut].rfind(char::is_whitespace) {
            if space > 0 {
                cut = space + rest[space..].chars().next().map_or(1, char::len_utf8);
            }
        }
        chunks.push(rest[..cut].trim_end().to_string());
        rest = &rest[cut..];
    }
    let tail = rest.trim_end();
    if !tail.is_empty() || chunks.is_empty() {
        chunks.push(tail.to_string());
    }
    chunks
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateMessage {
    pub message_id: String,
    pub content: String,
}

impl PrivateMessage {
    pub fn encode(&self) -> Option<Vec<u8>> {
        let id = self.message_id.as_bytes();
        let content = self.content.as_bytes();
        if id.len() > 255 || content.len() > 255 {
            return None;
        }
        let mut data = Vec::new();
        data.push(TLV_MESSAGE_ID);
        data.push(id.len() as u8);
        data.extend_from_slice(id);
        data.push(TLV_CONTENT);
        data.push(content.len() as u8);
        data.extend_from_slice(content);
        Some(data)
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut offset = 0;
        let mut message_id = None;
        let mut content = None;
        while offset + 2 <= data.len() {
            let typ = data[offset];
            let length = data[offset + 1] as usize;
            offset += 2;
            if offset + length > data.len() {
                return None;
            }
            let value = &data[offset..offset + length];
            offset += length;
            match typ {
                TLV_MESSAGE_ID => message_id = String::from_utf8(value.to_vec()).ok(),
                TLV_CONTENT => content = String::from_utf8(value.to_vec()).ok(),
                _ => return None,
            }
        }
        Some(Self { message_id: message_id?, content: content? })
    }
}
