use witchat_protocol::PeerId;

#[derive(Debug, Clone)]
pub enum MeshEvent {
    Status(String),
    PeerDiscovered(PeerId, String),
    PeerLost(PeerId),
    PublicMessage(ChatLine),
    PrivateMessage(ChatLine),
    /// Status is one of `handshaking`, `secured`, `failed`.
    HandshakeProgress(PeerId, String),
    /// Peer acknowledged our DM with this message id.
    Delivered(PeerId, String),
    /// Peer read our DM with this message id.
    Read(PeerId, String),
    /// Our DM was never acknowledged after retries.
    DeliveryFailed(PeerId, String),
    /// The session-delivered signing key matched (`true`) or contradicted
    /// (`false`) the peer's announce.
    PeerAuthenticated(PeerId, bool),
    /// Peer (un)favorited us.
    FavoriteNotice(PeerId, crate::FavoriteNotice),
    /// Identity learned from a sync replay; the peer is not reachable now.
    PeerKnown(PeerId, String),
    /// A validated image, voice note or file (broadcast or private).
    FileReceived(crate::IncomingFile),
    /// A peer started (`live`) or finished/canceled a push-to-talk burst, to
    /// us in a DM or to everyone in `#mesh` (`public`).
    VoiceLive {
        peer: PeerId,
        burst: String,
        live: bool,
        public: bool,
    },
    /// The peer proved it owns the identity in a QR we scanned: its
    /// fingerprint, and the `npub` the QR carried (signed with it).
    QrVerified(PeerId, String, Option<String>),
    /// The peer scanned our QR and we answered its challenge.
    QrChallenged(PeerId),
    /// The peer answered our challenge with a signature that does not
    /// match the QR's signing key: the QR is not this peer's.
    QrMismatch(PeerId),
    /// The peer answered our `/ping`; `hops` is None when its TTLs are odd.
    Pong {
        peer: PeerId,
        rtt_ms: u64,
        hops: Option<u8>,
    },
    /// Our `/ping` to this peer went unanswered.
    PingTimeout(PeerId),
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ChatLine {
    pub id: String,
    pub from: String,
    pub peer_id: Option<PeerId>,
    pub content: String,
    pub timestamp_ms: u64,
    pub is_self: bool,
    pub is_private: bool,
}
