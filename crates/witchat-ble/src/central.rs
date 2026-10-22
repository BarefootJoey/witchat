use crate::backoff::{AttachBackoff, SILENT_COOLDOWN};
use crate::denials::{self, DenialWatch};
use crate::gatt_probe::{self, Probe};
use crate::link_limit::LinkLimit;
use crate::report::{LinkEntry, RadioReport, ScanEntry};
use crate::scan_error::{
    explain_scan_error, is_already_scanning, is_expected_attach_error, is_retryable_scan_error, radio_off_hint,
};
use crate::writer::{self, WriterCtx};
use btleplug::api::{Central, CentralState, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info, trace, warn};
use uuid::Uuid;
use witchat_protocol::constants::{CHARACTERISTIC_UUID, SERVICE_UUID_MAINNET, SERVICE_UUID_TESTNET};
use witchat_protocol::NotificationStreamAssembler;

#[derive(Debug, Clone)]
pub enum BleEvent {
    Adapter(String),
    Radio {
        ready: bool,
        detail: String,
    },
    PeerFound {
        id: String,
        name: String,
    },
    Frame {
        from: String,
        data: Vec<u8>,
    },
    Connected {
        id: String,
        name: String,
    },
    Disconnected(String),
    Error(String),
    /// Bluetooth is stuck in a way only turning it off and on fixes; the
    /// text says so for the user.
    NeedsReset(String),
    Report(RadioReport),
}

const MAX_LINKS: usize = 8;
const MAX_INFLIGHT: usize = 3;
const CENSUS_EVERY: Duration = Duration::from_secs(5);

/// A link whose queue stays full this long is dying; drop its copy.
const LINK_STALL: Duration = Duration::from_secs(5);
/// Extra wait before the last discovery when the phone was only busy.
const LATE_DISCOVERY: Duration = Duration::from_millis(1500);
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(10);
/// iOS gives up on a connect after 8 s; a subscribe that takes longer is a
/// link that will not carry traffic.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(8);
/// Phones announce on every link at least every ~38 s (iOS: 30 s + 8 s
/// jitter when dense), so a link quiet this long is dead even if Windows
/// still reports it connected or subscribed.
const SILENT_LINK: Duration = Duration::from_secs(75);
const DUPLICATE_HOLD: Duration = Duration::from_secs(10 * 60);
const SHUTDOWN_DISCONNECT: Duration = Duration::from_secs(2);
/// On battery the scan rests between windows, as phones do. Phones
/// advertise continuously, so a 5 s window still hears every one nearby.
const DUTY_SCAN: Duration = Duration::from_secs(5);
const DUTY_REST: Duration = Duration::from_secs(10);
const SCAN_RECHECK: Duration = Duration::from_secs(2);

/// A scan that hears no advertiser at all, not even a non-bitchat one, has
/// a stuck radio more often than an empty room: restart the scan once, then
/// tell the user.
const DEAF_RESTART: Duration = Duration::from_secs(45);
const DEAF_WARN: Duration = Duration::from_secs(90);
const DEAF_HINT: &str = "Bluetooth hears no devices at all; the radio reports on but is not receiving. If \
                         phones are nearby, press \"reset Bluetooth\" to turn it off and on and restart witchat \
                         (or restart the PC).";

/// Two phones, so one phone that is busy or walking away is not enough.
const STALLED_PHONES: usize = 2;
/// Marks an attach that timed out on a connection Windows already held
/// before we asked; one such phone is enough to offer a reset.
const LEFTOVER: &str = " on a connection Windows already held";

#[derive(Debug, PartialEq, Eq)]
enum DeafStep {
    Fine,
    RestartScan,
    Warn,
}

#[derive(Default)]
struct DeafWatch {
    since: Option<Instant>,
    restarted: bool,
    warned: bool,
}

impl DeafWatch {
    fn check(&mut self, scanning: bool, ads_seen: usize, now: Instant) -> DeafStep {
        if !scanning || ads_seen > 0 {
            *self = Self::default();
            return DeafStep::Fine;
        }
        let deaf_for = now.saturating_duration_since(*self.since.get_or_insert(now));
        if deaf_for >= DEAF_WARN && !self.warned {
            self.warned = true;
            DeafStep::Warn
        } else if deaf_for >= DEAF_RESTART && !self.restarted {
            self.restarted = true;
            DeafStep::RestartScan
        } else {
            DeafStep::Fine
        }
    }
}

/// What `maintain_scan` does next with a running scan.
#[derive(Debug, PartialEq, Eq)]
enum ScanStep {
    /// Keep scanning; look again after this long.
    Keep(Duration),
    /// Stop scanning and rest this long.
    Rest(Duration),
}

fn next_scan_step(low_power: bool, scanned_for: Duration) -> ScanStep {
    if !low_power {
        return ScanStep::Keep(SCAN_RECHECK);
    }
    match DUTY_SCAN.checked_sub(scanned_for) {
        Some(left) if !left.is_zero() => ScanStep::Keep(left.min(SCAN_RECHECK)),
        _ => ScanStep::Rest(DUTY_REST),
    }
}

/// A live GATT link with its own writer task: one we opened to a phone
/// (see `writer`), or a phone subscribed to our server (see `peripheral`).
pub(crate) struct Link {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    limit: Arc<LinkLimit>,
    since: Instant,
    /// When the phone last sent. None for BlueZ's shared link, which
    /// cannot tell its phones apart.
    heard: Option<Arc<Heard>>,
    /// Links we opened, so [`BleCentral::shutdown`] can close them itself.
    peripheral: Option<Peripheral>,
    frames_out: u64,
    bytes_out: u64,
}

impl Link {
    pub(crate) fn new(tx: tokio::sync::mpsc::Sender<Vec<u8>>, limit: Arc<LinkLimit>) -> Self {
        Self { tx, limit, since: Instant::now(), heard: None, peripheral: None, frames_out: 0, bytes_out: 0 }
    }

    fn outbound(
        tx: tokio::sync::mpsc::Sender<Vec<u8>>,
        limit: Arc<LinkLimit>,
        heard: Arc<Heard>,
        peripheral: Peripheral,
    ) -> Self {
        Self { heard: Some(heard), peripheral: Some(peripheral), ..Self::new(tx, limit) }
    }

    /// A phone subscribed to our server; [`Self::touch`] on each write.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn inbound(tx: tokio::sync::mpsc::Sender<Vec<u8>>, limit: Arc<LinkLimit>) -> Self {
        Self { heard: Some(Arc::new(Heard::new())), ..Self::new(tx, limit) }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn touch(&self) {
        if let Some(heard) = &self.heard {
            heard.touch();
        }
    }

    fn entry(&self, id: &str) -> LinkEntry {
        LinkEntry {
            id: id.to_string(),
            inbound: id.starts_with("in:"),
            max_write: self.limit.max_write(),
            rssi: None,
            up_for: self.since.elapsed(),
            heard_ago: self.heard.as_ref().map(|h| h.quiet_for()),
            frames_out: self.frames_out,
            bytes_out: self.bytes_out,
        }
    }
}

/// A link's receive clock, touched by its notification task without
/// taking the link table lock.
struct Heard {
    start: Instant,
    last_ms: AtomicU64,
}

impl Heard {
    fn new() -> Self {
        Self { start: Instant::now(), last_ms: AtomicU64::new(0) }
    }

    fn touch(&self) {
        let ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_ms.store(ms, Ordering::Relaxed);
    }

    fn quiet_for(&self) -> Duration {
        self.start.elapsed().saturating_sub(Duration::from_millis(self.last_ms.load(Ordering::Relaxed)))
    }
}

pub struct BleCentral {
    adapter: Adapter,
    pub(crate) links: Arc<Mutex<HashMap<String, Link>>>,
    inflight: Arc<Mutex<HashSet<String>>>,
    backoff: Arc<Mutex<AttachBackoff>>,
    denials: Mutex<DenialWatch>,
    /// Phones that connected but timed out, for [`denials::STALL_HINT`].
    stalls: Mutex<DenialWatch>,
    /// Phones that timed out on a connection Windows already held.
    jammed: Mutex<DenialWatch>,
    scan_alive: AtomicBool,
    low_power: AtomicBool,
    /// Set by [`Self::shutdown`]: no more scanning or attaching.
    stopping: AtomicBool,
    pub(crate) events: broadcast::Sender<BleEvent>,
    pub(crate) include_testnet: bool,
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) server: std::sync::Mutex<Option<crate::peripheral::Server>>,
}

/// Name and power state of the first adapter, without scanning.
pub async fn probe_adapter() -> Result<String, String> {
    let manager = Manager::new().await.map_err(|e| e.to_string())?;
    let adapters = manager.adapters().await.map_err(|e| e.to_string())?;
    let count = adapters.len();
    let adapter = adapters.into_iter().next().ok_or("no Bluetooth adapter")?;
    let info = adapter.adapter_info().await.unwrap_or_else(|_| "unknown".into());
    let state = adapter.adapter_state().await.unwrap_or(CentralState::Unknown);
    Ok(format!("{info} ({state:?}, {count} adapter(s))"))
}

impl BleCentral {
    pub async fn connect_adapter() -> Result<Self, String> {
        let manager = Manager::new().await.map_err(|e| e.to_string())?;
        let adapters = manager.adapters().await.map_err(|e| e.to_string())?;
        let count = adapters.len();
        let adapter = adapters.into_iter().next().ok_or("no Bluetooth adapter")?;
        let info = adapter.adapter_info().await.unwrap_or_else(|_| "unknown".into());
        info!(adapter = %info, count, "opened Bluetooth adapter");
        let (events, _) = broadcast::channel(256);
        let _ = events.send(BleEvent::Adapter(info));
        Ok(Self {
            adapter,
            links: Arc::new(Mutex::new(HashMap::new())),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            backoff: Arc::new(Mutex::new(AttachBackoff::default())),
            denials: Mutex::new(DenialWatch::default()),
            stalls: Mutex::new(DenialWatch::needing(STALLED_PHONES)),
            jammed: Mutex::new(DenialWatch::needing(1)),
            scan_alive: AtomicBool::new(false),
            low_power: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            events,
            include_testnet: false,
            #[cfg(any(windows, target_os = "linux"))]
            server: std::sync::Mutex::new(None),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BleEvent> {
        self.events.subscribe()
    }

    pub fn include_testnet(&mut self, yes: bool) {
        self.include_testnet = yes;
    }

    /// Scan in windows instead of continuously (on battery). Takes effect at
    /// the next scan check.
    pub fn set_low_power(&self, on: bool) {
        if self.low_power.swap(on, Ordering::SeqCst) != on {
            info!(low_power = on, "BLE scan duty cycle {}", if on { "on" } else { "off" });
        }
    }

    fn service_uuids(&self) -> Vec<Uuid> {
        // Always scan both: store/release phones use mainnet, debug builds use testnet.
        let mut ids = vec![SERVICE_UUID_MAINNET, SERVICE_UUID_TESTNET];
        if self.include_testnet {
            ids.push(SERVICE_UUID_TESTNET);
        }
        ids.sort();
        ids.dedup();
        ids
    }

    async fn radio_state(&self) -> CentralState {
        self.adapter.adapter_state().await.unwrap_or(CentralState::Unknown)
    }

    async fn start_scan_now(&self) -> Result<(), String> {
        // Windows UUID filters drop iOS/Android ads that put the service in
        // the scan response. Scan everything, match in software.
        let filter = if cfg!(windows) {
            debug!("Windows BLE scan is unfiltered; matching bitchat UUIDs in software");
            ScanFilter::default()
        } else {
            ScanFilter { services: self.service_uuids() }
        };
        match self.adapter.start_scan(filter).await {
            Ok(()) => Ok(()),
            Err(err) => {
                let msg = err.to_string();
                if is_already_scanning(&msg) {
                    Ok(())
                } else {
                    Err(msg)
                }
            }
        }
    }

    /// One-shot scan. Prefer [`Self::maintain_scan`] in the app so a powered-off
    /// radio is retried instead of treated as a fatal start-up error.
    pub async fn scan_and_connect(&self) -> Result<(), String> {
        self.start_scan_now().await.map_err(|e| explain_scan_error(&e))
    }

    /// Watch the adapter forever: if the user turns Bluetooth off, wait; when it
    /// comes back, start scanning again without requiring an app restart.
    pub async fn maintain_scan(&self) {
        if let Ok(info) = self.adapter.adapter_info().await {
            let _ = self.events.send(BleEvent::Adapter(info));
        }

        let mut scanning = false;
        let mut scan_started = Instant::now();
        let mut last_ready: Option<bool> = None;
        let mut delay = Duration::from_millis(400);

        loop {
            if self.stopping.load(Ordering::SeqCst) {
                let _ = self.adapter.stop_scan().await;
                return;
            }
            let state = self.radio_state().await;
            if scanning && !self.scan_alive.load(Ordering::SeqCst) {
                scanning = false;
                let _ = self.adapter.stop_scan().await;
            }
            if matches!(state, CentralState::PoweredOff) {
                if scanning {
                    let _ = self.adapter.stop_scan().await;
                    scanning = false;
                }
                self.scan_alive.store(false, Ordering::SeqCst);
                self.drop_all_links().await;
                if last_ready != Some(false) {
                    last_ready = Some(false);
                    let _ = self.events.send(BleEvent::Radio { ready: false, detail: radio_off_hint().into() });
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                delay = Duration::from_millis(400);
                continue;
            }

            if scanning {
                match next_scan_step(self.low_power.load(Ordering::SeqCst), scan_started.elapsed()) {
                    ScanStep::Keep(wait) => tokio::time::sleep(wait).await,
                    ScanStep::Rest(rest) => {
                        let _ = self.adapter.stop_scan().await;
                        scanning = false;
                        trace!(rest_s = rest.as_secs(), "BLE scan resting");
                        tokio::time::sleep(rest).await;
                    }
                }
                continue;
            }

            match self.start_scan_now().await {
                Ok(()) => {
                    scanning = true;
                    scan_started = Instant::now();
                    self.scan_alive.store(true, Ordering::SeqCst);
                    delay = Duration::from_millis(400);
                    if last_ready != Some(true) {
                        last_ready = Some(true);
                        let radio = self.radio_state().await;
                        info!(?radio, "scanning for official bitchat service UUID");
                        let _ = self.events.send(BleEvent::Radio {
                            ready: true,
                            detail: "BLE scan started for official service UUID".into(),
                        });
                    }
                }
                Err(err) => {
                    scanning = false;
                    self.scan_alive.store(false, Ordering::SeqCst);
                    self.drop_all_links().await;
                    if last_ready != Some(false) {
                        last_ready = Some(false);
                        let _ = self.events.send(BleEvent::Radio { ready: false, detail: explain_scan_error(&err) });
                    }
                    if !is_retryable_scan_error(&err) {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    } else {
                        tokio::time::sleep(delay).await;
                        delay = (delay.saturating_mul(2)).min(Duration::from_secs(10));
                    }
                }
            }
        }
    }

    async fn drop_all_links(&self) {
        let ids: Vec<String> = {
            let mut links = self.links.lock().await;
            links.drain().map(|(id, _)| id).collect()
        };
        self.inflight.lock().await.clear();
        for id in ids {
            let _ = self.events.send(BleEvent::Disconnected(id));
        }
    }

    pub async fn pump(&self) -> Result<(), String> {
        let mut last_census = Instant::now() - Duration::from_secs(10);
        let mut deaf = DeafWatch::default();
        loop {
            tokio::time::sleep(Duration::from_millis(800)).await;
            if self.stopping.load(Ordering::SeqCst) {
                return Ok(());
            }
            let peripherals = match self.adapter.peripherals().await {
                Ok(list) => list,
                Err(err) => {
                    warn!("peripheral list: {err}");
                    self.scan_alive.store(false, Ordering::SeqCst);
                    continue;
                }
            };
            if last_census.elapsed() >= CENSUS_EVERY {
                last_census = Instant::now();
                let report = self.census(&peripherals).await;
                debug!(ads = report.ads_seen, bitchat = report.bitchat.len(), links = report.links.len(), "BLE census");
                match deaf.check(report.scanning, report.ads_seen, Instant::now()) {
                    DeafStep::Fine => {}
                    DeafStep::RestartScan => {
                        warn!("BLE scan hears no devices at all; restarting it");
                        self.scan_alive.store(false, Ordering::SeqCst);
                    }
                    DeafStep::Warn => {
                        warn!("BLE scan still hears no devices after a restart; telling the user");
                        let _ = self.events.send(BleEvent::NeedsReset(DEAF_HINT.into()));
                    }
                }
                let _ = self.events.send(BleEvent::Report(report));
                self.drop_silent_links().await;
            }
            for p in peripherals {
                let id = p.id().to_string();
                {
                    // A phone already subscribed to our GATT server carries
                    // traffic both ways; a second, outbound link only doubles it.
                    let links = self.links.lock().await;
                    if links.contains_key(&id) || links.contains_key(&format!("in:{id}")) {
                        continue;
                    }
                }
                if self.inflight.lock().await.contains(&id) {
                    continue;
                }
                if !self.backoff.lock().await.ready(&id, Instant::now()) {
                    continue;
                }
                let nlinks = self.links.lock().await.len();
                let nfly = self.inflight.lock().await.len();
                if nlinks >= MAX_LINKS || nfly >= MAX_INFLIGHT || nlinks + nfly >= MAX_LINKS {
                    continue;
                }
                let props = p.properties().await.ok().flatten();
                let name = props.as_ref().and_then(|pr| pr.local_name.clone()).unwrap_or_else(|| id.clone());
                let wanted = self.service_uuids();
                if !advertisement_matches(props.as_ref(), &wanted) {
                    continue;
                }
                info!(%id, %name, "bitchat advertisement matched; attaching");
                let _ = self.events.send(BleEvent::PeerFound { id: id.clone(), name: name.clone() });
                if let Err(err) = self.attach(p.clone(), id.clone(), name).await {
                    // Never leave a half-open GATT connection behind.
                    let _ = p.disconnect().await;
                    let now = Instant::now();
                    let wait = if err.contains(gatt_probe::NO_SERVICE) {
                        self.backoff.lock().await.failed_without_service(&id, now)
                    } else if err.contains(gatt_probe::ACCESS_DENIED) {
                        if self.denials.lock().await.denied(&id, now) {
                            warn!("several phones refused access; telling the user");
                            let _ = self.events.send(BleEvent::Error(denials::HINT.into()));
                        }
                        self.backoff.lock().await.busy(&id, now)
                    } else {
                        let jammed = err.ends_with(LEFTOVER) && self.jammed.lock().await.denied(&id, now);
                        let stalled = err.contains("timed out")
                            && self.links.lock().await.is_empty()
                            && self.stalls.lock().await.denied(&id, now);
                        if jammed || stalled {
                            warn!(jammed, stalled, "phones in range connect but never answer; offering a reset");
                            let _ = self.events.send(BleEvent::NeedsReset(denials::STALL_HINT.into()));
                        }
                        self.backoff.lock().await.failed(&id, now)
                    };
                    info!(%id, %err, retry_in_s = wait.as_secs(), "attach failed; backing off");
                    if !is_expected_attach_error(&err) {
                        let _ = self.events.send(BleEvent::Error(err));
                    }
                    let _ = self.events.send(BleEvent::Disconnected(id));
                }
            }
        }
    }

    async fn attach(&self, peripheral: Peripheral, id: String, name: String) -> Result<(), String> {
        {
            let mut inflight = self.inflight.lock().await;
            if !inflight.insert(id.clone()) {
                return Ok(());
            }
        }
        // A phone that connected in to our GATT server also shows connected,
        // so this only means something when the attach then times out.
        let held = peripheral.is_connected().await.unwrap_or(false);
        let result = self.attach_inner(peripheral, id.clone(), name).await;
        self.inflight.lock().await.remove(&id);
        result.map_err(|err| if held && err.contains("timed out") { format!("{err}{LEFTOVER}") } else { err })
    }

    async fn attach_inner(&self, peripheral: Peripheral, id: String, name: String) -> Result<(), String> {
        info!(%id, %name, "GATT connect");
        match tokio::time::timeout(Duration::from_secs(12), peripheral.connect()).await {
            Ok(Ok(())) => info!(%id, "connected"),
            Ok(Err(err)) => return Err(format!("connect {name}: {err}")),
            Err(_) => {
                let _ = peripheral.disconnect().await;
                return Err(format!("connect {name}: timed out"));
            }
        }
        // iOS/Android often need a beat after connect before the GATT database is visible.
        tokio::time::sleep(Duration::from_millis(250)).await;
        let mut characteristic = None;
        for attempt in 0..3 {
            if let Err(err) = discover(&peripheral).await {
                if attempt == 2 {
                    return Err(format!("discover {name}: {err}"));
                }
            } else if let Some(found) = peripheral.characteristics().into_iter().find(|c| c.uuid == CHARACTERISTIC_UUID)
            {
                characteristic = Some(found);
                break;
            }
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
        let characteristic = match characteristic {
            Some(found) => found,
            None => recheck_characteristic(&peripheral, &id, &name).await?,
        };
        if characteristic.properties.contains(CharPropFlags::NOTIFY)
            || characteristic.properties.contains(CharPropFlags::INDICATE)
        {
            match tokio::time::timeout(SUBSCRIBE_TIMEOUT, peripheral.subscribe(&characteristic)).await {
                Ok(Ok(())) => info!(%name, "subscribed to notifications"),
                Ok(Err(err)) => warn!(%name, "subscribe failed: {err}"),
                Err(_) => return Err(format!("subscribe {name}: timed out")),
            }
        }
        let heard = Arc::new(Heard::new());
        if let Ok(mut notifications) = peripheral.notifications().await {
            let events = self.events.clone();
            let links = self.links.clone();
            let from = id.clone();
            let heard = heard.clone();
            tokio::spawn(async move {
                let mut assembler = NotificationStreamAssembler::new();
                while let Some(n) = notifications.next().await {
                    heard.touch();
                    let assembled = assembler.append(&n.value);
                    for data in assembled.frames {
                        debug!(from = %from, bytes = data.len(), "assembled mesh frame");
                        let _ = events.send(BleEvent::Frame { from: from.clone(), data });
                    }
                }
                links.lock().await.remove(&from);
                let _ = events.send(BleEvent::Disconnected(from));
            });
        }
        let limit = Arc::new(LinkLimit::open(&id).await);
        if self.stopping.load(Ordering::SeqCst) {
            let _ = peripheral.disconnect().await;
            return Err(format!("{name}: shutting down"));
        }
        let ctx = WriterCtx { links: self.links.clone(), backoff: self.backoff.clone(), events: self.events.clone() };
        let tx = writer::spawn(id.clone(), peripheral.clone(), characteristic, limit.clone(), ctx);
        self.links.lock().await.insert(id.clone(), Link::outbound(tx, limit, heard, peripheral));
        self.backoff.lock().await.succeeded(&id);
        self.denials.lock().await.linked();
        self.stalls.lock().await.linked();
        self.jammed.lock().await.linked();
        let _ = self.events.send(BleEvent::Connected { id, name });
        Ok(())
    }

    /// Queue a frame on every live link not in `skip`, waiting while a
    /// queue is full so long files are paced by the radio. Returns how many
    /// links took it.
    pub async fn broadcast(&self, frame: &[u8], skip: &[String]) -> Result<usize, String> {
        let links: Vec<(String, tokio::sync::mpsc::Sender<Vec<u8>>)> = self
            .links
            .lock()
            .await
            .iter()
            .filter(|(id, _)| !skip.contains(id))
            .map(|(id, l)| (id.clone(), l.tx.clone()))
            .collect();
        let mut queued = 0;
        for (id, tx) in links {
            match tx.send_timeout(frame.to_vec(), LINK_STALL).await {
                Ok(()) => {
                    queued += 1;
                    if let Some(link) = self.links.lock().await.get_mut(&id) {
                        link.frames_out += 1;
                        link.bytes_out += frame.len() as u64;
                    }
                }
                Err(tokio::sync::mpsc::error::SendTimeoutError::Timeout(_)) => {
                    warn!(%id, "link stalled; frame dropped");
                }
                Err(tokio::sync::mpsc::error::SendTimeoutError::Closed(_)) => {}
            }
        }
        Ok(queued)
    }

    /// The smallest negotiated single-write size across live links, so
    /// fragments can be cut to fit one radio packet. None until known.
    pub async fn write_limit(&self) -> Option<usize> {
        self.links.lock().await.values().filter_map(|l| l.limit.max_write()).min()
    }

    pub async fn connected_count(&self) -> usize {
        self.links.lock().await.len()
    }

    /// Drop an outbound link; the scan loop may reattach after the usual
    /// cooldown. Phones connected in cannot be dropped from our side.
    pub async fn disconnect(&self, id: &str) -> bool {
        if id.starts_with("in:") || self.links.lock().await.remove(id).is_none() {
            return false;
        }
        self.backoff.lock().await.dropped(id, Instant::now());
        info!(%id, "BLE link dropped on request");
        let _ = self.events.send(BleEvent::Disconnected(id.to_string()));
        true
    }

    /// Drop links the phone stopped sending on (see [`SILENT_LINK`]).
    /// Outbound writers then disconnect, and the phone is retried after
    /// [`SILENT_COOLDOWN`]. A phone connected in stays subscribed as far as
    /// Windows knows, but leaving its link would block an outbound attach
    /// to the same address forever; it comes back if it writes again.
    async fn drop_silent_links(&self) {
        let silent = take_silent(&mut *self.links.lock().await);
        let now = Instant::now();
        for (id, quiet) in silent {
            if id.starts_with("in:") {
                info!(%id, quiet_s = quiet.as_secs(), "phone connected in went silent; dropping its link");
            } else {
                self.backoff.lock().await.hold(&id, now, SILENT_COOLDOWN);
                info!(%id, quiet_s = quiet.as_secs(), "BLE link went silent; dropping it");
            }
            let _ = self.events.send(BleEvent::Disconnected(id));
        }
    }

    /// Drop an outbound link that duplicates a newer one to the same peer
    /// (phones rotate their address, and Windows sees each as a new device).
    /// The old address is left alone for a while in case it still advertises.
    pub async fn retire_duplicate(&self, id: &str) -> bool {
        if id.starts_with("in:") || self.links.lock().await.remove(id).is_none() {
            return false;
        }
        self.backoff.lock().await.hold(id, Instant::now(), DUPLICATE_HOLD);
        info!(%id, "BLE link retired: same peer on a newer link");
        let _ = self.events.send(BleEvent::Disconnected(id.to_string()));
        true
    }

    /// Release the radio before the process exits or the radio is reset:
    /// stop scanning and advertising, drop the GATT server and close our
    /// links. Windows may still keep a phone's connection open afterwards;
    /// the log line counts those, since they jam the next run.
    /// Returns the phones the OS still reports connected afterwards.
    pub async fn shutdown(&self) -> Vec<String> {
        self.stopping.store(true, Ordering::SeqCst);
        self.scan_alive.store(false, Ordering::SeqCst);
        let _ = self.adapter.stop_scan().await;
        #[cfg(any(windows, target_os = "linux"))]
        let advertised = self.server.lock().unwrap_or_else(|p| p.into_inner()).take().is_some();
        #[cfg(not(any(windows, target_os = "linux")))]
        let advertised = false;
        let links: Vec<(String, Link)> = self.links.lock().await.drain().collect();
        let ids: Vec<String> = links.iter().map(|(id, _)| id.clone()).collect();
        let peripherals: Vec<Peripheral> = links.iter().filter_map(|(_, l)| l.peripheral.clone()).collect();
        drop(links);
        let closing = peripherals.iter().map(|p| tokio::time::timeout(SHUTDOWN_DISCONNECT, p.disconnect()));
        futures::future::join_all(closing).await;
        drop(peripherals);
        let links = ids.len();
        let held = tokio::task::spawn_blocking(move || held_by_os(&ids)).await.unwrap_or_default();
        info!(advertised, links, still_connected = held.len(), "BLE released for exit");
        held
    }

    /// Forget an attach backoff so the next scan pass tries the device.
    pub async fn retry_now(&self, id: &str) {
        self.backoff.lock().await.succeeded(id);
        info!(%id, "BLE attach retry requested");
    }

    async fn census(&self, peripherals: &[Peripheral]) -> RadioReport {
        let wanted = self.service_uuids();
        let mut report = RadioReport {
            scanning: self.scan_alive.load(Ordering::SeqCst),
            low_power: self.low_power.load(Ordering::SeqCst),
            ads_seen: peripherals.len(),
            max_links: MAX_LINKS,
            attaching: self.inflight.lock().await.len(),
            links: self.links.lock().await.iter().map(|(id, l)| l.entry(id)).collect(),
            bitchat: Vec::new(),
        };
        report.links.sort_by(|a, b| a.id.cmp(&b.id));
        let now = Instant::now();
        for p in peripherals {
            let id = p.id().to_string();
            let props = p.properties().await.ok().flatten();
            let name = props.as_ref().and_then(|pr| pr.local_name.clone());
            let rssi = props.as_ref().and_then(|pr| pr.rssi);
            if !advertisement_matches(props.as_ref(), &wanted) {
                trace!(%id, ?name, ?rssi, "advertisement (not bitchat)");
                continue;
            }
            debug!(%id, ?name, ?rssi, "bitchat advertisement");
            let linked = report.links.iter().any(|l| l.id == id || l.id == format!("in:{id}"));
            let retry_in = self.backoff.lock().await.wait(&id, now);
            report.bitchat.push(ScanEntry { id, name, rssi, linked, retry_in });
        }
        report.join_rssi();
        report
    }
}

/// Discovery came back without our characteristic. Ask the phone why (see
/// `gatt_probe`); if it was only busy, give it one more chance before the
/// attach backs off.
async fn recheck_characteristic(peripheral: &Peripheral, id: &str, name: &str) -> Result<Characteristic, String> {
    let seen: Vec<String> =
        peripheral.services().iter().map(|s| format!("{}({})", s.uuid, s.characteristics.len())).collect();
    let mut probe = probe_within(id, SERVICE_UUID_MAINNET).await;
    if matches!(&probe, Probe::Missing(d) if d == gatt_probe::NO_SERVICE) {
        probe = probe_within(id, SERVICE_UUID_TESTNET).await;
    }
    info!(%id, ?seen, probe = probe.detail(), "bitchat characteristic not found; GATT probe");
    if matches!(probe, Probe::Ready | Probe::Unreachable(_)) {
        tokio::time::sleep(LATE_DISCOVERY).await;
        if discover(peripheral).await.is_ok() {
            if let Some(found) = peripheral.characteristics().into_iter().find(|c| c.uuid == CHARACTERISTIC_UUID) {
                info!(%id, "bitchat characteristic found on late retry");
                return Ok(found);
            }
        }
    }
    Err(format!("{name} missing bitchat characteristic ({})", probe.detail()))
}

/// Windows GATT queries against a phone that stopped answering run ~30 s
/// each; attaches are serial, so one such phone would stall every other.
/// The links' phones the OS still reports connected: a connection outliving
/// witchat is what jams the next run.
fn held_by_os(ids: &[String]) -> Vec<String> {
    let mut phones: Vec<&str> = ids.iter().map(|id| id.trim_start_matches("in:")).collect();
    phones.sort_unstable();
    phones.dedup();
    phones.into_iter().filter(|id| crate::radio::os_connected(id) == Some(true)).map(str::to_string).collect()
}

async fn discover(peripheral: &Peripheral) -> Result<(), String> {
    match tokio::time::timeout(DISCOVER_TIMEOUT, peripheral.discover_services()).await {
        Ok(result) => result.map_err(|e| e.to_string()),
        Err(_) => Err("service discovery timed out".into()),
    }
}

async fn probe_within(id: &str, service: Uuid) -> Probe {
    tokio::time::timeout(DISCOVER_TIMEOUT, gatt_probe::probe(id, service, CHARACTERISTIC_UUID))
        .await
        .unwrap_or_else(|_| Probe::Unreachable("service query: timed out".into()))
}

fn take_silent(links: &mut HashMap<String, Link>) -> Vec<(String, Duration)> {
    let found: Vec<(String, Duration)> = links
        .iter()
        .filter_map(|(id, l)| l.heard.as_ref().map(|h| (id.clone(), h.quiet_for())))
        .filter(|(_, quiet)| *quiet >= SILENT_LINK)
        .collect();
    for (id, _) in &found {
        links.remove(id);
    }
    found
}

fn advertisement_matches(props: Option<&btleplug::api::PeripheralProperties>, wanted: &[Uuid]) -> bool {
    let Some(pr) = props else {
        return false;
    };
    pr.services.iter().any(|s| wanted.contains(s)) || pr.service_data.keys().any(|s| wanted.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_mains_the_scan_never_rests() {
        assert_eq!(next_scan_step(false, Duration::from_secs(3600)), ScanStep::Keep(SCAN_RECHECK));
    }

    #[test]
    fn on_battery_the_scan_rests_after_each_window() {
        assert_eq!(next_scan_step(true, Duration::ZERO), ScanStep::Keep(SCAN_RECHECK));
        assert_eq!(next_scan_step(true, Duration::from_millis(4200)), ScanStep::Keep(Duration::from_millis(800)));
        assert_eq!(next_scan_step(true, DUTY_SCAN), ScanStep::Rest(DUTY_REST));
        assert_eq!(next_scan_step(true, Duration::from_secs(60)), ScanStep::Rest(DUTY_REST));
    }

    #[test]
    fn a_deaf_scan_is_restarted_once_then_reported_once() {
        let t = Instant::now();
        let mut deaf = DeafWatch::default();
        assert_eq!(deaf.check(true, 0, t), DeafStep::Fine);
        assert_eq!(deaf.check(true, 0, t + DEAF_RESTART), DeafStep::RestartScan);
        assert_eq!(deaf.check(true, 0, t + DEAF_RESTART + Duration::from_secs(5)), DeafStep::Fine);
        assert_eq!(deaf.check(true, 0, t + DEAF_WARN), DeafStep::Warn);
        assert_eq!(deaf.check(true, 0, t + DEAF_WARN * 3), DeafStep::Fine);
        assert_eq!(deaf.check(true, 12, t + DEAF_WARN * 4), DeafStep::Fine);
        assert_eq!(deaf.check(true, 0, t + DEAF_WARN * 5), DeafStep::Fine, "hearing again resets the clock");
        assert_eq!(deaf.check(false, 0, t + DEAF_WARN * 9), DeafStep::Fine, "not scanning is not deaf");
    }

    fn link(heard: Option<Duration>) -> Link {
        let (tx, _) = tokio::sync::mpsc::channel(1);
        let mut link = Link::new(tx, Arc::new(LinkLimit::unknown()));
        link.heard = heard.map(|quiet| {
            let start = Instant::now().checked_sub(quiet).expect("clock too young for the test");
            Arc::new(Heard { start, last_ms: AtomicU64::new(0) })
        });
        link
    }

    #[test]
    fn silent_links_are_dropped_in_either_direction() {
        let quiet = SILENT_LINK + Duration::from_secs(5);
        let mut links: HashMap<String, Link> = [
            ("AA:00:00:00:00:01".to_string(), link(Some(quiet))),
            ("in:AA:00:00:00:00:02".to_string(), link(Some(quiet))),
            ("in:AA:00:00:00:00:03".to_string(), link(Some(Duration::from_secs(10)))),
            ("shared".to_string(), link(None)),
        ]
        .into_iter()
        .collect();
        let mut dropped: Vec<String> = take_silent(&mut links).into_iter().map(|(id, _)| id).collect();
        dropped.sort();
        assert_eq!(dropped, ["AA:00:00:00:00:01", "in:AA:00:00:00:00:02"]);
        let mut left: Vec<&String> = links.keys().collect();
        left.sort();
        assert_eq!(left, ["in:AA:00:00:00:00:03", "shared"]);
    }

    #[test]
    fn a_write_resets_an_inbound_link_clock() {
        let link = link(Some(SILENT_LINK * 2));
        link.touch();
        assert!(link.heard.as_ref().unwrap().quiet_for() < Duration::from_secs(1));
    }
}
