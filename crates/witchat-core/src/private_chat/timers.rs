//! Periodic DM upkeep: retries, expiry, and stalled-handshake restarts.

use super::{PendingDm, ACK_TIMEOUT, HANDSHAKE_STALL, MAX_SEND_ATTEMPTS, PENDING_EXPIRY};
use crate::events::MeshEvent;
use crate::router::{Ingest, MeshRouter};
use std::time::Instant;
use witchat_protocol::{Packet, PeerId};

impl MeshRouter {
    /// Periodic DM upkeep: resend unacked DMs, expire ones that never made it,
    /// and restart stalled handshakes that still have queued DMs.
    pub fn tick_private(&mut self) -> Ingest {
        let mut out = Ingest::default();
        self.retry_unacked(&mut out);
        self.expire_pending(&mut out);
        for media in self.dm.media_retry.expire(Instant::now()) {
            tracing::info!(peer = %media.peer.hex, id = %media.message_id, "file: private media never acknowledged");
            out.events.push(MeshEvent::DeliveryFailed(media.peer, media.message_id));
        }
        out.outbox.extend(self.restart_stalled_handshakes());
        out
    }

    fn retry_unacked(&mut self, out: &mut Ingest) {
        let due: Vec<String> = self
            .dm
            .inflight
            .iter()
            .filter(|(_, dm)| dm.sent_at.elapsed() >= ACK_TIMEOUT)
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            let Some(dm) = self.dm.inflight.remove(&id) else {
                continue;
            };
            if dm.attempts >= MAX_SEND_ATTEMPTS {
                tracing::info!(peer = %dm.peer.hex, %id, "noise: DM not acknowledged; giving up");
                out.events.push(MeshEvent::DeliveryFailed(dm.peer, id));
                continue;
            }
            if self.sessions.is_ready(&dm.peer) {
                tracing::info!(peer = %dm.peer.hex, %id, attempt = dm.attempts + 1, "noise: resending unacked DM");
                match self.send_tracked(&dm.peer, &id, &dm.content, dm.attempts) {
                    Ok(pkt) => out.outbox.push(pkt),
                    Err(_) => out.events.push(MeshEvent::DeliveryFailed(dm.peer, id)),
                }
            } else {
                self.dm.pending.entry(dm.peer.hex.clone()).or_default().push(PendingDm {
                    message_id: id,
                    content: dm.content,
                    attempts: dm.attempts,
                    queued_at: Instant::now(),
                });
                out.outbox.extend(self.ensure_session(&dm.peer));
            }
        }
    }

    fn expire_pending(&mut self, out: &mut Ingest) {
        for (hex, queue) in self.dm.pending.iter_mut() {
            queue.retain(|dm| {
                let alive = dm.queued_at.elapsed() < PENDING_EXPIRY;
                if !alive {
                    tracing::info!(peer = %hex, id = %dm.message_id, "noise: queued DM expired without a session");
                    out.events.push(MeshEvent::DeliveryFailed(PeerId::from_hex(hex.clone()), dm.message_id.clone()));
                }
                alive
            });
        }
        self.dm.pending.retain(|_, q| !q.is_empty());
    }

    fn restart_stalled_handshakes(&mut self) -> Vec<Packet> {
        let stalled: Vec<PeerId> = self
            .dm
            .pending
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .map(|(hex, _)| PeerId::from_hex(hex.clone()))
            .filter(|peer| {
                !self.sessions.is_ready(peer)
                    && self.dm.handshake_started.get(&peer.hex).is_none_or(|t| t.elapsed() >= HANDSHAKE_STALL)
            })
            .collect();
        stalled
            .into_iter()
            .filter_map(|peer| {
                tracing::info!(peer = %peer.hex, "noise: handshake stalled; retrying");
                self.begin_handshake(peer).ok()
            })
            .collect()
    }
}
