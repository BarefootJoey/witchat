//! Bounded automatic resend of private media, as iOS
//! `PrivateMediaReconnectRetryLimits`: a file the peer has not acked by its
//! stable id is kept for two minutes and sent again, at most twice, when the
//! peer authenticates a *new* session proving `privateMediaReceipts` (bit 9).
//! Receivers dedupe by that id, so a resend never creates a second bubble.

use std::collections::VecDeque;
use std::time::{Duration, Instant};
use witchat_protocol::{FilePacket, PeerId};

const MAX_RETAINED: usize = 8;
const MAX_RETAINED_BYTES: usize = 4 * 1024 * 1024;
const MAX_RETRIES: u8 = 2;
const MAX_RETRIES_PER_RECONNECT: usize = 2;
pub(crate) const RETENTION: Duration = Duration::from_secs(120);

pub(crate) struct Retained {
    pub message_id: String,
    pub peer: PeerId,
    pub file: FilePacket,
    /// Session generation the file last went out on.
    session: u64,
    retries: u8,
    created: Instant,
}

#[derive(Default)]
pub(crate) struct MediaRetry {
    records: VecDeque<Retained>,
}

impl MediaRetry {
    pub(crate) fn retain(&mut self, message_id: String, peer: PeerId, file: FilePacket, session: u64, now: Instant) {
        self.records.retain(|r| r.message_id != message_id);
        self.records.push_back(Retained { message_id, peer, file, session, retries: 0, created: now });
        while self.records.len() > MAX_RETAINED || self.bytes() > MAX_RETAINED_BYTES {
            self.records.pop_front();
        }
    }

    /// The peer acknowledged `message_id`; stop tracking it.
    pub(crate) fn acked(&mut self, message_id: &str) -> bool {
        let before = self.records.len();
        self.records.retain(|r| r.message_id != message_id);
        self.records.len() != before
    }

    /// Files to send again now that `peer` proved receipts on `session`.
    pub(crate) fn due(&mut self, peer: &PeerId, session: u64, now: Instant) -> Vec<(String, FilePacket)> {
        self.records
            .iter_mut()
            .filter(|r| r.peer == *peer && r.session != session && r.retries < MAX_RETRIES)
            .filter(|r| now.duration_since(r.created) < RETENTION)
            .take(MAX_RETRIES_PER_RECONNECT)
            .map(|r| {
                r.session = session;
                r.retries += 1;
                (r.message_id.clone(), r.file.clone())
            })
            .collect()
    }

    /// Drop records past the retention window; they were never acked.
    pub(crate) fn expire(&mut self, now: Instant) -> Vec<Retained> {
        let (gone, kept) = self.records.drain(..).partition(|r| now.duration_since(r.created) >= RETENTION);
        self.records = kept;
        gone.into()
    }

    fn bytes(&self) -> usize {
        self.records.iter().map(|r| r.file.content.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(n: usize) -> FilePacket {
        FilePacket { name: Some("voice_x.m4a".into()), mime: Some("audio/m4a".into()), content: vec![0; n] }
    }

    #[test]
    fn resends_twice_on_new_sessions_only() {
        let peer = PeerId::from_hex("aa".repeat(8));
        let t0 = Instant::now();
        let mut retry = MediaRetry::default();
        retry.retain("media-1".into(), peer.clone(), file(10), 1, t0);
        assert!(retry.due(&peer, 1, t0).is_empty(), "same session: the first copy may still land");
        assert_eq!(retry.due(&peer, 2, t0).len(), 1);
        assert!(retry.due(&peer, 2, t0).is_empty(), "once per session");
        assert_eq!(retry.due(&peer, 3, t0).len(), 1);
        assert!(retry.due(&peer, 4, t0).is_empty(), "at most two retries");
    }

    #[test]
    fn acks_expiry_and_caps_bound_the_store() {
        let peer = PeerId::from_hex("bb".repeat(8));
        let t0 = Instant::now();
        let mut retry = MediaRetry::default();
        for i in 0..10 {
            retry.retain(format!("media-{i}"), peer.clone(), file(10), 1, t0);
        }
        assert_eq!(retry.records.len(), MAX_RETAINED);
        assert!(retry.acked("media-9"));
        assert!(!retry.acked("media-0"), "oldest were evicted");
        retry.retain("big".into(), peer.clone(), file(MAX_RETAINED_BYTES), 1, t0);
        assert_eq!(retry.records.len(), 1, "byte cap keeps only what fits");
        assert_eq!(retry.expire(t0 + RETENTION).len(), 1);
        assert!(retry.due(&peer, 9, t0 + RETENTION).is_empty());
    }
}
