mod app_dirs;
mod autostart;
mod ble_held;
mod ble_links;
mod camera;
mod carried;
mod cli;
mod commands;
mod console_stop;
mod diag;
mod diagnostics;
mod fanout;
mod geo_dm;
mod geo_lab;
mod geo_link;
mod history;
mod identity_store;
mod inbox;
mod instance;
mod key_protect;
mod ledger;
mod link_bindings;
mod locate;
mod logging;
mod media;
mod mesh_worker;
mod nostr_link;
mod notify;
mod outbound;
mod outbox;
mod pay;
mod peer_keys;
mod power;
mod prekey_file;
mod qr_image;
mod qr_lab;
mod recorder;
mod relaunch;
mod relay_mode;
mod relay_refresh;
mod relay_schedule;
mod relay_spool;
mod roster;
mod social;
mod stay_awake;
mod theme;
mod tor_link;
mod ui;
mod voice;
mod wake;
mod worker_commands;
mod worker_diag;
mod worker_outbox;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "witchat", about = "Independent BitChat-compatible desktop client")]
struct Args {
    /// Terminal BLE interop spike instead of the GUI.
    #[arg(long)]
    scan: bool,

    /// Include the official debug/testnet BLE UUID.
    #[arg(long)]
    testnet: bool,

    /// Do not publish the GATT server; connect out to phones only.
    #[arg(long)]
    no_advertise: bool,

    /// Scan in windows as on battery, even when plugged in.
    #[arg(long)]
    low_power: bool,

    /// Join a Nostr geohash channel (internet fallback).
    #[arg(long)]
    geohash: Option<String>,

    /// Nickname override.
    #[arg(long)]
    nick: Option<String>,

    /// Swapped-key QR experiments: `mine`, or a phone's verify URL or QR picture.
    #[arg(long, value_name = "mine|URL|PICTURE")]
    qr_lab: Option<String>,

    /// Location-channel visibility check: heartbeat each of the cell's relays
    /// and list whom they fan out.
    #[arg(long, value_name = "GEOHASH")]
    geo_lab: Option<String>,

    /// How long `--geo-lab` listens.
    #[arg(long, value_name = "SECONDS", default_value_t = 120)]
    listen: u64,

    /// Start hidden in the tray (used by the start-at-login entry).
    #[arg(long, hide = true)]
    background: bool,

    /// Print a redacted diagnostics report (for bug reports) and exit.
    #[arg(long)]
    diag: bool,

    /// Show whether Windows still holds a connection to recently used
    /// phones (run with witchat closed) and exit.
    #[arg(long)]
    ble_links: bool,

    /// Relay for the mesh with no window, as a separate node with its own
    /// identity (for a PC or server left on).
    #[arg(long)]
    relay: bool,

    /// With --relay: let the PC sleep as usual. By default a relay holds
    /// off idle sleep while on mains power.
    #[arg(long, requires = "relay")]
    allow_sleep: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.relay {
        app_dirs::use_profile(relay_mode::PROFILE);
    }
    if args.diag {
        return cli::run_diag();
    }
    if args.ble_links {
        return ble_links::run();
    }
    let settings = social::Social::default_path().and_then(|path| social::Social::load(&path).ok()).unwrap_or_default();
    let log_dir = logging::init(settings.verbose_logs)?;
    tracing::info!(
        build = %diag::report::build_line(),
        log_dir = %log_dir.display(),
        mode = if args.relay {
            "relay"
        } else if args.scan {
            "scan"
        } else if args.geohash.is_some() {
            "geohash"
        } else {
            "gui"
        },
        testnet = args.testnet,
        advertise = !args.no_advertise,
        tor = settings.tor_proxy.is_some(),
        keep_history = settings.keep_history,
        key_protection = key_protect::Protection::current().describe(),
        renderer = %std::env::var("ICED_BACKEND").unwrap_or_else(|_| "default".into()),
        "witchat starting"
    );
    if let Some(source) = args.qr_lab {
        return qr_lab::run(&source);
    }
    if let Some(cell) = args.geo_lab {
        return geo_lab::run(&cell, std::time::Duration::from_secs(args.listen));
    }
    if let Some(geohash) = args.geohash {
        return cli::run_geohash_cli(geohash, args.nick);
    }
    let mode = if args.relay {
        "relay"
    } else if args.scan {
        "scan"
    } else {
        "gui"
    };
    let Some(_instance) = claim_radio(mode, args.background)? else {
        return Ok(());
    };
    let radio = mesh_worker::Radio { testnet: args.testnet, advertise: !args.no_advertise, low_power: args.low_power };
    if args.relay || args.scan {
        console_stop::install();
    }
    if args.relay {
        return relay_mode::run(radio, args.nick, !args.allow_sleep);
    }
    if args.scan {
        return cli::run_scan_cli(radio, args.nick);
    }
    autostart::refresh();
    ui::run_gui(radio, args.nick, args.background)
}

/// None when this run should stop because another witchat has the radio.
fn claim_radio(mode: &str, background: bool) -> Result<Option<instance::InstanceLock>> {
    let started = std::time::Instant::now();
    let owner = loop {
        match instance::claim(mode) {
            Ok(instance::Claim::Ours(lock)) => return Ok(Some(lock)),
            Ok(instance::Claim::Taken { .. }) if relaunch::is_relaunch() && started.elapsed() < relaunch::WAIT => {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Ok(instance::Claim::Taken { owner }) => break owner,
            Err(err) => {
                tracing::warn!(%err, "single-instance lock unavailable; starting anyway");
                return Ok(Some(instance::InstanceLock::unheld()));
            }
        }
    };
    if mode == "gui" && owner == "gui" {
        if !background {
            if let Err(err) = instance::request_show() {
                tracing::warn!(%err, "could not ask the running window to show");
            }
        }
        tracing::info!("witchat is already running; asked it to show its window");
        return Ok(None);
    }
    let message = format!(
        "{} is already using Bluetooth on this PC. Only one copy of witchat can use it at a time; stop that one first.",
        instance::describe(&owner)
    );
    if mode == "gui" {
        tracing::warn!("{message}");
        rfd::MessageDialog::new()
            .set_title("witchat")
            .set_description(&message)
            .set_level(rfd::MessageLevel::Warning)
            .show();
        return Ok(None);
    }
    anyhow::bail!(message)
}
