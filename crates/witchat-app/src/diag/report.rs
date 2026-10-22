//! Plain-text diagnostics reports, safe to paste into a public issue: no
//! message text, keys or nicknames, and identifiers are shortened (see
//! [`super::redact`]).

use super::redact::{self, mask_device, short_id};
use super::{human_bytes, human_duration, Snapshot};
use crate::outbound::SendStats;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::time::SystemTime;
use witchat_core::MeshStats;

/// Recent WARN/ERROR lines included in a report.
pub const PROBLEM_LINES: usize = 40;

/// `witchat 0.4.0 (abc123def456) windows/x86_64`.
pub fn build_line() -> String {
    format!(
        "witchat {} ({}) {}/{}",
        env!("CARGO_PKG_VERSION"),
        env!("WITCHAT_GIT_REV"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// The live report behind "Copy diagnostics report".
pub fn live(snap: &Snapshot) -> String {
    let mut r = header("live");
    radio(&mut r, snap);
    mesh(&mut r, snap);
    let _ = writeln!(r, "\n[for the mesh]");
    for (label, value) in snap.contribution_lines() {
        let _ = writeln!(r, "{label}: {value}");
    }
    peers(&mut r, snap);
    relays(&mut r, snap);
    problems(&mut r);
    r
}

/// `witchat --diag`: what can be checked without starting the mesh.
pub fn offline(adapter: Result<String, String>) -> String {
    let mut r = header("offline");
    let social =
        crate::social::Social::default_path().and_then(|p| crate::social::Social::load(&p).ok()).unwrap_or_default();
    let _ = writeln!(r, "adapter: {}", adapter.unwrap_or_else(|e| format!("NOT AVAILABLE ({e})")));
    let _ = writeln!(r, "advertising: {}", if cfg!(any(windows, target_os = "linux")) { "supported" } else { "no" });
    let _ = writeln!(r, "key protection: {}", crate::key_protect::Protection::current().describe());
    let _ = writeln!(r, "identity: {}", if crate::identity_store::exists() { "present" } else { "not created yet" });
    let _ = writeln!(r, "tor: {}", if social.tor_proxy.is_some() { "on" } else { "off" });
    let _ = writeln!(r, "verbose logs: {}", social.verbose_logs);
    let _ = writeln!(r, "contacts: {}  blocked: {}", social.contacts.len(), social.blocked.len());
    let _ = writeln!(r, "logs: {}", crate::logging::dir_path().map_or("-".into(), |d| d.display().to_string()));
    problems(&mut r);
    r
}

fn header(kind: &str) -> String {
    let mut r = String::new();
    let _ = writeln!(r, "witchat diagnostics ({kind})");
    let _ = writeln!(r, "build: {}", build_line());
    let _ = writeln!(r, "generated: {}", crate::logging::utc_stamp(SystemTime::now()));
    r
}

fn radio(r: &mut String, s: &Snapshot) {
    let radio = &s.radio;
    let _ = writeln!(r, "uptime: {}", human_duration(s.uptime));
    let _ = writeln!(r, "\n[radio]");
    let _ = writeln!(r, "adapter: {}", s.adapter.as_deref().unwrap_or("none"));
    let _ = writeln!(r, "role: {} ({})", if s.advertising { "central + peripheral" } else { "central only" }, s.role);
    let _ = writeln!(
        r,
        "scanning: {}{}  ads in range: {}  bitchat: {}  links: {} out / {} in (max {})  attaching: {}  duplicates retired: {}",
        yes(radio.scanning),
        if radio.low_power { " (battery duty cycle)" } else { "" },
        radio.ads_seen,
        radio.bitchat.len(),
        radio.outbound(),
        radio.inbound(),
        radio.max_links,
        radio.attaching,
        s.retired_links
    );
    for link in &radio.links {
        let (frames_in, bytes_in) = s.frames_in.get(&link.id).copied().unwrap_or_default();
        let _ = writeln!(
            r,
            "  link {}  peer {}  write {}  rssi {}  up {}  heard {}  out {} ({})  in {} ({})",
            mask_device(&link.id),
            s.link_peers.get(&link.id).map_or("-".into(), |id| short_id(id)),
            link.max_write.map_or("?".into(), |n| n.to_string()),
            link.rssi.map_or("-".into(), |n| n.to_string()),
            human_duration(link.up_for),
            link.heard_ago.map_or("-".into(), human_duration),
            link.frames_out,
            human_bytes(link.bytes_out),
            frames_in,
            human_bytes(bytes_in),
        );
    }
    for seen in &radio.bitchat {
        let _ = writeln!(
            r,
            "  seen {}  rssi {}{}{}",
            mask_device(&seen.id),
            seen.rssi.map_or("-".into(), |n| n.to_string()),
            if seen.linked { "  linked" } else { "" },
            seen.retry_in.map_or(String::new(), |d| format!("  retry in {}", human_duration(d))),
        );
    }
}

fn mesh(r: &mut String, s: &Snapshot) {
    let m = &s.mesh;
    let _ = writeln!(r, "\n[mesh]");
    let _ = writeln!(r, "relaying: {}  tor: {}  verbose logs: {}", yes(s.relaying), yes(s.tor), yes(s.verbose));
    let _ = writeln!(r, "received {}: {}", m.received_total(), MeshStats::line(&m.received));
    let _ = writeln!(r, "relayed: {}", m.relayed);
    let _ = writeln!(r, "dropped {}: {}", m.dropped_total(), MeshStats::line(&m.drops));
    let _ = writeln!(r, "queued {}: {}", s.sent_total(), sent_line(&s.sent));
    let sync = m.sync;
    let _ = writeln!(
        r,
        "sync: asked {}  answered {}  sent {}  rate-limited {}  holding {} announces, {} messages, {} prekey bundles, {} fragments",
        sync.asked,
        sync.answered,
        sync.sent,
        sync.rate_limited,
        sync.held_announces,
        sync.held_messages,
        sync.held_bundles,
        sync.held_fragments
    );
    let confirmed = s.topology.edges.iter().filter(|e| e.confirmed).count();
    let _ = writeln!(
        r,
        "topology: {} nodes, {} links ({} confirmed)",
        s.topology.nodes().len(),
        s.topology.edges.len(),
        confirmed
    );
    let _ = writeln!(r, "courier mail carried: {}  outbox waiting: {}", s.courier, s.outbox);
    let _ = writeln!(r, "prekeys: {}", s.prekey_line());
}

fn peers(r: &mut String, s: &Snapshot) {
    let _ =
        writeln!(r, "\n[peers] {} known, {} direct, {} secured sessions", s.peers.len(), s.direct_peers(), s.secured);
    for p in &s.peers {
        let _ = writeln!(
            r,
            "  {}  {}{}{}  session {}  seen {} ago  caps {}",
            short_id(&p.id),
            if p.direct { "direct" } else { "relayed" },
            if p.connected { " online" } else { "" },
            if p.verified { " signed" } else { " UNSIGNED" },
            p.session,
            human_duration(p.seen),
            if p.caps.is_empty() { "-".into() } else { p.caps.join(",") },
        );
    }
}

fn relays(r: &mut String, s: &Snapshot) {
    let _ = writeln!(r, "\n[relays] {}", s.relay_summary());
    for relay in s.relays.iter().filter(|r| r.state != witchat_nostr::relay_health::RelayState::Healthy) {
        let _ = writeln!(
            r,
            "  {}  {}  ok {}  failed {}  {}",
            relay.url,
            relay.state.label(),
            relay.ok,
            relay.failures,
            relay.last_error.as_deref().map(redact::line).unwrap_or_default()
        );
    }
}

fn problems(r: &mut String) {
    let lines = recent_problems(PROBLEM_LINES);
    let _ = writeln!(r, "\n[recent warnings and errors] {}", lines.len());
    for line in lines {
        let _ = writeln!(r, "  {line}");
    }
}

fn sent_line(sent: &SendStats) -> String {
    let mut line = MeshStats::line(&sent.queued);
    if sent.no_link > 0 {
        let _ = write!(line, "  (with no link up: {})", sent.no_link);
    }
    line
}

fn yes(on: bool) -> &'static str {
    if on {
        "yes"
    } else {
        "no"
    }
}

/// WARN/ERROR lines scanned per report, newest first.
const SCAN_LINES: usize = 5_000;

/// The last `n` distinct WARN/ERROR messages from the two newest log files,
/// oldest first and redacted. Repeats collapse to one line with a count.
pub fn recent_problems(n: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for file in crate::logging::files_newest_first().into_iter().take(2).rev() {
        let Ok(handle) = std::fs::File::open(&file) else { continue };
        lines.extend(BufReader::new(handle).lines().map_while(Result::ok).filter(|l| is_problem(l)));
        let excess = lines.len().saturating_sub(SCAN_LINES);
        lines.drain(..excess);
    }
    collapse(&lines, n).iter().map(|l| redact::line(l)).collect()
}

/// Group lines by everything after the timestamp; keep the `n` groups seen
/// most recently, each shown at its last time with `(xN)` when repeated.
fn collapse(lines: &[String], n: usize) -> Vec<String> {
    let mut groups: Vec<(&str, &str, usize)> = Vec::new();
    for line in lines {
        let (stamp, body) = line.split_once(char::is_whitespace).unwrap_or(("", line));
        let body = body.trim_start();
        match groups.iter().position(|(b, _, _)| *b == body) {
            Some(i) => {
                let (b, _, count) = groups.remove(i);
                groups.push((b, stamp, count + 1));
            }
            None => groups.push((body, stamp, 1)),
        }
    }
    let start = groups.len().saturating_sub(n);
    groups[start..]
        .iter()
        .map(|(body, stamp, count)| match count {
            1 => format!("{stamp} {body}"),
            _ => format!("{stamp} (x{count}) {body}"),
        })
        .collect()
}

fn is_problem(line: &str) -> bool {
    line.split_whitespace().nth(1).is_some_and(|level| level == "WARN" || level == "ERROR")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_collapse_to_their_last_time() {
        let lines: Vec<String> =
            ["t1  WARN a: relay down x", "t2  WARN b: other", "t3  WARN a: relay down x", "t4 ERROR c: broke"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(collapse(&lines, 10), vec!["t2 WARN b: other", "t3 (x2) WARN a: relay down x", "t4 ERROR c: broke"]);
        assert_eq!(collapse(&lines, 1), vec!["t4 ERROR c: broke"]);
        assert!(is_problem("2026-10-18T01:02:03Z  WARN x: y"));
        assert!(!is_problem("2026-10-18T01:02:03Z  INFO x: WARN"));
    }
}
