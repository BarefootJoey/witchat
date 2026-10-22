//! Terminal front ends: the `--scan` BLE interop spike, `--geohash` Nostr
//! chat and the `--diag` report.

use crate::mesh_worker::{self, Delivery, UiEvent};
use anyhow::Result;
use std::io::{self, BufRead, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use witchat_nostr::GeoChannel;

pub fn run_scan_cli(radio: mesh_worker::Radio, nickname: Option<String>) -> Result<()> {
    let handle = mesh_worker::spawn(radio, nickname, witchat_nostr::Transport::Direct)?;
    println!("witchat BLE interop spike");
    println!("Looking for official bitchat phones (mainnet UUID F47B5E2D-...4B5C).");
    println!("Type a message and press Enter. /quit to exit. /who lists peers.");
    let lines = stdin_lines();
    loop {
        while let Ok(event) = handle.rx.try_recv() {
            print_event(&event);
        }
        if let Ok(line) = lines.try_recv() {
            let line = line.trim().to_string();
            if line == "/quit" {
                handle.stop();
                break;
            }
            if line == "/reset" {
                let _ = handle.tx.send(mesh_worker::Command::ResetBluetooth);
                continue;
            }
            if let Some(cmd) = mesh_worker::parse_command_line(&line) {
                let _ = handle.tx.send(cmd);
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

pub fn run_diag() -> Result<()> {
    let adapter = tokio::runtime::Runtime::new()?.block_on(witchat_ble::probe_adapter());
    println!("{}", crate::diag::report::offline(adapter));
    Ok(())
}

pub fn run_geohash_cli(cell: String, nickname: Option<String>) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let nick = nickname.unwrap_or_else(witchat_crypto::generate_default_nickname);
        let channel =
            GeoChannel::connect(&cell, witchat_nostr::Keys::generate(), witchat_nostr::Transport::Direct).await?;
        let mut rx = channel.subscribe().await?;
        println!("joined geohash #{cell} as {nick}. Type to post. /quit exits.");
        let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel();
        thread::spawn(move || {
            for line in io::stdin().lock().lines().map_while(Result::ok) {
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });
        loop {
            tokio::select! {
                Some(msg) = rx.recv() => {
                    println!("[{}] {}", msg.nickname, msg.content);
                }
                Some(line) = line_rx.recv() => {
                    if line.trim() == "/quit" {
                        break;
                    }
                    // The CLI never takes a location fix.
                    channel.publish(line.trim(), &nick, true).await?;
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(())
}

fn stdin_lines() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn print_event(event: &UiEvent) {
    match event {
        UiEvent::Status(s) => println!("* {s}"),
        UiEvent::Radio { .. } => {} // the status line right after names the role
        UiEvent::Error(s) => eprintln!("! {s}"),
        UiEvent::NeedsReset(s) => eprintln!("! {s} Here: type /reset."),
        UiEvent::Relaunch => println!("* Bluetooth was turned off and on; /quit and run witchat --scan again"),
        UiEvent::Me { nick, fingerprint, .. } => println!("* you are {nick}, fingerprint {fingerprint}"),
        UiEvent::Peer { id, nick } => println!("* peer {nick} ({id})"),
        UiEvent::Known { id, nick } => println!("* seen recently {nick} ({id})"),
        UiEvent::PeerLost(id) => println!("* lost {id}"),
        UiEvent::Fingerprint { id, fingerprint } => println!("* {id} fingerprint {fingerprint}"),
        UiEvent::Authenticated { id, ok } => println!("* {id} signing key {}", if *ok { "bound" } else { "MISMATCH" }),
        UiEvent::Favorite { id, on, .. } => println!("* {} favorite {id}", if *on { "added" } else { "removed" }),
        UiEvent::FavoritedBy { id, on, .. } => println!("* {id} {} you", if *on { "favorited" } else { "unfavorited" }),
        UiEvent::Linking { name, .. } => println!("* radio saw {name}"),
        UiEvent::LinkLost(id) => println!("* link lost {id}"),
        UiEvent::Session { id, state } => println!("* {id}: {state}"),
        UiEvent::Receipt { message_id, state } => {
            let label = match state {
                Delivery::Delivered => "delivered",
                Delivery::Read => "read",
                Delivery::Failed => "NOT delivered",
                Delivery::Queued => "queued until they're back",
                Delivery::Couriered { forward_secret: true } => "with couriers (forward secret)",
                Delivery::Couriered { forward_secret: false } => "with couriers",
            };
            println!("* {label} {message_id}");
        }
        UiEvent::Line(line) => {
            let flag = if line.is_private { "DM" } else { "#" };
            println!("[{flag}] {}: {}", line.from, line.content);
        }
        UiEvent::Progress { id, percent } => println!("* sending {id}: {percent}%"),
        UiEvent::Talking { id, on, public } => {
            let place = if *public { " in #mesh" } else { "" };
            println!("* {id} {}{place}", if *on { "is talking (live)" } else { "stopped talking" })
        }
        UiEvent::MyQr(url) => println!("* verify me: {url}"),
        UiEvent::QrVerified { id, .. } => println!("* verified {id} by QR"),
        UiEvent::QrRequested { nick, .. } => println!("* verification requested for {nick}"),
        UiEvent::QrRejected(err) => println!("! {err}"),
        UiEvent::QrChallenged { id, .. } => println!("* {id} scanned your QR; answered its identity check"),
        UiEvent::Tor(health) => println!(
            "* tor {}",
            match health {
                witchat_nostr::TorHealth::Down => "proxy not answering",
                witchat_nostr::TorHealth::Building => "building circuits; relays wait",
                witchat_nostr::TorHealth::Ready => "ready",
            }
        ),
        UiEvent::Diagnostic { text, .. } => println!("* {text}"),
        UiEvent::Diag(_) => {}
        UiEvent::DiagReport(text) => println!("{text}"),
        UiEvent::QrMismatch(id) => println!("! {id} does NOT hold the signing key in that QR; not verified"),
        UiEvent::File { line, path, .. } => println!("[file] {}: {} ({})", line.from, line.content, path.display()),
        UiEvent::GeoLine { geohash, line } => println!("[#{geohash}] {}: {}", line.from, line.content),
        UiEvent::GeoDm { cell, line, .. } => println!("[#{cell} DM] {}: {}", line.from, line.content),
        UiEvent::GeoJoined(cell) => println!("* joined #{cell}"),
        UiEvent::GeoSeen { .. } => {}
        UiEvent::GeoLeft(cell) => println!("* left #{cell}"),
    }
    let _ = io::stdout().flush();
}
