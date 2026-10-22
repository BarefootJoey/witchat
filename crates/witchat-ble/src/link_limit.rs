//! How many bytes one radio packet can carry on a link.
//!
//! Links we opened (central role): btleplug does not expose the negotiated
//! MTU, so on Windows we open the link's `GattSession` and read `MaxPduSize`
//! live (ATT MTU - 3 per write). Links a phone opened to our GATT server
//! (peripheral role): `GattSubscribedClient.MaxNotificationSize`. Windows
//! negotiates both shortly after connecting and may change them later.

/// Below this the MTU exchange has not happened yet (the ATT default is 23).
const NEGOTIATED_MIN: usize = 100;

pub(crate) struct LinkLimit {
    #[cfg(windows)]
    source: Option<platform::Source>,
    /// BlueZ GATT server: the smallest payload any subscribed phone negotiated.
    #[cfg(target_os = "linux")]
    shared: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
}

impl LinkLimit {
    /// `id` is the btleplug peripheral id, `AA:BB:CC:DD:EE:FF` on Windows.
    pub(crate) async fn open(id: &str) -> Self {
        #[cfg(windows)]
        {
            let session = match parse_address(id) {
                Some(addr) => tokio::task::spawn_blocking(move || platform::Source::open(addr)).await.ok().flatten(),
                None => None,
            };
            Self { source: session }
        }
        #[cfg(not(windows))]
        {
            let _ = id;
            Self::unknown()
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub(crate) fn unknown() -> Self {
        #[cfg(windows)]
        return Self { source: None };
        #[cfg(target_os = "linux")]
        return Self { shared: None };
        #[cfg(not(any(windows, target_os = "linux")))]
        return Self {};
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn shared(payload: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        Self { shared: Some(payload) }
    }

    /// A phone subscribed to our characteristic; size by its notifications.
    #[cfg(windows)]
    pub(crate) fn for_client(
        client: windows::Devices::Bluetooth::GenericAttributeProfile::GattSubscribedClient,
    ) -> Self {
        Self { source: Some(platform::Source::Client(client)) }
    }

    /// Largest payload one unacknowledged write or notification can carry,
    /// once negotiated.
    pub(crate) fn max_write(&self) -> Option<usize> {
        #[cfg(windows)]
        let payload = self.source.as_ref().and_then(|s| s.max_payload());
        #[cfg(target_os = "linux")]
        let payload = self.shared.as_ref().map(|n| n.load(std::sync::atomic::Ordering::Relaxed));
        #[cfg(not(any(windows, target_os = "linux")))]
        let payload: Option<usize> = None;
        payload.filter(|n| *n >= NEGOTIATED_MIN)
    }
}

pub(crate) fn parse_address(id: &str) -> Option<u64> {
    let hex: String = id.split(':').collect();
    (hex.len() == 12).then(|| u64::from_str_radix(&hex, 16).ok()).flatten()
}

#[cfg(windows)]
mod platform {
    use windows::Devices::Bluetooth::BluetoothLEDevice;
    use windows::Devices::Bluetooth::GenericAttributeProfile::{GattSession, GattSubscribedClient};

    pub(super) enum Source {
        Session { _device: BluetoothLEDevice, session: GattSession },
        Client(GattSubscribedClient),
    }

    impl Source {
        pub(super) fn open(address: u64) -> Option<Self> {
            let device = BluetoothLEDevice::FromBluetoothAddressAsync(address).ok()?.get().ok()?;
            let device_id = device.BluetoothDeviceId().ok()?;
            let session = GattSession::FromDeviceIdAsync(&device_id).ok()?.get().ok()?;
            Some(Self::Session { _device: device, session })
        }

        pub(super) fn max_payload(&self) -> Option<usize> {
            match self {
                Self::Session { session, .. } => session.MaxPduSize().ok().map(|p| usize::from(p).saturating_sub(3)),
                Self::Client(client) => client.MaxNotificationSize().ok().map(usize::from),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_peripheral_ids() {
        assert_eq!(parse_address("AA:BB:CC:DD:EE:FF"), Some(0xAABB_CCDD_EEFF));
        assert_eq!(parse_address("not-a-mac"), None);
    }
}
