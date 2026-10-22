use std::collections::HashMap;
use witchat_crypto::{NoiseRole, NoiseSession};
use witchat_protocol::PeerId;

#[derive(Default)]
pub struct SessionTable {
    sessions: HashMap<String, NoiseSession>,
}

impl SessionTable {
    pub fn get(&self, peer: &PeerId) -> Option<&NoiseSession> {
        self.sessions.get(&peer.hex)
    }

    pub fn get_mut(&mut self, peer: &PeerId) -> Option<&mut NoiseSession> {
        self.sessions.get_mut(&peer.hex)
    }

    pub fn insert(&mut self, peer: PeerId, session: NoiseSession) {
        self.sessions.insert(peer.hex, session);
    }

    pub fn remove(&mut self, peer: &PeerId) {
        self.sessions.remove(&peer.hex);
    }

    pub fn ensure(
        &mut self,
        peer: PeerId,
        role: NoiseRole,
        local_static: &[u8; 32],
    ) -> Result<&mut NoiseSession, String> {
        if !self.sessions.contains_key(&peer.hex) {
            let session = NoiseSession::new(role, local_static).map_err(|e| e.to_string())?;
            self.sessions.insert(peer.hex.clone(), session);
        }
        Ok(self.sessions.get_mut(&peer.hex).unwrap())
    }

    /// Drop any in-progress handshake and start again with `role`.
    pub fn reset(
        &mut self,
        peer: PeerId,
        role: NoiseRole,
        local_static: &[u8; 32],
    ) -> Result<&mut NoiseSession, String> {
        self.sessions.remove(&peer.hex);
        self.ensure(peer, role, local_static)
    }

    pub fn is_ready(&self, peer: &PeerId) -> bool {
        self.sessions.get(&peer.hex).is_some_and(|s| s.is_transport())
    }

    pub fn ready_count(&self) -> usize {
        self.sessions.values().filter(|s| s.is_transport()).count()
    }
}
