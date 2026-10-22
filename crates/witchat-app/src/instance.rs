//! One Bluetooth-using witchat per user. Two would publish the same GATT
//! service and connect to the same phones; Windows then answers both with
//! "access denied" and links drop. The window app, `--relay` and `--scan`
//! claim this lock in the folder every profile shares. A second window app
//! asks the first to show itself instead of starting.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const LOCK: &str = "witchat.lock";
const OWNER: &str = "witchat.owner";
const SHOW: &str = "show-request";

/// Held for the life of the process; the OS releases it on exit or crash.
pub struct InstanceLock {
    _file: Option<File>,
}

impl InstanceLock {
    /// Run without the guard when the lock file cannot be opened.
    pub fn unheld() -> Self {
        Self { _file: None }
    }
}

pub enum Claim {
    Ours(InstanceLock),
    /// Another witchat holds the radio; `owner` is the mode it wrote.
    Taken {
        owner: String,
    },
}

fn dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(crate::app_dirs::shared_local_dir).as_deref()
}

pub fn claim(mode: &str) -> io::Result<Claim> {
    let dir = dir().ok_or_else(|| io::Error::other("no per-user data folder"))?;
    claim_in(dir, mode)
}

fn claim_in(dir: &Path, mode: &str) -> io::Result<Claim> {
    fs::create_dir_all(dir)?;
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let owner = fs::read_to_string(dir.join(OWNER)).unwrap_or_default().trim().to_string();
            return Ok(Claim::Taken { owner });
        }
        Err(TryLockError::Error(err)) => return Err(err),
    }
    fs::write(dir.join(OWNER), mode)?;
    let _ = fs::remove_file(dir.join(SHOW));
    Ok(Claim::Ours(InstanceLock { _file: Some(file) }))
}

/// Whether a Bluetooth-using witchat holds the lock, without claiming it.
pub fn is_running() -> bool {
    dir().is_some_and(running_in)
}

fn running_in(dir: &Path) -> bool {
    let Ok(file) = OpenOptions::new().write(true).open(dir.join(LOCK)) else { return false };
    matches!(file.try_lock(), Err(TryLockError::WouldBlock))
}

/// What a running `owner` is, for the "already running" message.
pub fn describe(owner: &str) -> &'static str {
    match owner {
        "gui" => "the witchat window (it may be hidden in the notification area)",
        "relay" => "a headless relay (witchat --relay)",
        "scan" => "witchat --scan in a terminal",
        _ => "another copy of witchat",
    }
}

/// Ask the running window app to come to the front.
pub fn request_show() -> io::Result<()> {
    let dir = dir().ok_or_else(|| io::Error::other("no per-user data folder"))?;
    fs::write(dir.join(SHOW), b"")
}

/// True once per [`request_show`] from another process.
pub fn show_requested() -> bool {
    dir().is_some_and(take_show_in)
}

fn take_show_in(dir: &Path) -> bool {
    fs::remove_file(dir.join(SHOW)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("witchat-instance-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_second_claim_sees_the_first_owner_until_it_exits() {
        let dir = scratch("claim");
        let Claim::Ours(first) = claim_in(&dir, "relay").unwrap() else { panic!("first claim refused") };
        match claim_in(&dir, "gui").unwrap() {
            Claim::Taken { owner } => assert_eq!(owner, "relay"),
            Claim::Ours(_) => panic!("two holders"),
        }
        assert!(running_in(&dir));
        drop(first);
        assert!(!running_in(&dir));
        assert!(matches!(claim_in(&dir, "gui").unwrap(), Claim::Ours(_)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn show_requests_are_taken_once_and_cleared_on_claim() {
        let dir = scratch("show");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(SHOW), b"").unwrap();
        let Claim::Ours(lock) = claim_in(&dir, "gui").unwrap() else { panic!("claim refused") };
        assert!(!take_show_in(&dir), "stale request from before start");
        fs::write(dir.join(SHOW), b"").unwrap();
        assert!(take_show_in(&dir));
        assert!(!take_show_in(&dir));
        drop(lock);
        let _ = fs::remove_dir_all(&dir);
    }
}
