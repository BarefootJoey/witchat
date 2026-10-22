//! Phones Windows still held connected when witchat last quit. Those
//! leftover connections keep phones from reconnecting properly, so the next
//! start checks them before advertising and offers "reset Bluetooth" at once
//! instead of after attach timeouts.

use std::fs;
use std::path::{Path, PathBuf};

const FILE: &str = "ble-held.txt";

pub const HINT: &str = "Bluetooth on this PC still holds a connection from the last witchat, so phones may not \
                        reconnect properly. Press \"reset Bluetooth\" to turn it off and on and restart witchat.";

fn path() -> Option<PathBuf> {
    crate::app_dirs::data_local_dir().map(|d| d.join(FILE))
}

/// Remember the phones still held at exit; an empty list clears the file.
pub fn save(phones: &[String]) {
    let Some(path) = path() else { return };
    if let Err(err) = save_to(&path, phones) {
        tracing::warn!(%err, "could not record Bluetooth connections held at exit");
    }
}

fn save_to(path: &Path, phones: &[String]) -> std::io::Result<()> {
    if phones.is_empty() {
        return match fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, phones.join("\n"))
}

/// The phones recorded at the last exit, forgetting them.
pub fn take() -> Vec<String> {
    path().map(|p| take_from(&p)).unwrap_or_default()
}

fn take_from(path: &Path) -> Vec<String> {
    let phones = fs::read_to_string(path).unwrap_or_default();
    let _ = fs::remove_file(path);
    phones.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// The recorded phones Windows still reports connected right now.
pub async fn still_held() -> Vec<String> {
    let phones = take();
    if phones.is_empty() {
        return phones;
    }
    tokio::task::spawn_blocking(move || {
        phones.into_iter().filter(|p| witchat_ble::radio::os_connected(p) == Some(true)).collect()
    })
    .await
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_phones_are_read_once_and_an_empty_save_clears() {
        let dir = std::env::temp_dir().join(format!("witchat-ble-held-{}", std::process::id()));
        let path = dir.join(FILE);
        save_to(&path, &["AA:BB:CC:DD:EE:FF".into(), "11:22:33:44:55:66".into()]).unwrap();
        assert_eq!(take_from(&path), ["AA:BB:CC:DD:EE:FF", "11:22:33:44:55:66"]);
        assert!(take_from(&path).is_empty(), "taken once");
        save_to(&path, &["AA:BB:CC:DD:EE:FF".into()]).unwrap();
        save_to(&path, &[]).unwrap();
        assert!(!path.exists());
        save_to(&path, &[]).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
