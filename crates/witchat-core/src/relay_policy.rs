//! Flood control for relays (iOS `RelayController`): how many hops a relayed
//! packet keeps and how long to wait before sending it. The wait lets a copy
//! from another neighbor arrive first, so the relay can be cancelled; the
//! TTL caps keep dense meshes from echoing broadcasts seven hops deep.

use std::ops::RangeInclusive;
use witchat_protocol::constants::DEFAULT_FRAGMENT_SIZE;
use witchat_protocol::{MessageType, Packet, DEFAULT_TTL};

/// iOS `bleHighDegreeThreshold`: connected peers from which a node is "dense".
pub const HIGH_DEGREE: usize = 6;
/// iOS `bleFragmentRelayTtlCap` / `…Dense`.
const FRAGMENT_TTL_CAP: u8 = 7;
const FRAGMENT_TTL_CAP_DENSE: u8 = 5;
/// With more connected peers than this, a duplicate cancels a pending relay
/// (iOS `shouldCancelScheduledRelayForDuplicate`).
pub const CANCEL_ABOVE_DEGREE: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Handshakes: deterministic, short jitter.
    Handshake,
    /// Directed DMs, courier envelopes, ping/pong, Nostr carriers.
    Directed,
    /// Media (packets we re-fragment) and live voice frames.
    Media,
    Broadcast {
        announce: bool,
    },
}

fn class(packet: &Packet) -> Option<Class> {
    let ty = MessageType::from_u8(packet.type_id);
    let directed = packet.recipient_id.is_some_and(|r| r != [0xff; 8]);
    Some(match ty {
        Some(MessageType::RequestSync) => return None,
        Some(MessageType::NoiseHandshake) => Class::Handshake,
        Some(
            MessageType::NoiseEncrypted
            | MessageType::CourierEnvelope
            | MessageType::Ping
            | MessageType::Pong
            | MessageType::NostrCarrier,
        ) if directed => Class::Directed,
        _ if directed && packet.payload.len() > DEFAULT_FRAGMENT_SIZE => Class::Directed,
        Some(MessageType::VoiceFrame) => Class::Media,
        _ if packet.payload.len() > DEFAULT_FRAGMENT_SIZE => Class::Media,
        Some(MessageType::Announce) => Class::Broadcast { announce: true },
        _ => Class::Broadcast { announce: false },
    })
}

/// The TTL a relayed copy of `packet` carries, or None to not relay it.
/// `degree`: connected peers. With 0–2 peers broadcasts keep full depth,
/// which is what witchat did before it had this policy.
pub fn relay_ttl(packet: &Packet, degree: usize) -> Option<u8> {
    let class = class(packet)?;
    let cap = packet.ttl.min(DEFAULT_TTL);
    if cap <= 1 {
        return None;
    }
    let limit = match class {
        Class::Handshake | Class::Directed => cap,
        Class::Media => cap.min(if degree >= HIGH_DEGREE { FRAGMENT_TTL_CAP_DENSE } else { FRAGMENT_TTL_CAP }),
        Class::Broadcast { .. } if degree <= 2 => cap,
        Class::Broadcast { .. } if degree >= HIGH_DEGREE => cap.min(5),
        Class::Broadcast { announce } => cap.min(if announce { 7 } else { 6 }),
    };
    (limit > 1).then(|| limit - 1)
}

/// How long to wait before sending a relayed copy, in milliseconds.
pub fn relay_delay_ms(packet: &Packet, degree: usize) -> RangeInclusive<u64> {
    match class(packet) {
        Some(Class::Handshake) => 10..=35,
        Some(Class::Directed) => 20..=60,
        Some(Class::Media) => 8..=25,
        Some(Class::Broadcast { .. }) | None => match degree {
            0..=2 => 10..=40,
            3..=5 => 60..=150,
            6..=9 => 80..=180,
            _ => 100..=220,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(ty: MessageType, ttl: u8, to: Option<[u8; 8]>, len: usize) -> Packet {
        let mut p = Packet::new(ty as u8, [1; 8], vec![0; len]);
        p.ttl = ttl;
        p.recipient_id = to;
        p
    }

    #[test]
    fn broadcasts_keep_full_depth_on_thin_chains_and_are_capped_when_dense() {
        let msg = packet(MessageType::Message, 7, None, 20);
        assert_eq!(relay_ttl(&msg, 1), Some(6));
        assert_eq!(relay_ttl(&msg, 4), Some(5), "capped at 6 before the hop");
        assert_eq!(relay_ttl(&msg, 8), Some(4), "dense: capped at 5");
        let announce = packet(MessageType::Announce, 7, None, 20);
        assert_eq!(relay_ttl(&announce, 4), Some(6));
        assert_eq!(relay_ttl(&packet(MessageType::Message, 1, None, 20), 1), None);
        assert_eq!(relay_ttl(&packet(MessageType::Message, 2, None, 20), 8), Some(1));
        assert_eq!(relay_ttl(&packet(MessageType::Message, 200, None, 20), 1), Some(6), "TTL above 7 is clamped");
    }

    #[test]
    fn directed_and_session_traffic_is_never_capped() {
        for ty in [MessageType::NoiseEncrypted, MessageType::Ping, MessageType::CourierEnvelope] {
            assert_eq!(relay_ttl(&packet(ty, 7, Some([2; 8]), 20), 12), Some(6), "{ty:?}");
        }
        assert_eq!(relay_ttl(&packet(MessageType::NoiseHandshake, 7, Some([2; 8]), 20), 12), Some(6));
        assert_eq!(relay_delay_ms(&packet(MessageType::NoiseHandshake, 7, None, 20), 12), 10..=35);
        assert_eq!(relay_delay_ms(&packet(MessageType::NoiseEncrypted, 7, Some([2; 8]), 20), 12), 20..=60);
    }

    #[test]
    fn media_uses_the_fragment_caps_and_sync_never_relays() {
        let file = packet(MessageType::FileTransfer, 7, None, 4_000);
        assert_eq!(relay_ttl(&file, 1), Some(6));
        assert_eq!(relay_ttl(&file, 7), Some(4));
        assert_eq!(relay_delay_ms(&file, 7), 8..=25);
        assert_eq!(relay_ttl(&packet(MessageType::RequestSync, 7, None, 20), 1), None);
    }

    #[test]
    fn broadcast_waits_grow_with_degree() {
        let msg = packet(MessageType::Message, 7, None, 20);
        assert_eq!(relay_delay_ms(&msg, 2), 10..=40);
        assert_eq!(relay_delay_ms(&msg, 5), 60..=150);
        assert_eq!(relay_delay_ms(&msg, 9), 80..=180);
        assert_eq!(relay_delay_ms(&msg, 10), 100..=220);
    }
}
