//! File transfer payload (`MessageType::FileTransfer`, and the body of
//! `NoisePayloadType::PrivateFile`): TLVs 0x01 name, 0x02 size (4 bytes),
//! 0x03 MIME, 0x04 content (4-byte length). Mirrors iOS `BitchatFilePacket`
//! plus its MIME allow-list and magic-byte checks.

/// iOS `FileTransferLimits.maxPayloadBytes`.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;

const TAG_NAME: u8 = 0x01;
const TAG_SIZE: u8 = 0x02;
const TAG_MIME: u8 = 0x03;
const TAG_CONTENT: u8 = 0x04;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePacket {
    pub name: Option<String>,
    pub mime: Option<String>,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Audio,
    File,
}

impl MediaKind {
    /// Chat-line prefix iOS uses for media (`[image] `, `[voice] `, `[file] `).
    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Audio => "voice",
            Self::File => "file",
        }
    }
}

impl FilePacket {
    pub fn encode(&self) -> Option<Vec<u8>> {
        if self.content.is_empty() || self.content.len() > MAX_FILE_BYTES {
            return None;
        }
        let mut out = Vec::with_capacity(self.content.len() + 64);
        if let Some(name) = self.name.as_deref().filter(|n| n.len() <= u16::MAX as usize) {
            out.push(TAG_NAME);
            out.extend_from_slice(&(name.len() as u16).to_be_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        out.push(TAG_SIZE);
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&(self.content.len() as u32).to_be_bytes());
        if let Some(mime) = self.mime.as_deref().filter(|m| m.len() <= u16::MAX as usize) {
            out.push(TAG_MIME);
            out.extend_from_slice(&(mime.len() as u16).to_be_bytes());
            out.extend_from_slice(mime.as_bytes());
        }
        out.push(TAG_CONTENT);
        out.extend_from_slice(&(self.content.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.content);
        Some(out)
    }

    /// Tolerates the legacy 8-byte size and 2-byte content length, like iOS.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut cur = 0usize;
        let (mut name, mut mime, mut content) = (None, None, Vec::new());
        let read_len = |cur: &mut usize, bytes: usize| -> Option<usize> {
            let slice = data.get(*cur..*cur + bytes)?;
            *cur += bytes;
            Some(slice.iter().fold(0usize, |acc, b| (acc << 8) | *b as usize))
        };
        while cur < data.len() {
            let tag = data[cur];
            cur += 1;
            let len = if tag == TAG_CONTENT {
                let snapshot = cur;
                match read_len(&mut cur, 4) {
                    Some(n) if n <= data.len() - cur => n,
                    _ => {
                        cur = snapshot;
                        read_len(&mut cur, 2)?
                    }
                }
            } else {
                read_len(&mut cur, 2)?
            };
            let value = data.get(cur..cur.checked_add(len)?)?;
            cur += len;
            match tag {
                TAG_NAME => name = String::from_utf8(value.to_vec()).ok(),
                TAG_SIZE => {
                    if (len == 4 || len == 8)
                        && value.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64) > MAX_FILE_BYTES as u64
                    {
                        return None;
                    }
                }
                TAG_MIME => mime = String::from_utf8(value.to_vec()).ok(),
                TAG_CONTENT => {
                    if content.len() + value.len() > MAX_FILE_BYTES {
                        return None;
                    }
                    content.extend_from_slice(value);
                }
                _ => {}
            }
        }
        if content.is_empty() {
            return None;
        }
        Some(Self { name, mime, content })
    }

    /// Accept only allow-listed MIME types whose bytes look like that type.
    /// Returns the kind and the canonical file extension.
    pub fn validate(&self) -> Option<(MediaKind, &'static str)> {
        let (kind, ext) = mime_info(self.mime.as_deref()?)?;
        magic_matches(ext, &self.content).then_some((kind, ext))
    }
}

/// Every extension `mime_for_extension` accepts, for file pickers and hints.
pub const SENDABLE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "m4a", "mp3", "wav", "ogg", "pdf"];

/// MIME type for a file extension we are willing to send.
pub fn mime_for_extension(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "m4a" => "audio/m4a",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "pdf" => "application/pdf",
        _ => return None,
    })
}

fn mime_info(mime: &str) -> Option<(MediaKind, &'static str)> {
    Some(match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => (MediaKind::Image, "jpg"),
        "image/png" => (MediaKind::Image, "png"),
        "image/gif" => (MediaKind::Image, "gif"),
        "image/webp" => (MediaKind::Image, "webp"),
        "audio/mp4" | "audio/m4a" | "audio/aac" | "audio/x-m4a" => (MediaKind::Audio, "m4a"),
        "audio/mpeg" | "audio/mp3" => (MediaKind::Audio, "mp3"),
        "audio/wav" | "audio/x-wav" => (MediaKind::Audio, "wav"),
        "audio/ogg" => (MediaKind::Audio, "ogg"),
        "application/pdf" => (MediaKind::File, "pdf"),
        "application/octet-stream" => (MediaKind::File, "bin"),
        _ => return None,
    })
}

fn magic_matches(ext: &str, d: &[u8]) -> bool {
    let starts = |p: &[u8]| d.starts_with(p);
    let riff = |tag: &[u8]| d.len() >= 12 && starts(b"RIFF") && &d[8..12] == tag;
    match ext {
        "jpg" => starts(&[0xFF, 0xD8, 0xFF]),
        "png" => starts(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "gif" => starts(b"GIF87a") || starts(b"GIF89a"),
        "webp" => riff(b"WEBP"),
        "wav" => riff(b"WAVE"),
        "m4a" => d.len() > 100,
        "mp3" => starts(b"ID3") || (d.len() >= 2 && d[0] == 0xFF && d[1] & 0xE0 == 0xE0),
        "ogg" => starts(b"OggS"),
        "pdf" => starts(b"%PDF"),
        "bin" => !d.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> FilePacket {
        let mut content = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        content.extend_from_slice(&[0u8; 300]);
        FilePacket { name: Some("img_1.png".into()), mime: Some("image/png".into()), content }
    }

    #[test]
    fn roundtrips_and_validates() {
        let file = png();
        let decoded = FilePacket::decode(&file.encode().unwrap()).unwrap();
        assert_eq!(decoded, file);
        assert_eq!(decoded.validate(), Some((MediaKind::Image, "png")));
    }

    #[test]
    fn sendable_extensions_all_have_a_mime() {
        for ext in SENDABLE_EXTENSIONS {
            assert!(mime_for_extension(ext).is_some(), "{ext}");
        }
        assert!(mime_for_extension("exe").is_none());
    }

    #[test]
    fn rejects_mismatched_magic_and_unknown_mime() {
        let mut file = png();
        file.mime = Some("image/jpeg".into());
        assert!(file.validate().is_none());
        file.mime = Some("application/x-msdownload".into());
        assert!(file.validate().is_none());
        file.mime = None;
        assert!(file.validate().is_none());
    }

    #[test]
    fn accepts_legacy_two_byte_content_length() {
        let mut data = vec![TAG_MIME, 0, 8];
        data.extend_from_slice(b"text/raw");
        data.extend_from_slice(&[TAG_CONTENT, 0, 3, b'a', b'b', b'c']);
        let file = FilePacket::decode(&data).unwrap();
        assert_eq!(file.content, b"abc");
    }

    #[test]
    fn rejects_oversize_and_truncated() {
        let mut big = png();
        big.content = vec![0; MAX_FILE_BYTES + 1];
        assert!(big.encode().is_none());
        assert!(FilePacket::decode(&[TAG_NAME, 0, 9, b'x']).is_none(), "value shorter than its length");
        assert!(FilePacket::decode(&[TAG_SIZE, 0, 4, 0, 0x20, 0, 0]).is_none(), "declared size over 1 MiB");
        assert!(FilePacket::decode(&[]).is_none());
    }
}
