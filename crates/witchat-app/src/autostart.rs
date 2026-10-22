//! Start at login, hidden in the tray, so the PC keeps relaying for phones
//! after a reboot. The OS entry (Windows `Run` value, XDG autostart file) is
//! the only record: deleting it by hand turns the setting off.

use std::path::{Path, PathBuf};

/// Hidden flag: start with the window hidden when a tray icon can bring it back.
pub const BACKGROUND_ARG: &str = "--background";

pub fn supported() -> bool {
    cfg!(any(windows, target_os = "linux"))
}

pub fn enabled() -> bool {
    platform::read().is_some()
}

pub fn set(on: bool) -> Result<(), String> {
    if on {
        platform::write(&current_exe()?)
    } else {
        platform::remove()
    }
}

/// A moved portable folder leaves the entry pointing at the old copy.
pub fn refresh() {
    let Some(existing) = platform::read() else { return };
    let Ok(exe) = current_exe() else { return };
    if existing == platform::render(&exe) {
        return;
    }
    match platform::write(&exe) {
        Ok(()) => tracing::info!("autostart: login entry now points at this copy"),
        Err(err) => tracing::warn!(%err, "autostart: could not update the login entry"),
    }
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("could not find the witchat program: {e}"))
}

#[cfg_attr(not(windows), allow(dead_code))]
fn run_command(exe: &Path) -> String {
    format!("\"{}\" {BACKGROUND_ARG}", exe.display())
}

/// XDG desktop-entry `Exec=` value: quoted per the spec, then string-escaped.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn desktop_exec(exe: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in exe.display().to_string().chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    format!("{} {BACKGROUND_ARG}", quoted.replace('\\', "\\\\").replace('%', "%%"))
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=witchat\nComment=BitChat-compatible mesh chat\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        desktop_exec(exe)
    )
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn exec_line(entry: &str) -> Option<String> {
    entry.lines().find_map(|l| l.strip_prefix("Exec=")).map(str::to_string)
}

#[cfg(windows)]
mod platform {
    use std::path::Path;
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };

    const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const NAME: PCWSTR = w!("witchat");

    pub(super) fn render(exe: &Path) -> String {
        super::run_command(exe)
    }

    pub(super) fn read() -> Option<String> {
        let mut len = 0u32;
        let sized = unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN, NAME, RRF_RT_REG_SZ, None, None, Some(&mut len)) };
        if sized.is_err() || len < 2 {
            return None;
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2)];
        let got = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN,
                NAME,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            )
        };
        if got.is_err() {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }

    pub(super) fn write(exe: &Path) -> Result<(), String> {
        let value = HSTRING::from(render(exe));
        let bytes = (value.len() as u32 + 1) * 2;
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, RUN, NAME, REG_SZ.0, Some(value.as_ptr().cast()), bytes) }
            .ok()
            .map_err(|e| format!("could not add witchat to startup apps: {}", e.message()))
    }

    pub(super) fn remove() -> Result<(), String> {
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, NAME) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        status.ok().map_err(|e| format!("could not remove witchat from startup apps: {}", e.message()))
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::{Path, PathBuf};

    fn entry_path() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(config.join("autostart").join("witchat.desktop"))
    }

    pub(super) fn render(exe: &Path) -> String {
        super::desktop_exec(exe)
    }

    pub(super) fn read() -> Option<String> {
        super::exec_line(&std::fs::read_to_string(entry_path()?).ok()?)
    }

    pub(super) fn write(exe: &Path) -> Result<(), String> {
        let path = entry_path().ok_or("no home directory for the autostart entry")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, super::desktop_entry(exe)).map_err(|e| format!("could not write {}: {e}", path.display()))
    }

    pub(super) fn remove() -> Result<(), String> {
        let Some(path) = entry_path() else { return Ok(()) };
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not remove {}: {e}", path.display())),
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use std::path::Path;

    pub(super) fn render(_exe: &Path) -> String {
        String::new()
    }

    pub(super) fn read() -> Option<String> {
        None
    }

    pub(super) fn write(_exe: &Path) -> Result<(), String> {
        Err("start at login is not supported on this system".into())
    }

    pub(super) fn remove() -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_run_value_quotes_the_path() {
        let exe = Path::new(r"C:\Program Files\witchat\witchat.exe");
        assert_eq!(run_command(exe), r#""C:\Program Files\witchat\witchat.exe" --background"#);
    }

    #[test]
    fn desktop_exec_escapes_per_xdg() {
        assert_eq!(desktop_exec(Path::new("/opt/wit chat/witchat")), "\"/opt/wit chat/witchat\" --background");
        assert_eq!(desktop_exec(Path::new("/tmp/a$b\"c%d")), "\"/tmp/a\\\\$b\\\\\"c%%d\" --background");
    }

    /// Touches the real login entry and restores it:
    /// `cargo test -p witchat-app -- --ignored login_entry`.
    #[test]
    #[ignore]
    fn login_entry_round_trip() {
        let before = enabled();
        set(true).unwrap();
        assert!(enabled());
        assert_eq!(platform::read(), Some(platform::render(&std::env::current_exe().unwrap())));
        set(false).unwrap();
        assert!(!enabled());
        set(false).expect("removing a missing entry is fine");
        if before {
            set(true).unwrap();
        }
    }

    #[test]
    fn desktop_entry_round_trips_its_exec_line() {
        let exe = Path::new("/home/ada/apps/witchat");
        let entry = desktop_entry(exe);
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert_eq!(exec_line(&entry).as_deref(), Some(desktop_exec(exe).as_str()));
        assert_eq!(exec_line("[Desktop Entry]\nName=x\n"), None);
    }
}
