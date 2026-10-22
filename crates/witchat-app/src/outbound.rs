//! Background BLE writer. GATT writes can take hundreds of milliseconds per
//! link, so the mesh loop only enqueues frames here and keeps reading.
//!
//! Two lanes: control traffic (handshakes, acks, DMs, announces, relays) and
//! bulk transfers (files). A file goes out one fragment at a time and any
//! control packet that arrives meanwhile is sent before the next fragment, so
//! a 1 MiB photo can never starve a handshake or a delivery ack.
//!
//! Packets are cut into frames only when they are about to go out, sized to
//! the links' negotiated write size so each frame is one radio packet.

use crate::fanout::{self, Plan};
use crate::link_bindings::LinkBindings;
use crate::mesh_worker::UiEvent;
use crate::relay_spool::RelaySpool;
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use witchat_ble::BleCentral;
use witchat_core::frames_for_link;
use witchat_protocol::{MessageType, Packet};

/// Packets larger than this (about 16 default fragments) ride the bulk lane.
const BULK_BYTES: usize = 16 * 469;
/// Progress events per transfer (every 5 %).
const PROGRESS_STEPS: usize = 20;

/// Where frames go: the live BLE links, or a test double.
pub(crate) trait FrameSink: Send + Sync + 'static {
    /// Send on every link not in `skip`; returns how many accepted.
    fn broadcast(&self, frame: &[u8], skip: &[String]) -> impl Future<Output = usize> + Send;
    /// Smallest single-write size across links, once negotiated.
    fn write_limit(&self) -> impl Future<Output = Option<usize>> + Send;
}

impl FrameSink for BleCentral {
    async fn broadcast(&self, frame: &[u8], skip: &[String]) -> usize {
        BleCentral::broadcast(self, frame, skip).await.unwrap_or(0)
    }

    async fn write_limit(&self) -> Option<usize> {
        BleCentral::write_limit(self).await
    }
}

struct Job {
    packet: Packet,
    plan: Plan,
    /// Tell the user when no live link accepted the packet.
    report: bool,
    /// Chat line whose send progress the UI shows.
    progress: Option<String>,
}

impl Job {
    fn skip(&self, frames: usize) -> Vec<String> {
        let mut skip = self.plan.skip.clone();
        if frames == 1 {
            skip.extend(self.plan.skip_single.iter().cloned());
        }
        skip
    }
}

/// What we queued for the radio since start, by packet type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendStats {
    pub queued: BTreeMap<&'static str, u64>,
    /// Packets that reached no link (none was up).
    pub no_link: u64,
    /// Directed packets held for a link (see `relay_spool`).
    pub spooled: u64,
    /// Held packets sent once a link came up.
    pub spool_sent: u64,
}

#[derive(Clone)]
pub struct Outbound {
    lanes: Option<(UnboundedSender<Job>, UnboundedSender<Job>)>,
    evt_tx: Sender<UiEvent>,
    central: Option<Arc<BleCentral>>,
    stats: Arc<Mutex<SendStats>>,
    links: Arc<Mutex<LinkBindings>>,
    spool: Arc<Mutex<RelaySpool>>,
}

impl Outbound {
    pub fn spawn(central: Option<Arc<BleCentral>>, evt_tx: Sender<UiEvent>) -> Self {
        let stats = Arc::new(Mutex::new(SendStats::default()));
        let (links, spool) = (Arc::default(), Arc::default());
        let Some(radio) = central.clone() else {
            return Self { lanes: None, evt_tx, central, stats, links, spool };
        };
        let (urgent_tx, urgent_rx) = unbounded_channel();
        let (bulk_tx, bulk_rx) = unbounded_channel();
        tokio::spawn(run(radio, urgent_rx, bulk_rx, evt_tx.clone(), stats.clone()));
        Self { lanes: Some((urgent_tx, bulk_tx)), evt_tx, central, stats, links, spool }
    }

    pub fn central(&self) -> Option<&Arc<BleCentral>> {
        self.central.as_ref()
    }

    pub fn stats(&self) -> SendStats {
        self.stats.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Which peer each live link reaches; the worker keeps it current.
    pub fn links(&self) -> MutexGuard<'_, LinkBindings> {
        self.links.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Queue a protocol packet (announce, handshake, sync).
    pub fn send(&self, packet: &Packet) {
        self.enqueue(packet, None, false, None);
    }

    /// Queue a packet to relay; it skips the link it arrived on and every
    /// other link to the same peer.
    pub fn relay(&self, packet: &Packet, ingress: &str) {
        self.enqueue(packet, Some(ingress), false, None);
    }

    /// Queue a packet the user sent; surfaces an error if nothing was linked.
    pub fn send_reported(&self, packet: &Packet) {
        self.enqueue(packet, None, true, None);
    }

    /// Queue a file; the UI shows progress on the chat line `line_id`.
    pub fn send_file(&self, packet: &Packet, line_id: &str) {
        self.enqueue(packet, None, true, Some(line_id.to_string()));
    }

    /// A link came up: send what was held for one, if still fresh.
    pub fn flush_spool(&self) {
        let held = self.spool.lock().map(|mut s| s.drain(Instant::now())).unwrap_or_default();
        if !held.is_empty() {
            tracing::info!(packets = held.len(), "relay spool: sending held directed packets");
            if let Ok(mut stats) = self.stats.lock() {
                stats.spool_sent += held.len() as u64;
            }
        }
        for packet in &held {
            self.send(packet);
        }
    }

    pub fn prune_spool(&self) {
        if let Ok(mut spool) = self.spool.lock() {
            spool.prune(Instant::now());
        }
    }

    fn enqueue(&self, packet: &Packet, ingress: Option<&str>, report: bool, progress: Option<String>) {
        let type_name = MessageType::from_u8(packet.type_id).map_or("unknown", |t| t.as_str());
        let plan = fanout::plan(packet, &self.links().views(), ingress);
        if plan.spool {
            self.hold(packet);
        }
        if let Ok(mut stats) = self.stats.lock() {
            *stats.queued.entry(type_name).or_default() += 1;
            if self.lanes.is_none() {
                stats.no_link += 1;
            }
        }
        let Some((urgent, bulk)) = &self.lanes else {
            if report {
                report_undelivered(&self.evt_tx);
            }
            return;
        };
        let large = packet.encode(false).is_some_and(|b| b.len() > BULK_BYTES);
        let lane = if progress.is_some() || large { bulk } else { urgent };
        let _ = lane.send(Job { packet: packet.clone(), plan, report, progress });
    }

    fn hold(&self, packet: &Packet) {
        let Some(recipient) = fanout::recipient(packet) else { return };
        let held = self
            .spool
            .lock()
            .is_ok_and(|mut s| s.hold(packet, recipient.clone(), fanout::message_id(packet), Instant::now()));
        if held {
            tracing::debug!(peer = %recipient, ty = packet.type_id, "relay spool: holding directed packet until a link comes up");
            if let Ok(mut stats) = self.stats.lock() {
                stats.spooled += 1;
            }
        }
    }
}

/// A file part-way out. Frames are cut when it reaches the front.
struct Transfer {
    job: Job,
    frames: Option<Vec<Vec<u8>>>,
    next: usize,
    delivered: usize,
    reported_step: usize,
    started: Instant,
}

impl Transfer {
    fn new(job: Job) -> Self {
        Self { job, frames: None, next: 0, delivered: 0, reported_step: 0, started: Instant::now() }
    }
}

async fn run<S: FrameSink>(
    sink: Arc<S>,
    mut urgent: UnboundedReceiver<Job>,
    mut bulk_rx: UnboundedReceiver<Job>,
    evt_tx: Sender<UiEvent>,
    stats: Arc<Mutex<SendStats>>,
) {
    let mut bulk: VecDeque<Transfer> = VecDeque::new();
    loop {
        while let Ok(job) = urgent.try_recv() {
            send_all(&*sink, &job, &evt_tx, &stats).await;
        }
        while let Ok(job) = bulk_rx.try_recv() {
            bulk.push_back(Transfer::new(job));
        }
        if let Some(transfer) = bulk.front_mut() {
            if step(&*sink, transfer, &evt_tx).await {
                bulk.pop_front();
            }
            continue;
        }
        tokio::select! {
            biased;
            job = urgent.recv() => match job {
                Some(job) => send_all(&*sink, &job, &evt_tx, &stats).await,
                None => break,
            },
            job = bulk_rx.recv() => match job {
                Some(job) => bulk.push_back(Transfer::new(job)),
                None => break,
            },
        }
    }
}

async fn send_all<S: FrameSink>(sink: &S, job: &Job, evt_tx: &Sender<UiEvent>, stats: &Mutex<SendStats>) {
    let mut delivered = 0;
    let frames = frames_for_link(&job.packet, sink.write_limit().await);
    let skip = job.skip(frames.len());
    for frame in frames {
        delivered = delivered.max(sink.broadcast(&frame, &skip).await);
    }
    if delivered == 0 {
        if let Ok(mut stats) = stats.lock() {
            stats.no_link += 1;
        }
        if job.report {
            report_undelivered(evt_tx);
        }
    }
}

/// Send one fragment of a transfer. Returns true when it is finished.
async fn step<S: FrameSink>(sink: &S, t: &mut Transfer, evt_tx: &Sender<UiEvent>) -> bool {
    if t.frames.is_none() {
        let limit = sink.write_limit().await;
        let frames = frames_for_link(&t.job.packet, limit);
        tracing::info!(frames = frames.len(), link_limit = limit, "file: transfer started");
        t.frames = Some(frames);
    }
    let frames = t.frames.as_deref().unwrap_or_default();
    let total = frames.len();
    if let Some(frame) = frames.get(t.next) {
        t.delivered = t.delivered.max(sink.broadcast(frame, &t.job.skip(total)).await);
        t.next += 1;
    }
    let done = t.next >= total;
    let step_now = (t.next * PROGRESS_STEPS).checked_div(total).unwrap_or(PROGRESS_STEPS);
    if let Some(id) = &t.job.progress {
        if step_now > t.reported_step || done {
            t.reported_step = step_now;
            let percent = (step_now * 100 / PROGRESS_STEPS) as u8;
            let _ = evt_tx.send(UiEvent::Progress { id: id.clone(), percent });
        }
    }
    if done {
        let secs = t.started.elapsed().as_secs_f32();
        tracing::info!(frames = total, links = t.delivered, secs, "file: transfer finished");
        if t.job.report && t.delivered == 0 {
            report_undelivered(evt_tx);
        }
    }
    done
}

fn report_undelivered(tx: &Sender<UiEvent>) {
    let _ = tx.send(UiEvent::Error(
        "not delivered over BLE - no live GATT link. Keep the phone in range; witchat will reconnect.".into(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use witchat_protocol::{Fragment, MessageType};

    /// Records frames in send order; each takes a millisecond, like a slow radio.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<Vec<u8>>>, Mutex<Vec<Vec<String>>>);

    impl FrameSink for Recorder {
        async fn broadcast(&self, frame: &[u8], skip: &[String]) -> usize {
            tokio::time::sleep(Duration::from_millis(1)).await;
            self.0.lock().unwrap().push(frame.to_vec());
            self.1.lock().unwrap().push(skip.to_vec());
            1
        }

        async fn write_limit(&self) -> Option<usize> {
            Some(LINK_LIMIT)
        }
    }

    const LINK_LIMIT: usize = 244;

    fn packet(type_id: MessageType, len: usize) -> Packet {
        let payload = (0..len as u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        Packet::new(type_id as u8, [7; 8], payload)
    }

    #[tokio::test]
    async fn the_broadcast_subset_applies_only_to_one_frame_packets() {
        let sink = Recorder::default();
        let (evt_tx, _evt_rx) = std::sync::mpsc::channel();
        let plan = Plan { skip: vec!["in:A".into()], skip_single: vec!["B".into()], spool: false };
        for len in [40, 2_000] {
            let job =
                Job { packet: packet(MessageType::Message, len), plan: plan.clone(), report: false, progress: None };
            send_all(&sink, &job, &evt_tx, &Mutex::default()).await;
        }
        let skips = sink.1.lock().unwrap().clone();
        assert!(skips.len() > 2, "the long one went out in fragments");
        assert_eq!(skips[0], ["in:A", "B"]);
        assert!(skips[1..].iter().all(|s| s == &["in:A"]), "every fragment reaches every chosen link");
    }

    #[tokio::test]
    async fn control_packets_overtake_a_file_in_flight() {
        let sink = Arc::new(Recorder::default());
        let (urgent_tx, urgent_rx) = unbounded_channel();
        let (bulk_tx, bulk_rx) = unbounded_channel();
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(run(sink.clone(), urgent_rx, bulk_rx, evt_tx, Default::default()));

        let file = packet(MessageType::FileTransfer, 40_000);
        bulk_tx.send(Job { packet: file, plan: Plan::default(), report: true, progress: Some("f".into()) }).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let ack = packet(MessageType::NoiseEncrypted, 40);
        urgent_tx.send(Job { packet: ack, plan: Plan::default(), report: false, progress: None }).unwrap();
        let task_done = async {
            loop {
                let sent = sink.0.lock().unwrap().clone();
                let fragments =
                    sent.iter().filter_map(|f| Packet::decode(f)).filter(|p| p.type_id == MessageType::Fragment as u8);
                if fragments.filter_map(|p| Fragment::decode(&p.payload)).any(|f| f.index + 1 == f.total) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), task_done).await.unwrap();
        drop((urgent_tx, bulk_tx));
        task.await.unwrap();

        let sent: Vec<Packet> = sink.0.lock().unwrap().iter().map(|f| Packet::decode(f).unwrap()).collect();
        assert!(sink.0.lock().unwrap().iter().all(|f| f.len() <= LINK_LIMIT), "each frame fits one write");
        let ack_at = sent.iter().position(|p| p.type_id == MessageType::NoiseEncrypted as u8).unwrap();
        assert!(ack_at < 100, "ack waited behind {ack_at} file fragments");
        let order: Vec<u16> = sent
            .iter()
            .filter_map(|p| Fragment::decode(&p.payload).filter(|_| p.type_id == MessageType::Fragment as u8))
            .map(|f| f.index)
            .collect();
        assert!(order.len() > 150);
        assert!(order.windows(2).all(|w| w[1] == w[0] + 1), "fragments stay in order");

        let progress: Vec<u8> = evt_rx
            .try_iter()
            .filter_map(|e| match e {
                UiEvent::Progress { percent, .. } => Some(percent),
                _ => None,
            })
            .collect();
        assert_eq!(progress.len(), PROGRESS_STEPS);
        assert_eq!(progress.last(), Some(&100));
        assert!(progress.windows(2).all(|w| w[0] < w[1]));
    }
}
