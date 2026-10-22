//! `--geo-lab <cell>`: why does (or doesn't) a phone list us in a location
//! channel? Uses the GUI's own per-cell key and Tor setting, sends one
//! heartbeat to each of the cell's relays, and prints whom each relay fans
//! out while we listen.

use crate::identity_store;
use crate::social::Social;
use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use witchat_nostr::geo_probe::{self, ProbeLine};
use witchat_nostr::{derive_geo_keys, derive_geo_seed, ios_relays_for, is_valid_geohash, socks_ready, Transport};

#[derive(Default)]
struct RelayTally {
    connected: Option<Result<(), String>>,
    heartbeat: Option<Result<(), String>>,
    echoed: bool,
    others: BTreeSet<String>,
}

pub fn run(cell: &str, listen: Duration) -> Result<()> {
    let cell = cell.trim().trim_start_matches('#').to_lowercase();
    if !is_valid_geohash(&cell) {
        bail!("{cell} is not a geohash");
    }
    crate::relay_refresh::load_cached();
    let identity = identity_store::load_or_create(None)?;
    let keys = derive_geo_keys(&derive_geo_seed(&identity.noise_secret_bytes()), &cell);
    let pubkey = keys.public_key().to_hex();
    let social = match Social::default_path() {
        Some(path) => Social::load(&path)?,
        None => Social::default(),
    };
    let transport = Transport::from_setting(social.tor_proxy.as_deref()).map_err(|e| anyhow!(e))?;
    let rt = tokio::runtime::Runtime::new()?;

    println!("location lab for #{cell}");
    println!(
        "  phones list you as anon#{} until you post in #{cell}, then as {}#{}",
        suffix(&pubkey),
        identity.nickname,
        suffix(&pubkey)
    );
    println!("  relays (same 5 as iOS): {}", ios_relays_for(&cell).join(", "));
    let heartbeat = match transport {
        Transport::Tor(proxy) => {
            if !rt.block_on(socks_ready(proxy)) {
                bail!("tor is on in settings but nothing answers at {proxy}; start Tor Browser or the tor service");
            }
            println!("  route: tor via {proxy}; sending one heartbeat per relay, like the GUI");
            true
        }
        Transport::Direct => {
            println!(
                "  route: direct. tor is off, so the GUI sends no heartbeats and phones only see you after you post."
            );
            println!("  listening only.");
            false
        }
    };
    println!("  listening {}s; post or open #{cell} on the phones now.\n", listen.as_secs());

    let tallies = rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let probe = tokio::spawn({
            let cell = cell.clone();
            async move { geo_probe::probe(&cell, keys, transport, heartbeat, listen, tx).await }
        });
        let mut tallies: BTreeMap<String, RelayTally> = BTreeMap::new();
        while let Some(line) = rx.recv().await {
            print_line(&line);
            record(&mut tallies, line);
        }
        let _ = probe.await;
        tallies
    });
    print_summary(&tallies, heartbeat);
    Ok(())
}

fn suffix(pubkey: &str) -> &str {
    &pubkey[pubkey.len().saturating_sub(4)..]
}

fn print_line(line: &ProbeLine) {
    match line {
        ProbeLine::Connected { relay, result: Ok(()) } => println!("+ {relay}: connected"),
        ProbeLine::Connected { relay, result: Err(why) } => println!("! {relay}: unreachable ({why})"),
        ProbeLine::Heartbeat { relay, result: Ok(()) } => println!("+ {relay}: heartbeat accepted"),
        ProbeLine::Heartbeat { relay, result: Err(why) } => println!("! {relay}: heartbeat refused ({why})"),
        ProbeLine::Seen { relay, message, mine } => {
            let kind = if message.presence { "presence" } else { "message" };
            let who = if *mine { " (you)" } else { "" };
            println!("  {relay}: {kind} from {}{who}", message.nickname);
        }
    }
}

fn record(tallies: &mut BTreeMap<String, RelayTally>, line: ProbeLine) {
    match line {
        ProbeLine::Connected { relay, result } => tallies.entry(relay).or_default().connected = Some(result),
        ProbeLine::Heartbeat { relay, result } => tallies.entry(relay).or_default().heartbeat = Some(result),
        ProbeLine::Seen { relay, message, mine } => {
            let tally = tallies.entry(relay).or_default();
            if mine {
                tally.echoed = true;
            } else {
                tally.others.insert(message.nickname);
            }
        }
    }
}

fn print_summary(tallies: &BTreeMap<String, RelayTally>, heartbeat: bool) {
    println!("\nsummary");
    let mut carrying = 0;
    for (relay, tally) in tallies {
        let reach = match &tally.connected {
            Some(Ok(())) => "connected",
            _ => "UNREACHABLE",
        };
        let beat = match (&tally.heartbeat, heartbeat) {
            (_, false) => "-",
            (Some(Ok(())), _) => "accepted",
            (Some(Err(_)), _) => "REFUSED",
            (None, _) => "not sent",
        };
        if tally.echoed {
            carrying += 1;
        }
        let others: Vec<&str> = tally.others.iter().map(String::as_str).collect();
        println!(
            "  {relay}: {reach}, heartbeat {beat}, delivered to a listener: {}, others: {}",
            if tally.echoed { "yes" } else { "no" },
            if others.is_empty() { "none".into() } else { others.join(" ") }
        );
    }
    if heartbeat {
        println!("  {carrying} of {} relays delivered your heartbeat to a separate listener.", tallies.len());
        if carrying == 0 {
            println!("  no relay carries you: phones in this cell cannot list you.");
        }
    }
}
