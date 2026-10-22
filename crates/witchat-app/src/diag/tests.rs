use super::redact::{line, mask_device, short_id};
use super::*;
use witchat_ble::{LinkEntry, ScanEntry};

fn snapshot() -> Snapshot {
    let mut mesh = MeshStats::default();
    mesh.received.insert("announce", 10);
    mesh.received.insert("message", 4);
    mesh.drops.insert("duplicate", 7);
    mesh.relayed = 3;
    let mut sent = SendStats::default();
    sent.queued.insert("announce", 5);
    Snapshot {
        uptime: Duration::from_secs(3725),
        me: "0011223344556677".into(),
        adapter: Some("Intel(R) Wireless Bluetooth(R)".into()),
        advertising: true,
        role: "central + peripheral".into(),
        radio: RadioReport {
            scanning: true,
            low_power: false,
            ads_seen: 12,
            bitchat: vec![ScanEntry {
                id: "AA:BB:CC:DD:EE:FF".into(),
                name: None,
                rssi: Some(-61),
                linked: true,
                retry_in: None,
            }],
            links: vec![LinkEntry {
                id: "AA:BB:CC:DD:EE:FF".into(),
                inbound: false,
                max_write: Some(512),
                rssi: Some(-61),
                up_for: Duration::from_secs(90),
                heard_ago: Some(Duration::from_secs(4)),
                frames_out: 20,
                bytes_out: 4096,
            }],
            max_links: 7,
            attaching: 0,
        },
        frames_in: HashMap::from([("AA:BB:CC:DD:EE:FF".to_string(), (30, 9000))]),
        link_peers: HashMap::new(),
        retired_links: 0,
        peers: vec![PeerRow {
            id: "8899aabbccddeeff".into(),
            nick: "alice".into(),
            direct: true,
            connected: true,
            verified: true,
            session: "secured",
            seen: Duration::from_secs(3),
            caps: vec!["media".into()],
        }],
        mesh,
        sent,
        relays: Vec::new(),
        topology: Topology::default(),
        secured: 1,
        courier: 0,
        prekeys: Some(8),
        bundles: 0,
        outbox: 2,
        tor: false,
        relaying: true,
        verbose: false,
        contribution: Default::default(),
    }
}

#[test]
fn contribution_is_counts_without_identities() {
    let mut snap = snapshot();
    snap.mesh.relayed_by_type.insert("message", 3);
    snap.mesh.relayed_for = 2;
    snap.contribution = super::contribution::Contribution {
        relaying_for: Some(Duration::from_secs(600)),
        awake: true,
        relays_cancelled: 1,
        dm_acked: 4,
        dm_failed: 1,
    };
    let lines: HashMap<&str, String> = snap.contribution_lines().into_iter().collect();
    assert_eq!(lines["relaying"], "on for 10m00s · keeping this PC awake");
    assert!(lines["relayed"].starts_with("2 packets for 2 senders · 1 skipped"), "{}", lines["relayed"]);
    assert_eq!(lines["bridging"], "no neighbors right now");
    assert!(lines["yours"].starts_with("4 private messages acknowledged · 1 given up"));
    let report = report::live(&snap);
    assert!(report.contains("[for the mesh]"));
    assert!(!report.contains("alice"));
}

#[test]
fn health_line_is_one_greppable_line() {
    let snap = snapshot();
    let text = snap.health_line((4, 1));
    assert!(!text.contains('\n'));
    for part in ["links out=1 in=0 max=7", "peers=1 direct=1 secured=1", "rx=14(+4)", "tx=5(+1)", "drops duplicate=7"] {
        assert!(text.contains(part), "{part} missing from {text}");
    }
}

#[test]
fn health_delta_counts_since_last_call() {
    let mut state = DiagState::default();
    assert_eq!(state.health_delta(10, 4), (10, 4));
    assert_eq!(state.health_delta(15, 4), (5, 0));
}

#[test]
fn live_report_holds_no_names_or_full_ids() {
    let text = report::live(&snapshot());
    assert!(!text.contains("alice"), "nicknames stay out of reports");
    assert!(!text.contains("8899aabbccddeeff"));
    assert!(!text.contains("AA:BB:CC:DD"));
    assert!(text.contains("8899…"));
    assert!(text.contains("…EE:FF"));
    assert!(text.contains("write 512"));
}

#[test]
fn redacts_ids_keys_addresses_and_npubs() {
    assert_eq!(short_id("0a1b2c3d4e5f6071"), "0a1b…");
    assert_eq!(mask_device("in:AA:BB:CC:DD:EE:FF"), "in:…EE:FF");
    assert_eq!(mask_device("BluetoothLE#BluetoothLE00:11-aa:bb:cc:dd:ee:ff"), "…ee:ff");
    assert_eq!(mask_device("123456789abc"), "…9abc");
    let raw = "WARN witchat_core: noise decrypt failed peer=0a1b2c3d4e5f6071 from AA:BB:CC:DD:EE:FF key npub1qqqqxyz, rssi -60";
    assert_eq!(line(raw), "WARN witchat_core: noise decrypt failed peer=0a1b… from …EE:FF key npub1…, rssi -60");
    assert_eq!(line("timestamp 2026-10-18T01:02:03Z ok"), "timestamp 2026-10-18T01:02:03Z ok");
    assert_eq!(line("word deadbeefcafe0011 inside"), "word dead… inside");
    assert_eq!(line("ünïcode ✓ fine"), "ünïcode ✓ fine");
}

#[test]
fn durations_and_sizes_read_naturally() {
    assert_eq!(human_duration(Duration::from_secs(42)), "42s");
    assert_eq!(human_duration(Duration::from_secs(303)), "5m03s");
    assert_eq!(human_duration(Duration::from_secs(3725)), "1h02m");
    assert_eq!(human_bytes(512), "512 B");
    assert_eq!(human_bytes(4096), "4.0 KB");
}
