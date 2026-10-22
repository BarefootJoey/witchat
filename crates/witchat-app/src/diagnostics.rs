//! `/ping` and `/trace` replies, worded as iOS `CommandProcessor`.

use crate::mesh_worker::UiEvent;
use witchat_core::MeshRouter;
use witchat_protocol::PeerId;

pub fn pong_text(nick: &str, rtt_ms: u64, hops: Option<u8>) -> String {
    let hops = match hops {
        Some(1) => " · direct (1 hop)".to_string(),
        Some(n) => format!(" · {n} hops"),
        None => String::new(),
    };
    format!("pong from {nick}: {rtt_ms} ms{hops}")
}

/// `intermediates` empty means a direct link; None means no known path.
pub fn trace_text(nick: &str, intermediates: Option<Vec<String>>) -> String {
    let Some(hops) = intermediates else {
        return format!("no known path to {nick}");
    };
    let count = hops.len() + 1;
    let chain = std::iter::once("you".to_string())
        .chain(hops)
        .chain(std::iter::once(nick.to_string()))
        .collect::<Vec<_>>()
        .join(" → ");
    if count == 1 {
        format!("estimated path: {chain} (1 hop)")
    } else {
        format!("estimated path: {chain} ({count} hops)")
    }
}

pub fn nick(router: &MeshRouter, peer: &PeerId) -> String {
    router.peers.get(peer).map_or_else(|| format!("{}…", peer.short()), |p| p.nickname.clone())
}

pub fn trace(router: &MeshRouter, peer: &PeerId) -> UiEvent {
    let hops = router.trace(peer).map(|path| path.iter().map(|hop| nick(router, hop)).collect());
    UiEvent::Diagnostic { id: peer.hex.clone(), text: trace_text(&nick(router, peer), hops) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_read_like_ios() {
        assert_eq!(pong_text("bob", 42, Some(1)), "pong from bob: 42 ms · direct (1 hop)");
        assert_eq!(pong_text("bob", 42, Some(3)), "pong from bob: 42 ms · 3 hops");
        assert_eq!(pong_text("bob", 42, None), "pong from bob: 42 ms");
        assert_eq!(trace_text("bob", None), "no known path to bob");
        assert_eq!(trace_text("bob", Some(vec![])), "estimated path: you → bob (1 hop)");
        assert_eq!(
            trace_text("bob", Some(vec!["amy".into(), "cat".into()])),
            "estimated path: you → amy → cat → bob (3 hops)"
        );
    }
}
