//! BLE transport.
//!
//! Central role uses `btleplug` (Windows / Linux / macOS). The peripheral
//! role (GATT server + advertising) runs on Windows and on Linux through
//! BlueZ, where the adapter supports it; see `peripheral`.

mod backoff;
#[cfg(any(target_os = "linux", test))]
mod bluez_server;
pub mod central;
mod denials;
mod gatt_probe;
mod link_limit;
pub mod peripheral;
pub mod radio;
pub mod report;
mod scan_error;
mod writer;

pub use central::{probe_adapter, BleCentral, BleEvent};
pub use peripheral::PeripheralError;
pub use report::{LinkEntry, RadioReport, ScanEntry};
