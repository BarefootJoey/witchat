//! Linux peripheral role: the bitchat GATT service published through
//! BlueZ (`GattManager1`) and advertised with `LEAdvertisingManager1`.
//!
//! BlueZ sends a notification to every subscribed phone at once, so all
//! phones that connected in share one link, `in:bluez`, alive while anyone
//! is subscribed. Writes still name their sender and are reassembled per
//! phone. Notifications are cut to the smallest payload any of them has
//! negotiated; phones reassemble the notification stream.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A phone that stopped writing this long ago no longer limits chunk size.
const MTU_STALE: Duration = Duration::from_secs(120);
/// ATT default MTU (23) minus the 3-byte notification header.
const DEFAULT_PAYLOAD: usize = 20;

/// Per-phone negotiated ATT MTU, as reported with each write.
#[derive(Default)]
pub(crate) struct MtuTable {
    seen: HashMap<String, (u16, Instant)>,
}

impl MtuTable {
    /// Records a write; true when this phone was not known yet.
    pub(crate) fn record(&mut self, phone: &str, mtu: u16, now: Instant) -> bool {
        self.seen.retain(|_, (_, at)| now.duration_since(*at) < MTU_STALE);
        self.seen.insert(phone.to_string(), (mtu, now)).is_none()
    }

    /// Largest notification payload every recent phone can take.
    pub(crate) fn payload(&self) -> usize {
        self.seen
            .values()
            .map(|(mtu, _)| usize::from(*mtu).saturating_sub(3))
            .min()
            .unwrap_or(DEFAULT_PAYLOAD)
            .max(DEFAULT_PAYLOAD)
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn clear(&mut self) {
        self.seen.clear();
    }
}

#[cfg(target_os = "linux")]
pub(crate) use server::{start, Ctx, Server};

#[cfg(target_os = "linux")]
mod server {
    use super::MtuTable;
    use crate::central::{BleEvent, Link};
    use crate::link_limit::LinkLimit;
    use crate::peripheral::PeripheralError;
    use crate::writer::LINK_QUEUE;
    use bluer::adv::{Advertisement, AdvertisementHandle, Type};
    use bluer::gatt::local::{
        Application, ApplicationHandle, Characteristic, CharacteristicNotifier, CharacteristicNotify,
        CharacteristicNotifyMethod, CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod,
        CharacteristicWriteRequest, Service,
    };
    use bluer::{ErrorKind, Session};
    use futures::FutureExt;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Instant;
    use tokio::sync::{broadcast, mpsc, Mutex};
    use tracing::{debug, info, warn};
    use uuid::Uuid;
    use witchat_protocol::constants::CHARACTERISTIC_UUID;
    use witchat_protocol::NotificationStreamAssembler;

    const SHARED_LINK: &str = "in:bluez";

    pub(crate) struct Ctx {
        pub links: Arc<Mutex<HashMap<String, Link>>>,
        pub events: broadcast::Sender<BleEvent>,
    }

    /// Dropping it unpublishes the service and stops advertising.
    pub(crate) struct Server {
        _advertisement: AdvertisementHandle,
        _application: ApplicationHandle,
        _session: Session,
    }

    impl Server {
        pub(crate) fn status(&self) -> String {
            "advertising bitchat service via BlueZ: phones can connect to this PC".into()
        }
    }

    struct Shared {
        ctx: Ctx,
        assemblers: StdMutex<HashMap<String, NotificationStreamAssembler>>,
        mtus: StdMutex<MtuTable>,
        payload: Arc<AtomicUsize>,
    }

    fn bluez(what: &'static str) -> impl Fn(bluer::Error) -> PeripheralError {
        move |err| match err.kind {
            ErrorKind::NotSupported => PeripheralError::Unsupported("this Bluetooth adapter cannot advertise (BlueZ)"),
            ErrorKind::NotPermitted | ErrorKind::NotAuthorized => {
                PeripheralError::Unsupported("BlueZ refused to let witchat advertise (D-Bus policy)")
            }
            ErrorKind::NotReady => PeripheralError::Unsupported("Bluetooth adapter is not ready"),
            _ => PeripheralError::Other(format!("{what}: {err}")),
        }
    }

    pub(crate) async fn start(service: Uuid, ctx: Ctx) -> Result<Server, PeripheralError> {
        let session = Session::new().await.map_err(bluez("BlueZ session"))?;
        let adapter = session.default_adapter().await.map_err(bluez("Bluetooth adapter"))?;
        if !adapter.is_powered().await.unwrap_or(false) {
            return Err(PeripheralError::Unsupported("Bluetooth adapter is off"));
        }
        let shared = Arc::new(Shared {
            ctx,
            assemblers: StdMutex::new(HashMap::new()),
            mtus: StdMutex::new(MtuTable::default()),
            payload: Arc::new(AtomicUsize::new(super::DEFAULT_PAYLOAD)),
        });
        let application = Application {
            services: vec![Service {
                uuid: service,
                primary: true,
                characteristics: vec![characteristic(shared)],
                ..Default::default()
            }],
            ..Default::default()
        };
        let application = adapter.serve_gatt_application(application).await.map_err(bluez("publish GATT service"))?;
        let advertisement = Advertisement {
            advertisement_type: Type::Peripheral,
            service_uuids: [service].into_iter().collect(),
            discoverable: Some(true),
            ..Default::default()
        };
        let advertisement = adapter.advertise(advertisement).await.map_err(bluez("advertise"))?;
        info!(%service, adapter = adapter.name(), "BlueZ GATT server advertising");
        Ok(Server { _advertisement: advertisement, _application: application, _session: session })
    }

    /// Read, write, write-without-response and notify, as iOS
    /// `BLEService+LinkLayerPeripheralRole`.
    fn characteristic(shared: Arc<Shared>) -> Characteristic {
        let on_write = shared.clone();
        let on_notify = shared;
        Characteristic {
            uuid: CHARACTERISTIC_UUID,
            read: Some(CharacteristicRead {
                read: true,
                fun: Box::new(|_| async { Ok(Vec::new()) }.boxed()),
                ..Default::default()
            }),
            write: Some(CharacteristicWrite {
                write: true,
                write_without_response: true,
                method: CharacteristicWriteMethod::Fun(Box::new(move |value, request| {
                    on_write.on_write(value, &request);
                    async { Ok(()) }.boxed()
                })),
                ..Default::default()
            }),
            notify: Some(CharacteristicNotify {
                notify: true,
                method: CharacteristicNotifyMethod::Fun(Box::new(move |notifier| {
                    let shared = on_notify.clone();
                    async move { shared.serve(notifier).await }.boxed()
                })),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    impl Shared {
        fn on_write(&self, value: Vec<u8>, request: &CharacteristicWriteRequest) {
            let from = format!("in:{}", request.device_address.to_string().to_ascii_uppercase());
            let new_phone = {
                let mut mtus = self.mtus.lock().unwrap_or_else(|p| p.into_inner());
                let new_phone = mtus.record(&from, request.mtu, Instant::now());
                self.payload.store(mtus.payload(), Ordering::Relaxed);
                new_phone
            };
            if new_phone {
                info!(%from, mtu = request.mtu, "phone writing to our GATT server");
                // Phones list us once they hear an announce; this asks for one now.
                let _ = self.ctx.events.send(BleEvent::Connected { name: from.clone(), id: from.clone() });
            }
            let frames = {
                let mut map = self.assemblers.lock().unwrap_or_else(|p| p.into_inner());
                map.entry(from.clone()).or_default().append(&value).frames
            };
            for data in frames {
                debug!(%from, bytes = data.len(), "frame from inbound link");
                let _ = self.ctx.events.send(BleEvent::Frame { from: from.clone(), data });
            }
        }

        /// BlueZ starts one notify session when the first phone subscribes
        /// and stops it when the last one leaves.
        async fn serve(&self, mut notifier: CharacteristicNotifier) {
            let (tx, mut rx) = mpsc::channel::<Vec<u8>>(LINK_QUEUE);
            let limit = Arc::new(LinkLimit::shared(self.payload.clone()));
            self.ctx.links.lock().await.insert(SHARED_LINK.into(), Link::new(tx, limit));
            info!("phone subscribed to our GATT server");
            let _ = self.ctx.events.send(BleEvent::Connected { name: SHARED_LINK.into(), id: SHARED_LINK.into() });
            let stopped = notifier.stopped();
            tokio::pin!(stopped);
            'session: loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    frame = rx.recv() => {
                        let Some(frame) = frame else { break };
                        let max = self.payload.load(Ordering::Relaxed);
                        for chunk in frame.chunks(max) {
                            if let Err(err) = notifier.notify(chunk.to_vec()).await {
                                warn!(%err, "notification failed; closing inbound link");
                                break 'session;
                            }
                        }
                    }
                }
            }
            self.ctx.links.lock().await.remove(SHARED_LINK);
            self.mtus.lock().unwrap_or_else(|p| p.into_inner()).clear();
            self.assemblers.lock().unwrap_or_else(|p| p.into_inner()).clear();
            info!("last phone unsubscribed from our GATT server");
            let _ = self.ctx.events.send(BleEvent::Disconnected(SHARED_LINK.into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_fit_the_smallest_recent_phone() {
        let now = Instant::now();
        let mut table = MtuTable::default();
        assert_eq!(table.payload(), DEFAULT_PAYLOAD);
        assert!(table.record("in:A", 517, now));
        assert!(!table.record("in:A", 517, now));
        assert_eq!(table.payload(), 514);
        assert!(table.record("in:B", 185, now));
        assert_eq!(table.payload(), 182);
        table.record("in:A", 517, now + MTU_STALE);
        assert_eq!(table.payload(), 514, "a phone gone quiet stops limiting");
        table.record("in:C", 10, now + MTU_STALE);
        assert_eq!(table.payload(), DEFAULT_PAYLOAD, "never below the ATT default");
    }
}
