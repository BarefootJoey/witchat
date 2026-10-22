//! In-memory conversation state for the GUI: message lines, unread counts,
//! DM delivery state, and which received DMs still need a read receipt.

use crate::mesh_worker::Delivery;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use witchat_core::ChatLine;
use witchat_protocol::{MediaKind, PeerId};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Conversation {
    Mesh,
    Dm(String),
    /// A location channel, by geohash.
    Geo(String),
    /// A private chat between our key in `cell` and someone's (iOS GeoDM).
    GeoDm {
        cell: String,
        pubkey: String,
    },
}

impl Conversation {
    /// Mesh or DM, from the line itself. Location lines are filed explicitly.
    pub fn of(line: &ChatLine) -> Self {
        match (&line.peer_id, line.is_private) {
            (Some(peer), true) => Self::Dm(peer.hex.clone()),
            _ => Self::Mesh,
        }
    }
}

/// A file saved on disk that a line refers to.
#[derive(Debug, Clone)]
pub struct Media {
    pub path: PathBuf,
    pub kind: MediaKind,
    /// Voice notes: length shown on the play button.
    pub duration_ms: Option<u64>,
}

impl Media {
    /// Reads voice notes once to learn their length (files are ≤ 1 MiB).
    pub fn load(path: PathBuf, kind: MediaKind) -> Self {
        let duration_ms = match kind {
            MediaKind::Audio => std::fs::read(&path).ok().and_then(|b| crate::voice::m4a_duration_ms(&b)),
            _ => None,
        };
        Self { path, kind, duration_ms }
    }
}

/// A chat line, or a gray `* nick connected`-style notice.
#[derive(Debug, Clone)]
pub struct Entry {
    pub conv: Conversation,
    pub line: ChatLine,
    pub system: bool,
    pub media: Option<Media>,
}

#[derive(Default)]
pub struct Inbox {
    lines: Vec<Entry>,
    unread: HashMap<Conversation, usize>,
    delivery: HashMap<String, Delivery>,
    /// Percent of an outgoing file handed to the radio, by line id.
    sending: HashMap<String, u8>,
    read_sent: HashSet<String>,
    /// Bumped whenever lines change, so saved history knows when to write.
    revision: u64,
}

impl Inbox {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.lines.iter()
    }

    /// Put saved lines ahead of this session's; they never count as unread.
    pub fn restore(&mut self, mut saved: Vec<Entry>) {
        saved.append(&mut self.lines);
        self.lines = saved;
    }

    /// Store a mesh or DM line. Returns true when it is an incoming message
    /// the user has not seen yet (it landed outside the selected conversation).
    pub fn push(&mut self, line: ChatLine, selected: &Conversation) -> bool {
        let conv = Conversation::of(&line);
        self.push_in(conv, line, selected)
    }

    pub fn push_in(&mut self, conv: Conversation, line: ChatLine, selected: &Conversation) -> bool {
        self.push_entry(conv, line, None, selected)
    }

    /// A mesh or DM line with an attached file.
    pub fn push_media(&mut self, line: ChatLine, media: Media, selected: &Conversation) -> bool {
        let conv = Conversation::of(&line);
        self.push_entry(conv, line, Some(media), selected)
    }

    fn push_entry(
        &mut self,
        conv: Conversation,
        line: ChatLine,
        media: Option<Media>,
        selected: &Conversation,
    ) -> bool {
        let unseen = !line.is_self && conv != *selected;
        if unseen {
            *self.unread.entry(conv.clone()).or_default() += 1;
        }
        let incoming = !line.is_self;
        self.revision += 1;
        self.lines.push(Entry { conv, line, system: false, media });
        incoming && unseen
    }

    /// A notice worth attention: like `push_system`, but unread unless `conv`
    /// is open. Returns true when it was unseen.
    pub fn push_notice(&mut self, conv: &Conversation, text: String, selected: &Conversation) -> bool {
        self.push_system(conv, text);
        let unseen = conv != selected;
        if unseen {
            *self.unread.entry(conv.clone()).or_default() += 1;
        }
        unseen
    }

    /// Gray notice in `conv`; never unread, never read-receipted.
    pub fn push_system(&mut self, conv: &Conversation, text: String) {
        let (peer_id, is_private) = match conv {
            Conversation::Dm(hex) => (Some(PeerId::from_hex(hex.clone())), true),
            _ => (None, false),
        };
        let line = ChatLine {
            id: String::new(),
            from: String::new(),
            peer_id,
            content: text,
            timestamp_ms: witchat_protocol::packet::now_millis(),
            is_self: false,
            is_private,
        };
        self.lines.push(Entry { conv: conv.clone(), line, system: true, media: None });
    }

    pub fn open(&mut self, conv: &Conversation) {
        self.unread.remove(conv);
    }

    pub fn unread(&self, conv: &Conversation) -> usize {
        self.unread.get(conv).copied().unwrap_or(0)
    }

    /// Delivery only moves forward, except a late ack overrides a failure.
    /// Queued and failed swap freely: the outbox can give up on a queued
    /// message, and a mesh retry failure can leave it queued. A copy with
    /// couriers stays noted until delivery or failure.
    pub fn set_delivery(&mut self, message_id: String, state: Delivery) {
        let rank = |d: Delivery| match d {
            Delivery::Failed | Delivery::Queued | Delivery::Couriered { .. } => 0,
            Delivery::Delivered => 1,
            Delivery::Read => 2,
        };
        let entry = self.delivery.entry(message_id).or_insert(state);
        let keeps_courier = matches!(*entry, Delivery::Couriered { .. }) && state == Delivery::Queued;
        if rank(state) >= rank(*entry) && !keeps_courier {
            *entry = state;
        }
    }

    pub fn delivery(&self, message_id: &str) -> Option<Delivery> {
        self.delivery.get(message_id).copied()
    }

    pub fn set_sending(&mut self, line_id: String, percent: u8) {
        self.sending.insert(line_id, percent.min(100));
    }

    pub fn sending(&self, line_id: &str) -> Option<u8> {
        self.sending.get(line_id).copied()
    }

    pub fn visible(&self, conv: &Conversation) -> Vec<&Entry> {
        self.lines.iter().filter(|e| e.conv == *conv).collect()
    }

    /// Location DM conversations with any line, most recently active first.
    pub fn geo_dms(&self) -> Vec<Conversation> {
        let mut seen = Vec::new();
        for e in self.lines.iter().rev() {
            if matches!(e.conv, Conversation::GeoDm { .. }) && !seen.contains(&e.conv) {
                seen.push(e.conv.clone());
            }
        }
        seen
    }

    /// Latest real message in `conv`, for previews.
    pub fn last(&self, conv: &Conversation) -> Option<&ChatLine> {
        self.lines.iter().rev().filter(|e| !e.system && e.conv == *conv).map(|e| &e.line).next()
    }

    /// `/clear`: forget everything shown in `conv`.
    pub fn clear(&mut self, conv: &Conversation) {
        self.lines.retain(|e| e.conv != *conv);
        self.unread.remove(conv);
        self.revision += 1;
    }

    /// Drop non-system lines matching `pred` (used when blocking a sender).
    pub fn remove_where(&mut self, pred: impl Fn(&Entry) -> bool) {
        self.lines.retain(|e| e.system || !pred(e));
        self.revision += 1;
    }

    /// Received DMs in `conv` that still need a read receipt; marks them sent.
    pub fn take_unreceipted(&mut self, conv: &Conversation) -> Vec<String> {
        if !matches!(conv, Conversation::Dm(_) | Conversation::GeoDm { .. }) {
            return Vec::new();
        }
        let ids: Vec<String> = self
            .lines
            .iter()
            .filter(|e| !e.system && e.conv == *conv)
            .map(|e| &e.line)
            .filter(|l| !l.is_self && !l.id.starts_with("local") && !self.read_sent.contains(&l.id))
            .map(|l| l.id.clone())
            .collect();
        self.read_sent.extend(ids.iter().cloned());
        ids
    }

    /// Sender ids in `conv` whose name is `nick` or `nick#suffix`, newest
    /// first, for `/block nick` lookups.
    pub fn sender_ids(&self, conv: &Conversation, nick: &str) -> Vec<String> {
        let nick = nick.trim_start_matches('@');
        let matches = |from: &str| {
            from.eq_ignore_ascii_case(nick)
                || from.split('#').next().is_some_and(|base| base.eq_ignore_ascii_case(nick))
        };
        let mut ids: Vec<String> = Vec::new();
        for e in self.lines.iter().rev().filter(|e| !e.system && !e.line.is_self && e.conv == *conv) {
            if matches(&e.line.from) && !ids.contains(&e.line.id) {
                ids.push(e.line.id.clone());
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: &str, peer: Option<&str>, is_self: bool, private: bool) -> ChatLine {
        ChatLine {
            id: id.into(),
            from: "x".into(),
            peer_id: peer.map(|p| PeerId::from_hex(p.to_string())),
            content: "hi".into(),
            timestamp_ms: 1,
            is_self,
            is_private: private,
        }
    }

    #[test]
    fn unread_counts_only_incoming_outside_selection() {
        let mut inbox = Inbox::default();
        let dm = Conversation::Dm("aa".into());
        assert!(inbox.push(line("1", Some("aa"), false, true), &Conversation::Mesh));
        assert!(!inbox.push(line("2", Some("aa"), false, true), &dm));
        assert!(!inbox.push(line("3", Some("aa"), true, true), &Conversation::Mesh));
        assert_eq!(inbox.unread(&dm), 1);
        inbox.open(&dm);
        assert_eq!(inbox.unread(&dm), 0);
    }

    #[test]
    fn notices_are_unread_only_when_elsewhere() {
        let mut inbox = Inbox::default();
        let dm = Conversation::Dm("aa".into());
        assert!(inbox.push_notice(&dm, "verified".into(), &Conversation::Mesh));
        assert!(!inbox.push_notice(&dm, "again".into(), &dm));
        assert_eq!(inbox.unread(&dm), 1);
        assert!(inbox.take_unreceipted(&dm).is_empty(), "notices never send read receipts");
    }

    #[test]
    fn delivery_moves_forward_and_late_ack_beats_failure() {
        let mut inbox = Inbox::default();
        inbox.set_delivery("m".into(), Delivery::Failed);
        inbox.set_delivery("m".into(), Delivery::Delivered);
        assert_eq!(inbox.delivery("m"), Some(Delivery::Delivered));
        inbox.set_delivery("m".into(), Delivery::Read);
        inbox.set_delivery("m".into(), Delivery::Delivered);
        assert_eq!(inbox.delivery("m"), Some(Delivery::Read));
    }

    #[test]
    fn a_couriered_copy_outlives_retries_but_not_delivery_or_failure() {
        let mut inbox = Inbox::default();
        let carried = Delivery::Couriered { forward_secret: true };
        inbox.set_delivery("m".into(), Delivery::Queued);
        inbox.set_delivery("m".into(), carried);
        inbox.set_delivery("m".into(), Delivery::Queued);
        assert_eq!(inbox.delivery("m"), Some(carried));
        inbox.set_delivery("m".into(), Delivery::Delivered);
        assert_eq!(inbox.delivery("m"), Some(Delivery::Delivered));
        inbox.set_delivery("n".into(), carried);
        inbox.set_delivery("n".into(), Delivery::Failed);
        assert_eq!(inbox.delivery("n"), Some(Delivery::Failed));
    }

    #[test]
    fn read_receipts_are_taken_once() {
        let mut inbox = Inbox::default();
        let dm = Conversation::Dm("aa".into());
        inbox.push(line("1", Some("aa"), false, true), &dm);
        inbox.push(line("2", Some("aa"), true, true), &dm);
        inbox.push(line("local-file-x", Some("aa"), false, true), &dm);
        assert_eq!(inbox.take_unreceipted(&dm), vec!["1".to_string()], "local ids mean nothing to the phone");
        assert!(inbox.take_unreceipted(&dm).is_empty());
        assert!(inbox.take_unreceipted(&Conversation::Mesh).is_empty());
    }

    #[test]
    fn system_notices_show_but_never_count() {
        let mut inbox = Inbox::default();
        let dm = Conversation::Dm("aa".into());
        inbox.push(line("1", Some("aa"), false, true), &dm);
        inbox.take_unreceipted(&dm);
        inbox.push_system(&dm, "* bob favorited you".into());
        assert_eq!(inbox.visible(&dm).len(), 2);
        assert_eq!(inbox.unread(&dm), 0);
        assert_eq!(inbox.last(&dm).unwrap().id, "1");
        assert!(inbox.take_unreceipted(&dm).is_empty());
    }

    #[test]
    fn geo_lines_are_filed_by_channel_and_clear_is_scoped() {
        let mut inbox = Inbox::default();
        let geo = Conversation::Geo("9q8yy".into());
        assert!(inbox.push_in(geo.clone(), line("pk1", None, false, false), &Conversation::Mesh));
        inbox.push(line("m", None, false, false), &Conversation::Mesh);
        assert_eq!(inbox.visible(&geo).len(), 1);
        assert_eq!(inbox.visible(&Conversation::Mesh).len(), 1);
        assert_eq!(inbox.sender_ids(&geo, "X"), vec!["pk1".to_string()]);
        inbox.clear(&geo);
        assert!(inbox.visible(&geo).is_empty());
        assert_eq!(inbox.unread(&geo), 0);
        assert_eq!(inbox.visible(&Conversation::Mesh).len(), 1);
        inbox.remove_where(|e| e.line.id == "m");
        assert!(inbox.visible(&Conversation::Mesh).is_empty());
    }

    #[test]
    fn location_dms_list_newest_first_and_take_read_receipts() {
        let mut inbox = Inbox::default();
        let a = Conversation::GeoDm { cell: "dn".into(), pubkey: "aaaa".into() };
        let b = Conversation::GeoDm { cell: "dn".into(), pubkey: "bbbb".into() };
        assert!(inbox.push_in(a.clone(), line("G1", None, false, true), &Conversation::Mesh));
        inbox.push_in(b.clone(), line("G2", None, true, true), &Conversation::Mesh);
        assert_eq!(inbox.geo_dms(), vec![b, a.clone()]);
        assert_eq!(inbox.take_unreceipted(&a), vec!["G1".to_string()]);
    }
}
