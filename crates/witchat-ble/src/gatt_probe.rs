//! Why an attach found no bitchat characteristic.
//!
//! btleplug turns a characteristic query that ends `Unreachable` or
//! `AccessDenied` into an empty list, so every such phone looked like
//! "missing bitchat characteristic". This asks WinRT directly for the
//! bitchat service and characteristic and reports the GATT status, so logs
//! show whether the phone was busy, refused us, or really lacks the server.

use uuid::Uuid;

/// [`Probe::Missing`] detail when the device has no bitchat service at all.
pub(crate) const NO_SERVICE: &str = "no bitchat service in the GATT database";
/// In a [`Probe`] detail when the phone (or Windows) refused the query.
pub(crate) const ACCESS_DENIED: &str = "access denied";

/// What the phone answered, for the log and the backoff decision.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum Probe {
    /// The characteristic is there now; a retry should attach.
    Ready,
    /// The phone did not answer (busy, asleep, out of range).
    Unreachable(String),
    /// The phone answered without a bitchat service or characteristic.
    Missing(String),
    /// Could not ask (non-Windows, or the device id is not an address).
    Unknown,
}

impl Probe {
    pub(crate) fn detail(&self) -> &str {
        match self {
            Self::Ready => "characteristic visible on recheck",
            Self::Unreachable(d) | Self::Missing(d) => d,
            Self::Unknown => "no GATT status available",
        }
    }
}

pub(crate) async fn probe(id: &str, service: Uuid, characteristic: Uuid) -> Probe {
    let Some(address) = crate::link_limit::parse_address(id) else {
        return Probe::Unknown;
    };
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || platform::probe(address, service, characteristic))
            .await
            .unwrap_or(Probe::Unknown)
    }
    #[cfg(not(windows))]
    {
        let _ = (address, service, characteristic);
        Probe::Unknown
    }
}

#[cfg(windows)]
mod platform {
    use super::Probe;
    use uuid::Uuid;
    use windows::core::GUID;
    use windows::Devices::Bluetooth::GenericAttributeProfile::GattCommunicationStatus;
    use windows::Devices::Bluetooth::{BluetoothCacheMode, BluetoothLEDevice};

    fn status_name(status: GattCommunicationStatus) -> &'static str {
        match status {
            GattCommunicationStatus::Success => "success",
            GattCommunicationStatus::Unreachable => "unreachable",
            GattCommunicationStatus::ProtocolError => "protocol error",
            GattCommunicationStatus::AccessDenied => super::ACCESS_DENIED,
            _ => "unknown status",
        }
    }

    fn unreachable(status: GattCommunicationStatus) -> bool {
        status == GattCommunicationStatus::Unreachable
    }

    pub(super) fn probe(address: u64, service: Uuid, characteristic: Uuid) -> Probe {
        let run = || -> windows::core::Result<Probe> {
            let device = BluetoothLEDevice::FromBluetoothAddressAsync(address)?.get()?;
            let services = device
                .GetGattServicesForUuidWithCacheModeAsync(
                    GUID::from_u128(service.as_u128()),
                    BluetoothCacheMode::Uncached,
                )?
                .get()?;
            let status = services.Status()?;
            if status != GattCommunicationStatus::Success {
                let detail = format!("service query: {}", status_name(status));
                return Ok(if unreachable(status) { Probe::Unreachable(detail) } else { Probe::Missing(detail) });
            }
            let Some(found) = services.Services()?.into_iter().next() else {
                return Ok(Probe::Missing(super::NO_SERVICE.into()));
            };
            let chars = found
                .GetCharacteristicsForUuidWithCacheModeAsync(
                    GUID::from_u128(characteristic.as_u128()),
                    BluetoothCacheMode::Uncached,
                )?
                .get()?;
            let status = chars.Status()?;
            if status != GattCommunicationStatus::Success {
                let detail = format!("characteristic query: {}", status_name(status));
                return Ok(if unreachable(status) { Probe::Unreachable(detail) } else { Probe::Missing(detail) });
            }
            Ok(if chars.Characteristics()?.Size()? > 0 {
                Probe::Ready
            } else {
                Probe::Missing("bitchat service without its characteristic".into())
            })
        };
        run().unwrap_or_else(|err| Probe::Unreachable(format!("WinRT: {err}")))
    }
}
