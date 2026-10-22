//! Keeps the location relay directory as fresh as the phones': the cached
//! download is loaded before any channel joins, and a background task
//! refetches it once a day over the current relay route (Tor when on, so a
//! Tor that is down just means "retry later", never a direct request).

use crate::app_dirs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::task::JoinHandle;
use witchat_nostr::relay_directory::{self, CHECK_INTERVAL, FETCH_INTERVAL, RETRY_INITIAL, RETRY_MAX};
use witchat_nostr::Transport;

fn cache_path() -> Option<PathBuf> {
    app_dirs::data_local_dir().map(|d| d.join("relay_directory.csv"))
}

/// Use the last download, if any; call before joining channels.
pub fn load_cached() {
    if let Some(path) = cache_path() {
        relay_directory::load_cache(&path);
    }
}

/// Panic wipe: the download says when witchat last ran.
pub fn forget_cache() {
    if let Some(path) = cache_path() {
        let _ = std::fs::remove_file(path);
    }
}

pub fn spawn(transport: Transport) -> JoinHandle<()> {
    tokio::spawn(async move {
        let path = cache_path();
        let mut retry = RETRY_INITIAL;
        loop {
            let wait = if is_due(path.as_deref(), SystemTime::now()) {
                match relay_directory::refresh(transport, path.as_deref()).await {
                    Ok(count) => {
                        tracing::info!(relays = count, tor = transport.is_tor(), "geo: relay directory refreshed");
                        retry = RETRY_INITIAL;
                        CHECK_INTERVAL
                    }
                    Err(err) => {
                        tracing::warn!(%err, retry_secs = retry.as_secs(), "geo: relay directory refresh failed");
                        let wait = retry;
                        retry = (retry * 2).min(RETRY_MAX);
                        wait
                    }
                }
            } else {
                CHECK_INTERVAL
            };
            tokio::time::sleep(wait).await;
        }
    })
}

/// Due when there is no download yet or it is a day old.
fn is_due(path: Option<&Path>, now: SystemTime) -> bool {
    let Some(modified) = path.and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok()) else {
        return true;
    };
    now.duration_since(modified).unwrap_or(Duration::ZERO) >= FETCH_INTERVAL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_day_old_download_is_due() {
        let path = std::env::temp_dir().join(format!("witchat-relays-{}.csv", uuid::Uuid::new_v4()));
        assert!(is_due(Some(&path), SystemTime::now()));
        std::fs::write(&path, "x").unwrap();
        assert!(!is_due(Some(&path), SystemTime::now()));
        assert!(is_due(Some(&path), SystemTime::now() + FETCH_INTERVAL));
        assert!(is_due(None, SystemTime::now()));
        let _ = std::fs::remove_file(path);
    }
}
