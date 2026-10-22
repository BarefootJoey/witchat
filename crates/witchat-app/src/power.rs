//! Whether this PC is running on battery, so the Bluetooth scan can rest
//! part of the time as phones do. Unknown counts as mains power: a desktop
//! with no battery must keep scanning continuously.

/// True when the OS reports the PC unplugged and running on battery.
pub fn on_battery() -> bool {
    imp::on_battery()
}

#[cfg(windows)]
mod imp {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    /// `ACLineStatus`: 0 offline, 1 online, 255 unknown.
    const AC_OFFLINE: u8 = 0;

    pub fn on_battery() -> bool {
        let mut status = SYSTEM_POWER_STATUS::default();
        // SAFETY: GetSystemPowerStatus only writes the struct we pass.
        unsafe { GetSystemPowerStatus(&mut status) }.is_ok() && status.ACLineStatus == AC_OFFLINE
    }
}

#[cfg(not(windows))]
mod imp {
    use std::fs;
    use std::path::Path;

    pub fn on_battery() -> bool {
        on_battery_in(Path::new("/sys/class/power_supply"))
    }

    /// A battery present and no mains or USB supply online.
    pub(super) fn on_battery_in(root: &Path) -> bool {
        let Ok(entries) = fs::read_dir(root) else { return false };
        let mut battery = false;
        for entry in entries.flatten() {
            let dir = entry.path();
            let read =
                |name: &str| fs::read_to_string(dir.join(name)).map(|s| s.trim().to_string()).unwrap_or_default();
            match read("type").as_str() {
                "Battery" => battery |= read("present") != "0",
                "Mains" | "USB" | "USB_C" | "USB_PD" if read("online") == "1" => return false,
                _ => {}
            }
        }
        battery
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::imp::on_battery_in;
    use std::fs;

    fn supply(root: &std::path::Path, name: &str, files: &[(&str, &str)]) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        for (file, value) in files {
            fs::write(dir.join(file), format!("{value}\n")).unwrap();
        }
    }

    #[test]
    fn battery_counts_only_when_nothing_is_plugged_in() {
        let root = std::env::temp_dir().join(format!("witchat-power-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        assert!(!on_battery_in(&root), "no power_supply dir: assume mains");
        supply(&root, "BAT0", &[("type", "Battery"), ("present", "1")]);
        supply(&root, "AC", &[("type", "Mains"), ("online", "0")]);
        assert!(on_battery_in(&root));
        supply(&root, "AC", &[("type", "Mains"), ("online", "1")]);
        assert!(!on_battery_in(&root));
        let _ = fs::remove_dir_all(&root);
        supply(&root, "AC", &[("type", "Mains"), ("online", "0")]);
        assert!(!on_battery_in(&root), "a desktop without a battery keeps scanning");
        let _ = fs::remove_dir_all(&root);
    }
}
