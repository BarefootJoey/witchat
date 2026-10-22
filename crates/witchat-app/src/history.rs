//! Opt-in saved history (`/history on`): `#mesh` and location-channel lines
//! survive a restart. Stored as plaintext JSON next to `social.json`.
//! Private messages are never written: they are end-to-end encrypted on the
//! wire and stay in memory only, as on iOS.

use crate::app_dirs;
use crate::inbox::{Conversation, Entry, Media};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use witchat_core::ChatLine;
use witchat_protocol::{MediaKind, PeerId};

/// Newest lines kept per channel.
pub const PER_CHANNEL: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedLine {
    /// `"mesh"`, or the geohash of a location channel.
    pub channel: String,
    pub id: String,
    pub from: String,
    #[serde(default)]
    pub peer: Option<String>,
    pub content: String,
    pub ts: u64,
    pub is_self: bool,
    #[serde(default)]
    pub media: Option<SavedMedia>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedMedia {
    pub path: PathBuf,
    /// `MediaKind::label`: `image`, `voice` or `file`.
    pub kind: String,
}

const MESH: &str = "mesh";

pub fn default_path() -> Option<PathBuf> {
    app_dirs::data_dir().map(|d| d.join("history.json"))
}

/// Channel lines worth keeping, oldest first, capped per channel.
pub fn snapshot<'a>(entries: impl Iterator<Item = &'a Entry>) -> Vec<SavedLine> {
    let mut lines: Vec<SavedLine> = entries.filter(|e| !e.system).filter_map(to_saved).collect();
    let mut kept: HashMap<String, usize> = HashMap::new();
    lines.reverse();
    lines.retain(|l| {
        let n = kept.entry(l.channel.clone()).or_default();
        *n += 1;
        *n <= PER_CHANNEL
    });
    lines.reverse();
    lines
}

fn to_saved(entry: &Entry) -> Option<SavedLine> {
    let channel = match &entry.conv {
        Conversation::Mesh => MESH.to_string(),
        Conversation::Geo(cell) => cell.clone(),
        Conversation::Dm(_) | Conversation::GeoDm { .. } => return None,
    };
    let line = &entry.line;
    Some(SavedLine {
        channel,
        id: line.id.clone(),
        from: line.from.clone(),
        peer: line.peer_id.as_ref().map(|p| p.hex.clone()),
        content: line.content.clone(),
        ts: line.timestamp_ms,
        is_self: line.is_self,
        media: entry.media.as_ref().map(|m| SavedMedia { path: m.path.clone(), kind: m.kind.label().into() }),
    })
}

/// Entries to show again. Lines whose saved file was deleted are dropped.
pub fn restore(saved: Vec<SavedLine>) -> Vec<Entry> {
    saved
        .into_iter()
        .filter_map(|s| {
            let media = match s.media {
                Some(m) => {
                    let kind = kind_from_label(&m.kind)?;
                    if !m.path.exists() {
                        return None;
                    }
                    Some(Media::load(m.path, kind))
                }
                None => None,
            };
            let conv = if s.channel == MESH { Conversation::Mesh } else { Conversation::Geo(s.channel) };
            let line = ChatLine {
                id: s.id,
                from: s.from,
                peer_id: s.peer.map(PeerId::from_hex),
                content: s.content,
                timestamp_ms: s.ts,
                is_self: s.is_self,
                is_private: false,
            };
            Some(Entry { conv, line, system: false, media })
        })
        .collect()
}

fn kind_from_label(label: &str) -> Option<MediaKind> {
    [MediaKind::Image, MediaKind::Audio, MediaKind::File].into_iter().find(|k| k.label() == label)
}

pub fn load(path: &Path) -> Result<Vec<SavedLine>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let json = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&json).with_context(|| format!("parse {}", path.display()))
}

pub fn save(path: &Path, lines: &[SavedLine]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string(lines)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn delete(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("delete {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(conv: Conversation, id: &str, system: bool) -> Entry {
        Entry {
            conv,
            line: ChatLine {
                id: id.into(),
                from: "bob".into(),
                peer_id: Some(PeerId::from_hex("aabbccddeeff0011")),
                content: format!("hi {id}"),
                timestamp_ms: 7,
                is_self: false,
                is_private: false,
            },
            system,
            media: None,
        }
    }

    #[test]
    fn keeps_channels_skips_dms_and_notices() {
        let entries = [
            entry(Conversation::Mesh, "m1", false),
            entry(Conversation::Mesh, "", true),
            entry(Conversation::Dm("aa".into()), "d1", false),
            entry(Conversation::Geo("9q8yy".into()), "g1", false),
        ];
        let saved = snapshot(entries.iter());
        assert_eq!(saved.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["m1", "g1"]);

        let dir = std::env::temp_dir().join(format!("witchat-history-{}", std::process::id()));
        let path = dir.join("history.json");
        save(&path, &saved).unwrap();
        let back = restore(load(&path).unwrap());
        assert_eq!(back.len(), 2);
        assert_eq!(back[1].conv, Conversation::Geo("9q8yy".into()));
        assert_eq!(back[0].line.peer_id.as_ref().unwrap().hex, "aabbccddeeff0011");
        delete(&path).unwrap();
        assert!(load(&path).unwrap().is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn caps_each_channel_and_drops_missing_files() {
        let mut entries: Vec<Entry> =
            (0..PER_CHANNEL + 5).map(|i| entry(Conversation::Mesh, &i.to_string(), false)).collect();
        entries.push(entry(Conversation::Geo("u4pru".into()), "g", false));
        let saved = snapshot(entries.iter());
        assert_eq!(saved.len(), PER_CHANNEL + 1);
        assert_eq!(saved[0].id, "5", "oldest lines go first");

        let mut gone = saved[0].clone();
        gone.media = Some(SavedMedia { path: "Z:/nope/missing.jpg".into(), kind: "image".into() });
        assert!(restore(vec![gone]).is_empty());
    }
}
