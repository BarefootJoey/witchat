//! Images, voice notes and files: saving what arrives (validated by the core)
//! under `Downloads\witchat`, and sending a local file to `#mesh` or a DM.

use crate::app_dirs;
use crate::mesh_worker::{send, UiEvent, Worker};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use witchat_core::{ChatLine, IncomingFile};
use witchat_protocol::file_packet::{mime_for_extension, MAX_FILE_BYTES, SENDABLE_EXTENSIONS};
use witchat_protocol::MediaKind;
use witchat_protocol::{FilePacket, PeerId};

impl Worker {
    pub(crate) fn on_file(&self, file: IncomingFile) {
        if let Some(id) = file.message_id.as_deref().filter(|_| file.is_private) {
            if let Err(err) = self.ledger.commit(id) {
                tracing::warn!(%err, "file: could not record media receipt");
            }
        }
        let blocked = self.router.peer_fingerprint(&file.peer_id).is_some_and(|fp| self.blocked.contains(&fp));
        if blocked {
            tracing::info!(peer = %file.peer_id.hex, "file from a blocked peer dropped before disk write");
            return;
        }
        let id =
            file.message_id.clone().unwrap_or_else(|| format!("local-file-{}-{}", file.peer_id.hex, file.timestamp_ms));
        let mut line = ChatLine {
            id,
            from: file.from.clone(),
            peer_id: Some(file.peer_id.clone()),
            content: String::new(),
            timestamp_ms: file.timestamp_ms,
            is_self: false,
            is_private: file.is_private,
        };
        match save_incoming(&file) {
            Ok(path) => {
                line.content = caption(file.kind, &path, file.content.len());
                send(&self.evt_tx, UiEvent::File { line, path, kind: file.kind });
            }
            Err(err) => {
                line.content = format!("[{}] could not be saved: {err:#}", file.kind.label());
                send(&self.evt_tx, UiEvent::Line(line));
            }
        }
    }

    /// Broadcast a local image, voice note or PDF to `#mesh`.
    pub(crate) fn send_public_file(&self, path: &str) {
        let path = clean_path(path);
        let result = load_outgoing(&path, None).and_then(|file| {
            let packet = self.router.build_public_file(&file).context("could not encode the file")?;
            Ok((file, packet))
        });
        match result {
            Ok((file, packet)) => {
                let frames = self.router.outbound_frames(&packet).len();
                tracing::info!(bytes = file.content.len(), frames, "file: broadcasting");
                let id = format!("local-file-{}", uuid::Uuid::new_v4());
                self.out.send_file(&packet, &id);
                self.echo_file(None, id, &file, path);
            }
            Err(err) => send(&self.evt_tx, UiEvent::Error(format!("send file: {err:#}"))),
        }
    }

    /// Encrypted inside the Noise session when the peer proved private media
    /// support; the signed visible fallback only with `visible`.
    pub(crate) fn send_private_file(&mut self, peer: &PeerId, path: &str, visible: bool) {
        let path = clean_path(path);
        let file = match load_outgoing(&path, Some(ios_style_name)) {
            Ok(file) => file,
            Err(err) => return send(&self.evt_tx, UiEvent::Error(format!("send file: {err:#}"))),
        };
        if !self.router.sessions.is_ready(peer) {
            if let Some(packet) = self.router.ensure_session(peer) {
                self.out.send(&packet);
            }
            return send(
                &self.evt_tx,
                UiEvent::Error("securing the chat first; send the file again in a moment".into()),
            );
        }
        match self.router.build_private_file(peer, &file, visible) {
            Ok(sent) => {
                let frames = self.router.outbound_frames(&sent.packet).len();
                tracing::info!(peer = %peer.hex, bytes = file.content.len(), frames, encrypted = sent.encrypted, "file: sending privately");
                let id = sent.message_id.unwrap_or_else(|| format!("local-file-{}", uuid::Uuid::new_v4()));
                self.out.send_file(&sent.packet, &id);
                self.echo_file(Some(peer.clone()), id, &file, path);
                if !sent.encrypted {
                    send(&self.evt_tx, UiEvent::Status("file sent signed but NOT end-to-end encrypted".into()));
                }
            }
            Err(_) => send(
                &self.evt_tx,
                UiEvent::Error(
                    "this peer has not proven encrypted private media; /sendvisible <path> sends it signed but readable by relays"
                        .into(),
                ),
            ),
        }
    }

    fn echo_file(&self, peer: Option<PeerId>, id: String, file: &FilePacket, path: PathBuf) {
        let (kind, _) = file.validate().expect("validated on load");
        let line = ChatLine {
            id,
            from: self.router.identity.nickname.clone(),
            is_private: peer.is_some(),
            peer_id: peer.or_else(|| Some(self.router.local_peer_id())),
            content: caption(kind, &path, file.content.len()),
            timestamp_ms: witchat_protocol::packet::now_millis(),
            is_self: true,
        };
        send(&self.evt_tx, UiEvent::File { line, path, kind });
    }
}

fn clean_path(path: &str) -> PathBuf {
    PathBuf::from(path.trim().trim_matches('"'))
}

/// Photos and voice notes are shown, not named (iOS hides their generated
/// names); documents keep the name so the user knows what they are opening.
fn caption(kind: MediaKind, path: &Path, bytes: usize) -> String {
    let size = human_size(bytes);
    match kind {
        MediaKind::File => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            format!("[{}] {name} · {size}", kind.label())
        }
        _ => format!("[{}] {size}", kind.label()),
    }
}

/// iOS names its photos `img_<date>_<time>_<UUID>.jpg`; matching it gives the
/// receiver a stable id, so delivered/read receipts work for our sends too.
fn ios_style_name(ext: &str) -> Option<String> {
    let ext = ext.to_ascii_lowercase();
    if ext != "jpg" && ext != "jpeg" {
        return None;
    }
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    Some(format!("img_{stamp}_{}.jpg", uuid::Uuid::new_v4().to_string().to_uppercase()))
}

fn load_outgoing(path: &Path, rename: Option<fn(&str) -> Option<String>>) -> Result<FilePacket> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    let Some(mime) = mime_for_extension(ext) else {
        bail!("only {} files can be sent", SENDABLE_EXTENSIONS.join(", "));
    };
    let size = std::fs::metadata(path).with_context(|| format!("{}", path.display()))?.len();
    if size as usize > MAX_FILE_BYTES {
        bail!("{} is over the 1 MiB BitChat limit", human_size(size as usize));
    }
    let original = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let file = FilePacket {
        name: rename.and_then(|r| r(ext)).or(original),
        mime: Some(mime.into()),
        content: std::fs::read(path).with_context(|| format!("{}", path.display()))?,
    };
    if file.validate().is_none() {
        bail!("the file contents do not look like {mime}");
    }
    Ok(file)
}

fn save_incoming(file: &IncomingFile) -> Result<PathBuf> {
    let dir = media_dir().context("no Downloads or data folder")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("{}", dir.display()))?;
    let stem = safe_stem(file.name.as_deref(), file.kind.label(), file.timestamp_ms);
    for n in 0..1000 {
        let name = if n == 0 { format!("{stem}.{}", file.ext) } else { format!("{stem}-{n}.{}", file.ext) };
        let path = dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                std::io::Write::write_all(&mut f, &file.content)?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err).with_context(|| format!("{}", path.display())),
        }
    }
    bail!("too many files named {stem}")
}

/// Where received and recorded media live (`Downloads\witchat`).
pub(crate) fn media_dir() -> Option<PathBuf> {
    app_dirs::download_dir().map(|p| p.join("witchat")).or_else(|| app_dirs::data_dir().map(|d| d.join("media")))
}

/// A file stem safe on Windows: leaf name only, no extension (the validated
/// MIME decides that), restricted characters, bounded length.
fn safe_stem(name: Option<&str>, kind: &str, timestamp_ms: u64) -> String {
    let leaf = name.unwrap_or_default().rsplit(['/', '\\']).next().unwrap_or_default();
    let stem = leaf.rsplit_once('.').map(|(s, _)| s).unwrap_or(leaf);
    let clean: String = stem.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).take(64).collect();
    let reserved = ["con", "prn", "aux", "nul", "com1", "lpt1"].contains(&clean.to_ascii_lowercase().as_str());
    if clean.is_empty() || reserved {
        format!("{kind}_{timestamp_ms}")
    } else {
        clean
    }
}

fn human_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_cannot_escape_or_pick_extensions() {
        assert_eq!(safe_stem(Some("..\\..\\evil.exe"), "file", 1), "evil");
        assert_eq!(safe_stem(Some("/tmp/img_ABC-1.jpg"), "image", 1), "img_ABC-1");
        assert_eq!(safe_stem(Some("CON.png"), "image", 7), "image_7");
        assert_eq!(safe_stem(None, "voice", 9), "voice_9");
        assert_eq!(safe_stem(Some("ünï cødé.png"), "image", 1), "ncd");
    }

    #[test]
    fn captions_hide_generated_media_names() {
        let dir = Path::new("d");
        let photo = dir.join("img_20000101_000000_AAAAAAAA.jpg");
        assert_eq!(caption(MediaKind::Image, &photo, 38 * 1024), "[image] 38 KB");
        let pdf = dir.join("menu.pdf");
        assert_eq!(caption(MediaKind::File, &pdf, 2048), "[file] menu.pdf · 2 KB");
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(900), "900 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn private_jpegs_get_receipt_eligible_names() {
        let name = ios_style_name("JPG").unwrap();
        let a = PeerId::from_hex("0011223344556677");
        let b = PeerId::from_hex("aabbccddeeff0011");
        assert!(witchat_core::files::stable_media_id(&a, &b, Some(&name)).is_some(), "{name}");
        assert!(ios_style_name("png").is_none());
    }
}
