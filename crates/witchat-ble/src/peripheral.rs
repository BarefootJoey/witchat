//! GATT server / advertise role, so phones can connect to the PC.
//!
//! Official nodes are dual-role. Witchat publishes the bitchat service with
//! one characteristic (read, write, write-without-response, notify; as iOS
//! `BLEService+LinkLayerPeripheralRole`). On Windows a phone that
//! subscribes becomes an ordinary link in [`BleCentral`]'s table, keyed
//! `in:<address>`, so broadcasts and fragment sizing reach it unchanged.
//! Linux goes through BlueZ, which notifies all subscribers at once; see
//! `bluez_server`. Writes are reassembled per phone and arrive as
//! [`BleEvent::Frame`].
//!
//! Many desktop adapters cannot advertise; then this fails with
//! [`PeripheralError::Unsupported`] and witchat stays central-only.

use crate::central::BleCentral;

#[derive(Debug, thiserror::Error)]
pub enum PeripheralError {
    #[error("{0}")]
    Unsupported(&'static str),
    #[error("{0}")]
    Other(String),
}

impl BleCentral {
    /// Publish the bitchat service and advertise it. Returns a status line.
    pub async fn start_peripheral(&self) -> Result<String, PeripheralError> {
        #[cfg(any(windows, target_os = "linux"))]
        let service = if self.include_testnet {
            witchat_protocol::constants::SERVICE_UUID_TESTNET
        } else {
            witchat_protocol::constants::SERVICE_UUID_MAINNET
        };
        #[cfg(windows)]
        {
            let ctx = server::Ctx { links: self.links.clone(), events: self.events.clone() };
            let started = tokio::task::spawn_blocking(move || server::Server::start(service, ctx))
                .await
                .map_err(|e| PeripheralError::Other(e.to_string()))??;
            let status = started.status();
            *self.server.lock().unwrap_or_else(|p| p.into_inner()) = Some(started);
            Ok(status)
        }
        #[cfg(target_os = "linux")]
        {
            let ctx = crate::bluez_server::Ctx { links: self.links.clone(), events: self.events.clone() };
            let started = crate::bluez_server::start(service, ctx).await?;
            let status = started.status();
            *self.server.lock().unwrap_or_else(|p| p.into_inner()) = Some(started);
            Ok(status)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Err(PeripheralError::Unsupported("peripheral role is not implemented on this OS"))
        }
    }
}

/// `BluetoothAdapter.IsPeripheralRoleSupported` for the default adapter.
#[cfg(windows)]
pub fn adapter_peripheral_support() -> Result<bool, String> {
    use windows::Devices::Bluetooth::BluetoothAdapter;
    let adapter =
        BluetoothAdapter::GetDefaultAsync().and_then(|op| op.get()).map_err(|e| format!("no default adapter: {e}"))?;
    adapter.IsPeripheralRoleSupported().map_err(|e| e.to_string())
}

/// `BluetoothLE#BluetoothLE<local>-<remote>` -> `in:<REMOTE>`, so a phone's
/// inbound link never collides with the outbound one we opened to it.
#[cfg_attr(not(windows), allow(dead_code))]
fn inbound_link_id(device_id: &str) -> String {
    let remote = device_id.rsplit('-').next().filter(|r| r.len() == 17).unwrap_or(device_id);
    format!("in:{}", remote.to_ascii_uppercase())
}

#[cfg(target_os = "linux")]
pub(crate) use crate::bluez_server::Server;
#[cfg(windows)]
pub(crate) use server::Server;

#[cfg(windows)]
mod server {
    use super::{adapter_peripheral_support, inbound_link_id, PeripheralError};
    use crate::central::{BleEvent, Link};
    use crate::link_limit::LinkLimit;
    use crate::writer::LINK_QUEUE;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::{broadcast, mpsc, Mutex};
    use tracing::{debug, info, warn};
    use uuid::Uuid;
    use windows::core::GUID;
    use windows::Devices::Bluetooth::BluetoothError;
    use windows::Devices::Bluetooth::GenericAttributeProfile::{
        GattCharacteristicProperties, GattCommunicationStatus, GattLocalCharacteristic,
        GattLocalCharacteristicParameters, GattProtectionLevel, GattReadRequestedEventArgs, GattServiceProvider,
        GattServiceProviderAdvertisementStatus, GattServiceProviderAdvertisementStatusChangedEventArgs,
        GattServiceProviderAdvertisingParameters, GattSession, GattSubscribedClient, GattWriteOption,
        GattWriteRequestedEventArgs,
    };
    use windows::Foundation::TypedEventHandler;
    use windows::Storage::Streams::{DataReader, DataWriter, IBuffer};
    use witchat_protocol::constants::CHARACTERISTIC_UUID;
    use witchat_protocol::NotificationStreamAssembler;

    /// What the server shares with the central role.
    pub(crate) struct Ctx {
        pub links: Arc<Mutex<HashMap<String, Link>>>,
        pub events: broadcast::Sender<BleEvent>,
    }

    /// Keeps the WinRT objects alive; dropping it stops advertising.
    pub(crate) struct Server {
        provider: GattServiceProvider,
        characteristic: GattLocalCharacteristic,
        tokens: [i64; 3],
        status: GattServiceProviderAdvertisementStatus,
    }

    fn guid(id: Uuid) -> GUID {
        GUID::from_u128(id.as_u128())
    }

    fn bt_error(what: &str, err: BluetoothError) -> PeripheralError {
        match err {
            BluetoothError::RadioNotAvailable => PeripheralError::Unsupported("Bluetooth radio is off or missing"),
            BluetoothError::NotSupported => PeripheralError::Unsupported("this adapter cannot host a GATT server"),
            BluetoothError::DisabledByPolicy | BluetoothError::DisabledByUser => {
                PeripheralError::Unsupported("Bluetooth peripheral role is disabled by Windows settings or policy")
            }
            other => PeripheralError::Other(format!("{what}: Bluetooth error {}", other.0)),
        }
    }

    fn win(what: &str) -> impl Fn(windows::core::Error) -> PeripheralError + '_ {
        move |e| PeripheralError::Other(format!("{what}: {e}"))
    }

    /// Advertising starts asynchronously; wait briefly for it to settle.
    fn settle(provider: &GattServiceProvider) -> Result<GattServiceProviderAdvertisementStatus, PeripheralError> {
        let mut status = GattServiceProviderAdvertisementStatus::Created;
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            status = provider.AdvertisementStatus().map_err(win("advertising status"))?;
            debug!(status = status.0, "advertising status");
            if status == GattServiceProviderAdvertisementStatus::Started
                || status == GattServiceProviderAdvertisementStatus::StartedWithoutAllAdvertisementData
            {
                break;
            }
        }
        Ok(status)
    }

    fn session_link_id(session: &GattSession) -> windows::core::Result<String> {
        Ok(inbound_link_id(&session.DeviceId()?.Id()?.to_string()))
    }

    fn bytes_of(buffer: &IBuffer) -> windows::core::Result<Vec<u8>> {
        let reader = DataReader::FromBuffer(buffer)?;
        let mut bytes = vec![0u8; buffer.Length()? as usize];
        reader.ReadBytes(&mut bytes)?;
        Ok(bytes)
    }

    fn buffer_of(bytes: &[u8]) -> windows::core::Result<IBuffer> {
        let writer = DataWriter::new()?;
        writer.WriteBytes(bytes)?;
        writer.DetachBuffer()
    }

    impl Server {
        pub(crate) fn start(service: Uuid, ctx: Ctx) -> Result<Self, PeripheralError> {
            match adapter_peripheral_support() {
                Ok(true) => {}
                Ok(false) => {
                    return Err(PeripheralError::Unsupported("this Bluetooth adapter reports no peripheral role"))
                }
                Err(err) => return Err(PeripheralError::Other(format!("could not ask the Bluetooth adapter: {err}"))),
            }

            let created = GattServiceProvider::CreateAsync(guid(service))
                .and_then(|op| op.get())
                .map_err(win("create GATT service"))?;
            let err = created.Error().map_err(win("create GATT service"))?;
            if err != BluetoothError::Success {
                return Err(bt_error("create GATT service", err));
            }
            let provider = created.ServiceProvider().map_err(win("create GATT service"))?;

            let params = GattLocalCharacteristicParameters::new().map_err(win("characteristic parameters"))?;
            let props = GattCharacteristicProperties::Read
                | GattCharacteristicProperties::Write
                | GattCharacteristicProperties::WriteWithoutResponse
                | GattCharacteristicProperties::Notify;
            params.SetCharacteristicProperties(props).map_err(win("characteristic properties"))?;
            params.SetReadProtectionLevel(GattProtectionLevel::Plain).map_err(win("read protection"))?;
            params.SetWriteProtectionLevel(GattProtectionLevel::Plain).map_err(win("write protection"))?;
            let made = provider
                .Service()
                .and_then(|s| s.CreateCharacteristicAsync(guid(CHARACTERISTIC_UUID), &params))
                .and_then(|op| op.get())
                .map_err(win("create characteristic"))?;
            let err = made.Error().map_err(win("create characteristic"))?;
            if err != BluetoothError::Success {
                return Err(bt_error("create characteristic", err));
            }
            let characteristic = made.Characteristic().map_err(win("create characteristic"))?;

            let ctx = Arc::new(ctx);
            let tokens = [
                characteristic.WriteRequested(&on_write(ctx.clone())).map_err(win("write handler"))?,
                characteristic.ReadRequested(&on_read()).map_err(win("read handler"))?,
                characteristic
                    .SubscribedClientsChanged(&on_subscribers(ctx.clone()))
                    .map_err(win("subscription handler"))?,
            ];

            let adv = GattServiceProviderAdvertisingParameters::new().map_err(win("advertising parameters"))?;
            adv.SetIsConnectable(true).map_err(win("advertising parameters"))?;
            adv.SetIsDiscoverable(true).map_err(win("advertising parameters"))?;
            let last_error = Arc::new(StdMutex::new(BluetoothError::Success));
            let seen = last_error.clone();
            let status_token = provider
                .AdvertisementStatusChanged(&TypedEventHandler::new(
                    move |_, args: windows::core::Ref<GattServiceProviderAdvertisementStatusChangedEventArgs>| {
                        if let Some(args) = args.as_ref() {
                            let (status, error) = (args.Status()?, args.Error()?);
                            debug!(status = status.0, error = error.0, "advertisement status changed");
                            *seen.lock().unwrap_or_else(|p| p.into_inner()) = error;
                        }
                        Ok(())
                    },
                ))
                .map_err(win("advertising status handler"))?;
            provider.StartAdvertisingWithParameters(&adv).map_err(win("start advertising"))?;
            let status = settle(&provider)?;
            let _ = provider.RemoveAdvertisementStatusChanged(status_token);
            info!(%service, status = status.0, "GATT server advertising status");
            if status != GattServiceProviderAdvertisementStatus::Started
                && status != GattServiceProviderAdvertisementStatus::StartedWithoutAllAdvertisementData
            {
                let error = *last_error.lock().unwrap_or_else(|p| p.into_inner());
                return Err(match error {
                    BluetoothError::Success => {
                        PeripheralError::Other(format!("Windows did not start advertising (status {})", status.0))
                    }
                    error => bt_error("advertising", error),
                });
            }
            Ok(Self { provider, characteristic, tokens, status })
        }

        pub(crate) fn status(&self) -> String {
            if self.status == GattServiceProviderAdvertisementStatus::StartedWithoutAllAdvertisementData {
                "advertising bitchat service (Windows trimmed some advertisement data)".into()
            } else {
                "advertising bitchat service: phones can connect to this PC".into()
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.characteristic.RemoveWriteRequested(self.tokens[0]);
            let _ = self.characteristic.RemoveReadRequested(self.tokens[1]);
            let _ = self.characteristic.RemoveSubscribedClientsChanged(self.tokens[2]);
            let _ = self.provider.StopAdvertising();
        }
    }

    /// A phone wrote to us. Answer first (iOS: centrals time out quickly),
    /// then reassemble: a frame may span several writes. A phone whose link
    /// was dropped as silent is linked again here.
    fn on_write(ctx: Arc<Ctx>) -> TypedEventHandler<GattLocalCharacteristic, GattWriteRequestedEventArgs> {
        let assemblers: StdMutex<HashMap<String, NotificationStreamAssembler>> = StdMutex::new(HashMap::new());
        TypedEventHandler::new(
            move |sender: windows::core::Ref<GattLocalCharacteristic>,
                  args: windows::core::Ref<GattWriteRequestedEventArgs>| {
                let Some(args) = args.as_ref() else { return Ok(()) };
                let deferral = args.GetDeferral()?;
                let request = args.GetRequestAsync()?.get()?;
                let data = bytes_of(&request.Value()?)?;
                if request.Option()? == GattWriteOption::WriteWithResponse {
                    request.Respond()?;
                }
                deferral.Complete()?;
                let from = session_link_id(&args.Session()?)?;
                let known = match ctx.links.blocking_lock().get(&from) {
                    Some(link) => {
                        link.touch();
                        true
                    }
                    None => false,
                };
                if !known {
                    if let Some(characteristic) = sender.as_ref() {
                        sync_subscribers(characteristic, &ctx)?;
                    }
                }
                let frames = {
                    let mut map = assemblers.lock().unwrap_or_else(|p| p.into_inner());
                    map.entry(from.clone()).or_default().append(&data).frames
                };
                for data in frames {
                    debug!(%from, bytes = data.len(), "frame from inbound link");
                    let _ = ctx.events.send(BleEvent::Frame { from: from.clone(), data });
                }
                Ok(())
            },
        )
    }

    /// The characteristic is readable as on iOS, but carries no value.
    fn on_read() -> TypedEventHandler<GattLocalCharacteristic, GattReadRequestedEventArgs> {
        TypedEventHandler::new(|_, args: windows::core::Ref<GattReadRequestedEventArgs>| {
            let Some(args) = args.as_ref() else { return Ok(()) };
            let deferral = args.GetDeferral()?;
            let request = args.GetRequestAsync()?.get()?;
            request.RespondWithValue(&buffer_of(&[])?)?;
            deferral.Complete()
        })
    }

    /// Diff the subscriber list against our links: new phones get a link and
    /// a notify thread; phones that left are dropped.
    fn on_subscribers(ctx: Arc<Ctx>) -> TypedEventHandler<GattLocalCharacteristic, windows::core::IInspectable> {
        TypedEventHandler::new(move |sender: windows::core::Ref<GattLocalCharacteristic>, _| {
            let Some(characteristic) = sender.as_ref() else { return Ok(()) };
            sync_subscribers(characteristic, &ctx)
        })
    }

    fn sync_subscribers(characteristic: &GattLocalCharacteristic, ctx: &Arc<Ctx>) -> windows::core::Result<()> {
        let mut current: HashMap<String, GattSubscribedClient> = HashMap::new();
        for client in characteristic.SubscribedClients()? {
            if let Ok(id) = session_link_id(&client.Session()?) {
                current.insert(id, client);
            }
        }
        let mut links = ctx.links.blocking_lock();
        let gone: Vec<String> =
            links.keys().filter(|id| id.starts_with("in:") && !current.contains_key(*id)).cloned().collect();
        for id in gone {
            links.remove(&id);
            info!(%id, "phone unsubscribed from our GATT server");
            let _ = ctx.events.send(BleEvent::Disconnected(id));
        }
        for (id, client) in current {
            if links.contains_key(&id) {
                continue;
            }
            let limit = Arc::new(LinkLimit::for_client(client.clone()));
            let tx = spawn_notifier(id.clone(), characteristic.clone(), client, ctx.clone());
            links.insert(id.clone(), Link::inbound(tx, limit));
            info!(%id, "phone subscribed to our GATT server");
            let _ = ctx.events.send(BleEvent::Connected { name: id.clone(), id });
        }
        Ok(())
    }

    /// One thread per subscribed phone: WinRT notify calls block, and a slow
    /// phone must only delay its own frames.
    fn spawn_notifier(
        id: String,
        characteristic: GattLocalCharacteristic,
        client: GattSubscribedClient,
        ctx: Arc<Ctx>,
    ) -> mpsc::Sender<Vec<u8>> {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(LINK_QUEUE);
        let spawned = std::thread::Builder::new().name(format!("witchat-notify-{id}")).spawn(move || {
            while let Some(frame) = rx.blocking_recv() {
                // Phones reassemble a notification stream, so a frame larger
                // than one notification is split rather than dropped.
                let max = client.MaxNotificationSize().map(usize::from).unwrap_or(20).max(20);
                for chunk in frame.chunks(max) {
                    match notify(&characteristic, &client, chunk) {
                        Ok(GattCommunicationStatus::Success) => {}
                        Ok(status) if status == GattCommunicationStatus::Unreachable => {
                            info!(%id, "phone unreachable; dropping inbound link");
                            ctx.links.blocking_lock().remove(&id);
                            let _ = ctx.events.send(BleEvent::Disconnected(id.clone()));
                            return;
                        }
                        Ok(status) => warn!(%id, status = status.0, "notification refused"),
                        Err(err) => warn!(%id, %err, "notification failed"),
                    }
                }
            }
            debug!(%id, "notify thread stopped");
        });
        if let Err(err) = spawned {
            warn!(%err, "could not start notify thread");
        }
        tx
    }

    fn notify(
        characteristic: &GattLocalCharacteristic,
        client: &GattSubscribedClient,
        chunk: &[u8],
    ) -> windows::core::Result<GattCommunicationStatus> {
        characteristic.NotifyValueForSubscribedClientAsync(&buffer_of(chunk)?, client)?.get()?.Status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_ids_use_the_remote_address() {
        assert_eq!(
            inbound_link_id("BluetoothLE#BluetoothLE00:11:22:33:44:55-AA:BB:CC:12:34:56"),
            "in:AA:BB:CC:12:34:56"
        );
        assert_eq!(inbound_link_id("weird"), "in:WEIRD");
    }

    /// Hardware check: `cargo test -p witchat-ble adapter_role -- --ignored --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore = "asks the real Bluetooth adapter"]
    fn adapter_role() {
        println!("IsPeripheralRoleSupported: {:?}", adapter_peripheral_support());
    }

    /// Hardware check: publishes the testnet service for a few seconds.
    /// `cargo test -p witchat-ble gatt_server_publishes -- --ignored --nocapture`
    #[cfg(windows)]
    #[test]
    #[ignore = "advertises on the real Bluetooth adapter"]
    fn gatt_server_publishes() {
        let ctx = server::Ctx { links: Default::default(), events: tokio::sync::broadcast::channel(8).0 };
        match Server::start(witchat_protocol::constants::SERVICE_UUID_TESTNET, ctx) {
            Ok(server) => {
                println!("{}", server.status());
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
            Err(err) => println!("GATT server failed: {err}"),
        }
    }
}
