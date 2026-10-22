//! Per-link GATT writer. Each link owns one task so a slow or dying phone
//! only delays its own frames.
//!
//! A frame that fits the negotiated ATT MTU goes as write-without-response:
//! one radio packet, no round trip, like iOS centrals. Larger frames (or an
//! MTU not yet known) use write-with-response, which Windows turns into a
//! multi-round-trip long write - correct, but around 200 ms per frame.

use crate::backoff::AttachBackoff;
use crate::central::{BleEvent, Link};
use crate::link_limit::LinkLimit;
use crate::scan_error::{explain_write_error, is_closed_link_error, is_transient_write_error};
use btleplug::api::{CharPropFlags, Characteristic, Peripheral as _, WriteType};
use btleplug::platform::Peripheral;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{debug, info, warn};

/// Frames waiting for one phone. Shallow, so an ack queued behind a file
/// transfer waits about a second; broadcasts wait for room instead.
pub(crate) const LINK_QUEUE: usize = 24;
/// Gap after each unacknowledged write. iOS spaces fragments 25-30 ms so the
/// receiver's buffers keep up; Windows already awaits the controller, so a
/// shorter gap suffices.
const FAST_WRITE_GAP: Duration = Duration::from_millis(8);

pub(crate) struct WriterCtx {
    pub links: Arc<Mutex<HashMap<String, Link>>>,
    pub backoff: Arc<Mutex<AttachBackoff>>,
    pub events: broadcast::Sender<BleEvent>,
}

pub(crate) fn spawn(
    id: String,
    peripheral: Peripheral,
    characteristic: Characteristic,
    limit: Arc<LinkLimit>,
    ctx: WriterCtx,
) -> mpsc::Sender<Vec<u8>> {
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(LINK_QUEUE);
    let reliable = reliable_write_type(&characteristic);
    let fast_ok = characteristic.properties.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE);
    tokio::spawn(async move {
        let mut logged_limit = None;
        let mut peer_closed = false;
        while let Some(frame) = rx.recv().await {
            let max_write = limit.max_write();
            if max_write.is_some() && max_write != logged_limit {
                info!(%id, max_write, fast_ok, "BLE link write size");
                logged_limit = max_write;
            }
            let kind = pick_write_type(frame.len(), fast_ok, max_write, reliable);
            let mut result = write_frame(&peripheral, &characteristic, kind, &frame).await;
            if matches!(&result, Err(msg) if is_transient_write_error(msg)) {
                debug!(%id, "transient GATT write error; retrying once");
                tokio::time::sleep(Duration::from_millis(150)).await;
                result = write_frame(&peripheral, &characteristic, kind, &frame).await;
            }
            let Err(msg) = result else {
                if kind == WriteType::WithoutResponse {
                    tokio::time::sleep(FAST_WRITE_GAP).await;
                }
                continue;
            };
            if is_closed_link_error(&msg) {
                // Expected when a phone walks away; the scan loop reattaches.
                info!(%id, "BLE link closed by the peer; dropping it");
                ctx.links.lock().await.remove(&id);
                ctx.backoff.lock().await.dropped(&id, Instant::now());
                let _ = ctx.events.send(BleEvent::Disconnected(id.clone()));
                peer_closed = true;
                break;
            }
            warn!(%id, bytes = frame.len(), error = %msg, "GATT write failed");
            // Mesh traffic is retried at the protocol layer; only
            // surface errors that need the user to do something.
            if !is_transient_write_error(&msg) {
                let _ = ctx.events.send(BleEvent::Error(explain_write_error(&id, &msg)));
            }
        }
        if !peer_closed {
            // The link was removed on our side; do not leave the phone's
            // connection half open.
            let _ = peripheral.disconnect().await;
        }
        debug!(%id, "link writer stopped");
    });
    tx
}

/// Unacknowledged when the frame fits one ATT packet, else the reliable type.
fn pick_write_type(len: usize, fast_ok: bool, max_write: Option<usize>, reliable: WriteType) -> WriteType {
    match max_write {
        Some(max) if fast_ok && len <= max => WriteType::WithoutResponse,
        _ => reliable,
    }
}

async fn write_frame(
    peripheral: &Peripheral,
    characteristic: &Characteristic,
    preferred: WriteType,
    frame: &[u8],
) -> Result<(), String> {
    match peripheral.write(characteristic, frame, preferred).await {
        Ok(()) => Ok(()),
        Err(err) => {
            let other = match preferred {
                WriteType::WithResponse => WriteType::WithoutResponse,
                WriteType::WithoutResponse => WriteType::WithResponse,
            };
            match peripheral.write(characteristic, frame, other).await {
                Ok(()) => Ok(()),
                Err(_) => Err(err.to_string()),
            }
        }
    }
}

/// The write type for frames that may exceed the MTU. On Windows only
/// write-with-response can do long (prepared) writes.
fn reliable_write_type(ch: &Characteristic) -> WriteType {
    let props = ch.properties;
    #[cfg(windows)]
    {
        if props.contains(CharPropFlags::WRITE) {
            WriteType::WithResponse
        } else {
            WriteType::WithoutResponse
        }
    }
    #[cfg(not(windows))]
    {
        if props.contains(CharPropFlags::WRITE_WITHOUT_RESPONSE) {
            WriteType::WithoutResponse
        } else {
            WriteType::WithResponse
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_that_fit_skip_the_round_trip() {
        let slow = WriteType::WithResponse;
        assert_eq!(pick_write_type(500, true, Some(512), slow), WriteType::WithoutResponse);
        assert_eq!(pick_write_type(513, true, Some(512), slow), slow, "too big: long write");
        assert_eq!(pick_write_type(100, true, None, slow), slow, "MTU unknown");
        assert_eq!(pick_write_type(100, false, Some(512), slow), slow, "not supported");
    }
}
