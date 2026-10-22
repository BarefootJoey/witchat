//! Fragment payload: 8-byte id + u16 index + u16 total + original type + chunk.

use crate::constants::{DEFAULT_FRAGMENT_SIZE, SENDER_ID_SIZE};
use crate::message_type::MessageType;
use crate::packet::Packet;
use crate::payload_limits;
use crate::request_sync::MAX_FRAGMENT_IDS;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub id: [u8; 8],
    pub index: u16,
    pub total: u16,
    pub original_type: u8,
    pub data: Vec<u8>,
}

impl Fragment {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(13 + self.data.len());
        out.extend_from_slice(&self.id);
        out.extend_from_slice(&self.index.to_be_bytes());
        out.extend_from_slice(&self.total.to_be_bytes());
        out.push(self.original_type);
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < 13 {
            return None;
        }
        let mut id = [0u8; 8];
        id.copy_from_slice(&data[..8]);
        let index = u16::from_be_bytes([data[8], data[9]]);
        let total = u16::from_be_bytes([data[10], data[11]]);
        let original_type = data[12];
        Some(Self { id, index, total, original_type, data: data[13..].to_vec() })
    }
}

/// Fragment header bytes around each chunk: packet header, sender, recipient
/// and the 13-byte fragment prefix (iOS `fragmentFrameOverhead` plus one).
const FRAGMENT_FRAME_OVERHEAD: usize = 14 + 8 + 8 + 13;
/// Android rejects fragment sets larger than this; iOS treats it as the
/// private-media contract (`privateMediaV1MaxFragments`).
pub const MAX_FRAGMENTS: usize = 256;
const MIN_CHUNK: usize = 64;
/// Open fragment streams at once; past this the quietest is dropped, so a
/// neighbor minting fresh ids cannot grow the table without bound.
const MAX_ASSEMBLIES: usize = 128;

/// Chunk size so each fragment fits one write of `link_limit` bytes, as iOS
/// `fragmentChunkSize(forLinkLimit:)`, but never more than `MAX_FRAGMENTS`
/// pieces (those then exceed the limit and travel as long writes).
pub fn chunk_for_link(link_limit: Option<usize>, packet_len: usize) -> usize {
    let fit = link_limit.map_or(DEFAULT_FRAGMENT_SIZE, |l| l.saturating_sub(FRAGMENT_FRAME_OVERHEAD));
    let floor = packet_len.div_ceil(MAX_FRAGMENTS);
    fit.min(DEFAULT_FRAGMENT_SIZE).max(floor).max(MIN_CHUNK)
}

pub fn split_packet(packet: &Packet, chunk_size: Option<usize>) -> Option<Vec<Packet>> {
    let full = packet.encode(false)?;
    let chunk = chunk_size.unwrap_or(DEFAULT_FRAGMENT_SIZE).max(1);
    if full.len() <= chunk {
        return Some(vec![packet.clone()]);
    }
    let chunks: Vec<&[u8]> = full.chunks(chunk).collect();
    let total = chunks.len() as u16;
    // Random per packet, as in iOS: two fragmented packets from one sender
    // may be in flight at once (a file and a long DM) and must not merge.
    let mut id = [0u8; 8];
    id.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..8]);
    Some(
        chunks
            .into_iter()
            .enumerate()
            .map(|(index, data)| {
                let frag =
                    Fragment { id, index: index as u16, total, original_type: packet.type_id, data: data.to_vec() };
                let mut out = Packet::new(MessageType::Fragment as u8, packet.sender_id, frag.encode());
                out.recipient_id = packet.recipient_id;
                out.timestamp = packet.timestamp;
                out.ttl = packet.ttl;
                out
            })
            .collect(),
    )
}

#[derive(Default)]
pub struct FragmentAssembler {
    assemblies: HashMap<[u8; 8], Assembly>,
}

struct Assembly {
    original_type: u8,
    total: u16,
    chunks: HashMap<u16, Vec<u8>>,
    seen: Instant,
    /// Addressed to everyone, so any neighbor may hold the missing pieces.
    broadcast: bool,
    resync_asked: Option<Instant>,
}

impl FragmentAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest(&mut self, fragment: Fragment) -> Option<Packet> {
        self.ingest_from(fragment, false)
    }

    /// `broadcast`: the fragment packet had no recipient (or the broadcast
    /// address), so its stream can be recovered through gossip sync.
    pub fn ingest_from(&mut self, fragment: Fragment, broadcast: bool) -> Option<Packet> {
        if fragment.total == 0 || fragment.index >= fragment.total {
            return None;
        }
        let max_frame = payload_limits::max_frame_bytes(fragment.original_type);
        if !self.assemblies.contains_key(&fragment.id) && self.assemblies.len() >= MAX_ASSEMBLIES {
            if let Some(quietest) = self.assemblies.iter().min_by_key(|(_, a)| a.seen).map(|(id, _)| *id) {
                self.assemblies.remove(&quietest);
            }
        }
        let entry = self.assemblies.entry(fragment.id).or_insert_with(|| Assembly {
            original_type: fragment.original_type,
            total: fragment.total,
            chunks: HashMap::new(),
            seen: Instant::now(),
            broadcast,
            resync_asked: None,
        });
        if entry.total != fragment.total || entry.original_type != fragment.original_type {
            return None;
        }
        let assembled_so_far: usize = entry.chunks.values().map(|c| c.len()).sum();
        if assembled_so_far + fragment.data.len() > max_frame {
            self.assemblies.remove(&fragment.id);
            return None;
        }
        entry.chunks.insert(fragment.index, fragment.data);
        entry.seen = Instant::now();
        if entry.chunks.len() as u16 != entry.total {
            return None;
        }
        let mut full = Vec::new();
        for i in 0..entry.total {
            full.extend_from_slice(entry.chunks.get(&i)?);
        }
        self.assemblies.remove(&fragment.id);
        Packet::decode(&full)
    }

    pub fn gc(&mut self, max_age: Duration) {
        self.assemblies.retain(|_, a| a.seen.elapsed() < max_age);
    }

    /// iOS `stalledBroadcastFragmentIDs`: broadcast streams still missing
    /// pieces and quiet for `stalled_after`, not asked about within
    /// `retry_after`, longest-quiet first, at most [`MAX_FRAGMENT_IDS`].
    /// The returned streams count as asked about.
    pub fn stalled_broadcast(&mut self, stalled_after: Duration, retry_after: Duration, now: Instant) -> Vec<[u8; 8]> {
        let mut stalled: Vec<([u8; 8], Instant)> = self
            .assemblies
            .iter()
            .filter(|(_, a)| {
                a.broadcast
                    && (a.chunks.len() as u16) < a.total
                    && now.saturating_duration_since(a.seen) >= stalled_after
                    && a.resync_asked.is_none_or(|t| now.saturating_duration_since(t) >= retry_after)
            })
            .map(|(id, a)| (*id, a.seen))
            .collect();
        stalled.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        stalled.truncate(MAX_FRAGMENT_IDS);
        for (id, _) in &stalled {
            if let Some(a) = self.assemblies.get_mut(id) {
                a.resync_asked = Some(now);
            }
        }
        stalled.into_iter().map(|(id, _)| id).collect()
    }
}

pub fn sender_as_fragment_id(sender: [u8; SENDER_ID_SIZE]) -> [u8; 8] {
    sender
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(id: u8, index: u16, total: u16) -> Fragment {
        Fragment { id: [id; 8], index, total, original_type: MessageType::Message as u8, data: vec![index as u8; 8] }
    }

    #[test]
    fn stalled_broadcast_streams_are_asked_about_once_per_retry() {
        let mut assembler = FragmentAssembler::new();
        assembler.ingest_from(piece(1, 0, 3), true);
        assembler.ingest_from(piece(2, 0, 3), false);
        let quiet = Duration::from_secs(5);
        let retry = Duration::from_secs(10);
        let now = Instant::now();
        assert!(assembler.stalled_broadcast(quiet, retry, now).is_empty(), "not quiet long enough");
        let later = now + Duration::from_secs(6);
        assert_eq!(assembler.stalled_broadcast(quiet, retry, later), [[1; 8]], "only the broadcast stream");
        assert!(assembler.stalled_broadcast(quiet, retry, later + Duration::from_secs(5)).is_empty());
        assert_eq!(assembler.stalled_broadcast(quiet, retry, later + retry).len(), 1);
    }

    #[test]
    fn open_streams_are_capped_dropping_the_quietest() {
        let mut assembler = FragmentAssembler::new();
        let first = Fragment { id: [0xff; 8], ..piece(0, 0, 2) };
        assembler.ingest(first);
        std::thread::sleep(Duration::from_millis(5));
        for n in 0..MAX_ASSEMBLIES as u32 {
            let mut id = [0u8; 8];
            id[..4].copy_from_slice(&n.to_be_bytes());
            assembler.ingest(Fragment { id, ..piece(0, 0, 2) });
        }
        assert_eq!(assembler.assemblies.len(), MAX_ASSEMBLIES);
        assert!(!assembler.assemblies.contains_key(&[0xff; 8]), "the oldest stream goes first");
    }
}
