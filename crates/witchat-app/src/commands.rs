//! User commands for the mesh worker, and slash-command parsing.

use crate::social::Contact;

pub enum Command {
    Public(String),
    Private {
        to: String,
        content: String,
    },
    Handshake(String),
    /// A DM conversation was opened; secure it in the background.
    OpenDm(String),
    /// The user has seen this DM; send a read receipt.
    MarkRead {
        peer: String,
        message_id: String,
    },
    /// (Un)favorite a peer and tell them, like iOS `[FAVORITED]`.
    Favorite {
        to: String,
        on: bool,
    },
    Announce,
    SetNick(String),
    JoinGeo(String),
    LeaveGeo(String),
    /// Count people in these listed cells without joining; empty stops.
    SampleGeo(Vec<String>),
    /// Cells around the latest location fix, for presence (Tor only).
    PresenceCells(Vec<String>),
    /// Route relays through this Tor SOCKS proxy, or directly when None.
    Tor(Option<String>),
    /// `teleported` when the cell is not around the user's located position.
    Geo {
        cell: String,
        text: String,
        teleported: bool,
    },
    /// Private message from our `cell` key to someone heard there.
    GeoDm {
        cell: String,
        pubkey: String,
        text: String,
    },
    /// Favorites reachable over Nostr; replaces the previous list.
    Contacts(Vec<Contact>),
    /// Blocked fingerprints; replaces the previous set.
    Blocked(Vec<String>),
    /// Mutual-favorite fingerprints, the preferred couriers; replaces the previous set.
    CourierFavorites(Vec<String>),
    /// User-added private-message relays; replaces the previous list.
    Relays(Vec<String>),
    /// Keep the PC from idle sleep while relaying on mains power.
    StayAwake(bool),
    /// Send a local image, voice note or PDF: to `#mesh` when `to` is None,
    /// otherwise privately. `visible` permits the signed-but-unencrypted
    /// fallback for peers without encrypted private media.
    SendFile {
        to: Option<String>,
        path: String,
        visible: bool,
    },
    /// Build a fresh signed QR for the "verify me" view.
    MyQr,
    /// Check a scanned `bitchat://verify` string and challenge its owner.
    VerifyQr(String),
    /// Emergency wipe: new identity, no sessions, no channels.
    Panic,
    /// Measure the round trip to a mesh peer (nickname or id).
    Ping(String),
    /// Estimate the mesh path to a peer (nickname or id).
    Trace(String),
    /// Debug tab requests.
    Diag(crate::worker_diag::DiagCommand),
    /// Release Bluetooth and stop the worker; answered once done.
    Shutdown(std::sync::mpsc::Sender<()>),
    /// Release Bluetooth, turn the radio off and on, then ask for a relaunch.
    ResetBluetooth,
}

/// Commands the worker understands. The GUI handles conversation-scoped
/// ones (`/block`, `/clear`, `/leave`) itself before calling this.
pub fn parse_command_line(line: &str) -> Option<Command> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(nick) = line.strip_prefix("/nick ") {
        return Some(Command::SetNick(nick.trim().into()));
    }
    if let Some(rest) = line.strip_prefix("/msg ") {
        let mut parts = rest.splitn(2, ' ');
        return Some(Command::Private {
            to: parts.next().unwrap_or("").into(),
            content: parts.next().unwrap_or("").into(),
        });
    }
    if let Some(to) = line.strip_prefix("/fav ") {
        return Some(Command::Favorite { to: to.trim().into(), on: true });
    }
    if let Some(to) = line.strip_prefix("/unfav ") {
        return Some(Command::Favorite { to: to.trim().into(), on: false });
    }
    if let Some(to) = line.strip_prefix("/hs ") {
        return Some(Command::Handshake(to.trim().into()));
    }
    if let Some(cell) = line.strip_prefix("/geo ") {
        return Some(Command::JoinGeo(normalize_geohash(cell)));
    }
    if let Some(path) = line.strip_prefix("/send ") {
        return Some(Command::SendFile { to: None, path: path.trim().into(), visible: false });
    }
    if let Some(to) = line.strip_prefix("/ping ") {
        return Some(Command::Ping(to.trim().trim_start_matches('@').into()));
    }
    if let Some(to) = line.strip_prefix("/trace ") {
        return Some(Command::Trace(to.trim().trim_start_matches('@').into()));
    }
    if line == "/who" || line == "/peers" {
        return Some(Command::Announce);
    }
    if line == "/diag" {
        return Some(Command::Diag(crate::worker_diag::DiagCommand::Report));
    }
    if line.starts_with('/') {
        // A typo'd command must never leak to everyone nearby as chat.
        return None;
    }
    Some(Command::Public(line.into()))
}

pub fn normalize_geohash(value: &str) -> String {
    value.trim().trim_start_matches('#').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands_parse() {
        assert!(
            matches!(parse_command_line("/msg bob hi there"), Some(Command::Private { to, content }) if to == "bob" && content == "hi there")
        );
        assert!(matches!(parse_command_line("/who"), Some(Command::Announce)));
        assert!(matches!(parse_command_line("/diag"), Some(Command::Diag(crate::worker_diag::DiagCommand::Report))));
        assert!(matches!(parse_command_line("/fav bob"), Some(Command::Favorite { to, on: true }) if to == "bob"));
        assert!(matches!(parse_command_line("/unfav bob"), Some(Command::Favorite { on: false, .. })));
        assert!(matches!(parse_command_line("/ping @bob"), Some(Command::Ping(t)) if t == "bob"));
        assert!(matches!(parse_command_line("/trace bob"), Some(Command::Trace(t)) if t == "bob"));
        assert!(matches!(parse_command_line("/geo #9Q8YY"), Some(Command::JoinGeo(c)) if c == "9q8yy"));
        assert!(
            matches!(parse_command_line("/send C:\\a b.png"), Some(Command::SendFile { to: None, path, .. }) if path == "C:\\a b.png")
        );
        assert!(matches!(parse_command_line("hello"), Some(Command::Public(t)) if t == "hello"));
        assert!(parse_command_line("   ").is_none());
        assert!(parse_command_line("/sned x").is_none(), "unknown commands are not chat");
    }
}
