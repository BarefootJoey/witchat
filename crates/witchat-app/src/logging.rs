//! Rotating file logs for lab debugging.
//!
//! Directory: `%LOCALAPPDATA%\witchat\witchat\data\logs` on Windows
//! (`app_dirs::data_local_dir` + `logs`). Daily files, keep 7. No secrets.
//!
//! Quiet by default (state changes and one health line a minute); the
//! "verbose logging" setting adds per-packet and per-advertisement detail.
//! `RUST_LOG` wins over both and pins the filter.

use crate::app_dirs;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, reload, EnvFilter, Registry};

/// `nostr_sdk::relay` logs every reconnect attempt at error; relay health
/// (`witchat_nostr::relay_health`) logs each relay's state changes instead.
const QUIET: &str = "warn,wgpu_hal=error,nostr_sdk::relay=off,witchat=info,witchat_ble=info,witchat_core=info,\
                     witchat_nostr=info,witchat_crypto=info";
const VERBOSE: &str = "info,witchat=debug,witchat_ble=trace,witchat_core=debug,witchat_nostr=debug,\
                       witchat_protocol=debug,witchat_crypto=debug";

static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();
static GUARD: OnceLock<WorkerGuard> = OnceLock::new();
static RELOAD: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();
static PINNED: AtomicBool = AtomicBool::new(false);
static VERBOSE_ON: AtomicBool = AtomicBool::new(false);

pub fn log_dir() -> Option<PathBuf> {
    LOG_DIR.get().cloned()
}

/// Where logs go, whether or not logging started in this process.
pub fn dir_path() -> Option<PathBuf> {
    log_dir().or_else(|| app_dirs::data_local_dir().map(|d| d.join("logs")))
}

/// Log files, newest first.
pub fn files_newest_first() -> Vec<PathBuf> {
    let Some(dir) = dir_path() else { return Vec::new() };
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("witchat.")))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files.reverse();
    files
}

/// `YYYY-MM-DD HH:MM UTC`.
pub fn utc_stamp(now: std::time::SystemTime) -> String {
    let secs = now.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format!("{} {:02}:{:02} UTC", chrono_day(now), secs % 86_400 / 3600, secs % 3600 / 60)
}

pub fn verbose() -> bool {
    VERBOSE_ON.load(Ordering::Relaxed)
}

/// True when `RUST_LOG` chose the filter; the setting then has no effect.
pub fn pinned() -> bool {
    PINNED.load(Ordering::Relaxed)
}

pub fn set_verbose(on: bool) {
    VERBOSE_ON.store(on, Ordering::Relaxed);
    if pinned() {
        return;
    }
    if let Some(handle) = RELOAD.get() {
        if let Err(err) = handle.reload(EnvFilter::new(if on { VERBOSE } else { QUIET })) {
            tracing::warn!(%err, "log filter not changed");
            return;
        }
        tracing::info!(verbose = on, "log detail changed");
    }
}

pub fn init(verbose: bool) -> Result<PathBuf> {
    let dir = app_dirs::data_local_dir().context("no per-user data folder")?.join("logs");
    fs::create_dir_all(&dir)?;

    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("witchat")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)
        .context("log appender")?;
    let (file_writer, guard) = tracing_appender::non_blocking(appender);
    let _ = GUARD.set(guard);
    let _ = LOG_DIR.set(dir.clone());

    VERBOSE_ON.store(verbose, Ordering::Relaxed);
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => {
            PINNED.store(true, Ordering::Relaxed);
            filter
        }
        Err(_) => EnvFilter::new(if verbose { VERBOSE } else { QUIET }),
    };
    let (filter, handle) = reload::Layer::new(filter);
    let _ = RELOAD.set(handle);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true))
        .with(fmt::layer().with_ansi(false).with_target(true).with_writer(file_writer))
        .init();

    tracing::info!(path = %dir.display(), verbose, pinned = pinned(), "file logging started (daily rotate, 7 files)");
    Ok(dir)
}

/// `YYYY-MM-DD` in UTC, as `tracing_appender` names daily files.
fn chrono_day(now: std::time::SystemTime) -> String {
    let secs = now.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days-to-civil conversion.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn names_files_like_the_appender() {
        assert_eq!(chrono_day(UNIX_EPOCH), "1970-01-01");
        assert_eq!(chrono_day(UNIX_EPOCH + Duration::from_secs(1_792_281_600)), "2026-10-18");
        assert_eq!(chrono_day(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29");
        assert_eq!(utc_stamp(UNIX_EPOCH + Duration::from_secs(1_792_281_600 + 3_725)), "2026-10-18 01:02 UTC");
    }

    #[test]
    fn quiet_logs_drop_relay_reconnect_errors_but_keep_relay_health() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::layer::{Context, Layer};

        struct Targets(Arc<Mutex<Vec<String>>>);
        impl<S: tracing::Subscriber> Layer<S> for Targets {
            fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
                self.0.lock().unwrap().push(event.metadata().target().to_string());
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(EnvFilter::new(QUIET)).with(Targets(seen.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: "nostr_sdk::relay::inner", "Connection failed.");
            tracing::warn!(target: "nostr_sdk::client", "kept");
            tracing::info!(target: "witchat_nostr::relay_health", "relay: failing");
        });
        assert_eq!(*seen.lock().unwrap(), ["nostr_sdk::client", "witchat_nostr::relay_health"]);
    }
}
