//! `witchat --relay`: the mesh worker with no window, for a PC or Linux box
//! left on to relay. It runs as its own node (identity, settings and logs in
//! the `relay` profile folder), so private messages to the user's identity
//! never reach a process nobody reads. It logs what it does for the mesh as
//! counts, never message text. Ctrl-C or the service manager stops it; files
//! are saved as they change, and the OS drops the stay-awake request on exit.

use crate::mesh_worker::{self, Command, UiEvent};
use crate::worker_diag::DiagCommand;
use anyhow::Result;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};
use witchat_nostr::Transport;

pub const PROFILE: &str = "relay";
const REPORT_EVERY: Duration = Duration::from_secs(15 * 60);
const FIRST_REPORT: Duration = Duration::from_secs(60);

pub fn run(radio: mesh_worker::Radio, nickname: Option<String>, stay_awake: bool) -> Result<()> {
    let settings =
        crate::social::Social::default_path().and_then(|p| crate::social::Social::load(&p).ok()).unwrap_or_default();
    let transport = Transport::from_setting(settings.tor_proxy.as_deref()).unwrap_or_else(|err| {
        tracing::warn!(%err, "relay: tor setting ignored");
        Transport::Direct
    });
    let handle = mesh_worker::spawn(radio, nickname, transport)?;
    let _ = handle.tx.send(Command::StayAwake(stay_awake));
    let _ = handle.tx.send(Command::Relays(settings.relays.clone()));
    say("relaying for the mesh with no window; Ctrl-C stops it");
    if let Some(dir) = crate::app_dirs::data_dir() {
        say(&format!("relay identity and settings: {}", dir.display()));
    }
    let mut next_report = Instant::now() + FIRST_REPORT;
    loop {
        match handle.rx.recv_timeout(Duration::from_secs(1)) {
            Ok(UiEvent::NeedsReset(hint)) => {
                tracing::warn!(target: "witchat::relay", "{hint}");
                if auto_reset() {
                    say("nobody is at this relay to press reset; turning Bluetooth off and on");
                    let _ = handle.tx.send(Command::ResetBluetooth);
                }
            }
            Ok(UiEvent::Relaunch) => {
                crate::relaunch::spawn()?;
                say("Bluetooth was reset; a fresh relay takes over");
                return Ok(());
            }
            Ok(event) => on_event(event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("the mesh worker stopped"),
        }
        if Instant::now() >= next_report {
            next_report = Instant::now() + REPORT_EVERY;
            let _ = handle.tx.send(Command::Diag(DiagCommand::Once));
        }
    }
}

/// Windows only: a Linux relay usually runs under systemd, which would stop
/// the relaunched copy along with this one. A relaunched relay never resets
/// again, so a radio that stays stuck cannot loop.
fn auto_reset() -> bool {
    cfg!(windows) && !crate::relaunch::is_relaunch()
}

fn on_event(event: UiEvent) {
    match event {
        UiEvent::Status(s) => say(&s),
        UiEvent::Error(s) => tracing::warn!(target: "witchat::relay", "{s}"),
        UiEvent::Me { id, .. } => say(&format!("relay node {}", &id[..id.len().min(8)])),
        UiEvent::Diag(snap) => {
            for (label, value) in snap.contribution_lines() {
                say(&format!("{label}: {value}"));
            }
        }
        _ => {}
    }
}

/// The log already mirrors to the console.
fn say(line: &str) {
    tracing::info!(target: "witchat::relay", "{line}");
}
