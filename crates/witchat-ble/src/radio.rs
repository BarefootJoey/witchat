//! The Bluetooth radio itself. Windows can keep a phone's connection open
//! after witchat exits, jammed so nothing on it is ever answered; phones
//! then never reappear until the radio is turned off and on. This module
//! does that reset, and reports whether Windows still holds a phone.

use std::time::Duration;

/// Long enough for every connection to drop, as with the settings switch.
const OFF_FOR: Duration = Duration::from_secs(3);

/// Turn Bluetooth off, wait, and turn it back on. Every connection on the
/// PC drops, headphones included; they reconnect on their own.
pub async fn cycle() -> Result<(), String> {
    #[cfg(windows)]
    return tokio::task::spawn_blocking(platform::cycle).await.map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    return platform::cycle().await;
    #[cfg(not(any(windows, target_os = "linux")))]
    Err("turning Bluetooth off and on is not supported here".into())
}

/// Whether Windows reports a live connection to this device (`AA:BB:..`
/// or `in:AA:BB:..`). None when it cannot tell, or off Windows.
pub fn os_connected(id: &str) -> Option<bool> {
    #[cfg(windows)]
    return platform::os_connected(crate::link_limit::parse_address(id.trim_start_matches("in:"))?);
    #[cfg(not(windows))]
    {
        let _ = id;
        None
    }
}

#[cfg(windows)]
mod platform {
    use super::OFF_FOR;
    use std::time::Duration;
    use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothLEDevice};
    use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};

    const SETTLE: Duration = Duration::from_secs(5);
    const POLL: Duration = Duration::from_millis(100);

    pub(super) fn cycle() -> Result<(), String> {
        let radio = bluetooth_radio()?;
        set(&radio, RadioState::Off)?;
        std::thread::sleep(OFF_FOR);
        set(&radio, RadioState::On)
    }

    fn bluetooth_radio() -> Result<Radio, String> {
        let access = Radio::RequestAccessAsync().and_then(|op| op.get()).map_err(|e| format!("radio access: {e}"))?;
        if access != RadioAccessStatus::Allowed {
            return Err(format!("Windows does not let witchat switch Bluetooth ({access:?})"));
        }
        let radios = Radio::GetRadiosAsync().and_then(|op| op.get()).map_err(|e| format!("list radios: {e}"))?;
        radios
            .into_iter()
            .find(|r| r.Kind().is_ok_and(|k| k == RadioKind::Bluetooth))
            .ok_or_else(|| "no Bluetooth radio".to_string())
    }

    fn set(radio: &Radio, state: RadioState) -> Result<(), String> {
        let access =
            radio.SetStateAsync(state).and_then(|op| op.get()).map_err(|e| format!("switch Bluetooth: {e}"))?;
        if access != RadioAccessStatus::Allowed {
            return Err(format!("Windows refused to switch Bluetooth ({access:?})"));
        }
        let mut waited = Duration::ZERO;
        while waited < SETTLE {
            if radio.State().is_ok_and(|s| s == state) {
                return Ok(());
            }
            std::thread::sleep(POLL);
            waited += POLL;
        }
        Err(format!("Bluetooth did not turn {}", if state == RadioState::On { "on" } else { "off" }))
    }

    pub(super) fn os_connected(address: u64) -> Option<bool> {
        let device = BluetoothLEDevice::FromBluetoothAddressAsync(address).ok()?.get().ok()?;
        let status = device.ConnectionStatus().ok();
        let _ = device.Close();
        status.map(|s| s == BluetoothConnectionStatus::Connected)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::OFF_FOR;

    pub(super) async fn cycle() -> Result<(), String> {
        let session = bluer::Session::new().await.map_err(|e| format!("BlueZ: {e}"))?;
        let adapter = session.default_adapter().await.map_err(|e| format!("adapter: {e}"))?;
        adapter.set_powered(false).await.map_err(|e| format!("power off: {e}"))?;
        tokio::time::sleep(OFF_FOR).await;
        adapter.set_powered(true).await.map_err(|e| format!("power on: {e}"))
    }
}
