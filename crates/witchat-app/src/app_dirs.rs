//! Where witchat keeps its files. Same paths `directories::ProjectDirs::from
//! ("org", "witchat", "witchat")` produced, so existing installs keep their
//! identity, favorites and logs; replaced because it pulled in an MPL-2.0
//! crate.
//!
//! Windows: `%APPDATA%\witchat\witchat\data` (data) and
//! `%LOCALAPPDATA%\witchat\witchat\data` (logs), via the Known Folder API so
//! redirected folders are honored. Linux: `$XDG_DATA_HOME/witchat`, falling
//! back to `~/.local/share/witchat`, for both.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static PROFILE: OnceLock<&'static str> = OnceLock::new();

/// Keep this run's files in a subfolder, as a separate node with its own
/// identity. Must be called before anything reads a path.
pub fn use_profile(name: &'static str) {
    let _ = PROFILE.set(name);
}

fn in_profile(dir: PathBuf) -> PathBuf {
    match PROFILE.get() {
        Some(name) => dir.join(name),
        None => dir,
    }
}

/// Roaming per-user data: identity, favorites, history, media ledger.
pub fn data_dir() -> Option<PathBuf> {
    imp::data_dir().map(in_profile)
}

/// Machine-local per-user data: rotating logs.
pub fn data_local_dir() -> Option<PathBuf> {
    imp::data_local_dir().map(in_profile)
}

/// Machine-local per-user data shared by every profile.
pub fn shared_local_dir() -> Option<PathBuf> {
    imp::data_local_dir()
}

/// The user's Downloads folder, if the OS names one.
pub fn download_dir() -> Option<PathBuf> {
    imp::download_dir()
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    use windows::core::GUID;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_Downloads, FOLDERID_LocalAppData, FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
    };

    const PROJECT: &str = r"witchat\witchat\data";

    pub fn data_dir() -> Option<PathBuf> {
        known_folder(&FOLDERID_RoamingAppData).map(|p| p.join(PROJECT))
    }

    pub fn data_local_dir() -> Option<PathBuf> {
        known_folder(&FOLDERID_LocalAppData).map(|p| p.join(PROJECT))
    }

    pub fn download_dir() -> Option<PathBuf> {
        known_folder(&FOLDERID_Downloads)
    }

    fn known_folder(id: &GUID) -> Option<PathBuf> {
        // SAFETY: on success the shell returns a NUL-terminated wide string
        // we own; it is copied before CoTaskMemFree releases it.
        unsafe {
            let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
            let path = OsString::from_wide(raw.as_wide());
            CoTaskMemFree(Some(raw.0 as *const _));
            Some(PathBuf::from(path))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;

    pub fn data_dir() -> Option<PathBuf> {
        let base = super::absolute_env("XDG_DATA_HOME").or_else(|| home().map(|h| h.join(".local/share")))?;
        Some(base.join("witchat"))
    }

    pub fn data_local_dir() -> Option<PathBuf> {
        data_dir()
    }

    pub fn download_dir() -> Option<PathBuf> {
        let home = home()?;
        let config = super::absolute_env("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let text = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
        super::user_dir(&text, "XDG_DOWNLOAD_DIR", &home)
    }

    fn home() -> Option<PathBuf> {
        super::absolute_env("HOME")
    }
}

#[cfg_attr(windows, allow(dead_code))]
fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// One entry of `user-dirs.dirs` (xdg-user-dirs). Only `"$HOME/…"` and
/// absolute values count; an entry equal to `$HOME` means "not set".
#[cfg_attr(windows, allow(dead_code))]
fn user_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let value = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix(key)?.trim_start().strip_prefix('='))?
        .trim()
        .strip_prefix('"')?
        .strip_suffix('"')?;
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None if value.starts_with('/') => PathBuf::from(value),
        None => return None,
    };
    (path != home).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dirs_entries_resolve_like_xdg() {
        let home = Path::new("/home/ada");
        let text = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Téléchargements\"\n";
        assert_eq!(user_dir(text, "XDG_DOWNLOAD_DIR", home), Some(home.join("Téléchargements")));
        assert_eq!(user_dir("XDG_DOWNLOAD_DIR=\"/mnt/dl\"", "XDG_DOWNLOAD_DIR", home), Some("/mnt/dl".into()));
        assert_eq!(user_dir("XDG_DOWNLOAD_DIR=\"$HOME/\"", "XDG_DOWNLOAD_DIR", home), None);
        assert_eq!(user_dir("XDG_DOWNLOAD_DIR=\"relative\"", "XDG_DOWNLOAD_DIR", home), None);
        assert_eq!(user_dir("#XDG_DOWNLOAD_DIR=\"$HOME/x\"", "XDG_DOWNLOAD_DIR", home), None);
        assert_eq!(user_dir(text, "XDG_MUSIC_DIR", home), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_match_the_old_project_dirs_layout() {
        let roaming = std::env::var_os("APPDATA").map(PathBuf::from).unwrap();
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap();
        assert_eq!(data_dir(), Some(roaming.join(r"witchat\witchat\data")));
        assert_eq!(data_local_dir(), Some(local.join(r"witchat\witchat\data")));
        assert!(download_dir().is_some());
    }
}
