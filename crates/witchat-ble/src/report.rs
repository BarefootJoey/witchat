//! What the radio sees, sent every few seconds as [`crate::BleEvent::Report`]
//! for the debug tab and the periodic health log line.

use std::time::Duration;

/// A device advertising the bitchat service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEntry {
    pub id: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    /// We hold a link to it, outbound or inbound.
    pub linked: bool,
    /// Attach backoff still running after failures.
    pub retry_in: Option<Duration>,
}

/// A live GATT link. Inbound ids start with `in:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEntry {
    pub id: String,
    pub inbound: bool,
    /// Largest single write the link takes; None until negotiated.
    pub max_write: Option<usize>,
    pub rssi: Option<i16>,
    pub up_for: Duration,
    /// Since the phone last sent us anything; outbound links only.
    pub heard_ago: Option<Duration>,
    pub frames_out: u64,
    pub bytes_out: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RadioReport {
    pub scanning: bool,
    /// On battery: scanning in windows rather than continuously.
    pub low_power: bool,
    /// Every advertiser in range, bitchat or not.
    pub ads_seen: usize,
    pub bitchat: Vec<ScanEntry>,
    pub links: Vec<LinkEntry>,
    pub max_links: usize,
    pub attaching: usize,
}

impl RadioReport {
    /// Fill link RSSI from the matching advertisement (outbound links only:
    /// a phone connected in is not necessarily one we can hear).
    pub(crate) fn join_rssi(&mut self) {
        for link in &mut self.links {
            link.rssi = self.bitchat.iter().find(|s| s.id == link.id).and_then(|s| s.rssi);
        }
    }

    pub fn inbound(&self) -> usize {
        self.links.iter().filter(|l| l.inbound).count()
    }

    pub fn outbound(&self) -> usize {
        self.links.len() - self.inbound()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(id: &str) -> LinkEntry {
        LinkEntry {
            id: id.into(),
            inbound: id.starts_with("in:"),
            max_write: None,
            rssi: None,
            up_for: Duration::ZERO,
            heard_ago: None,
            frames_out: 0,
            bytes_out: 0,
        }
    }

    #[test]
    fn joins_rssi_and_counts_roles() {
        let mut report = RadioReport {
            bitchat: vec![ScanEntry { id: "AA".into(), name: None, rssi: Some(-60), linked: true, retry_in: None }],
            links: vec![link("AA"), link("in:AA"), link("BB")],
            ..Default::default()
        };
        report.join_rssi();
        assert_eq!(report.links[0].rssi, Some(-60));
        assert_eq!(report.links[1].rssi, None, "inbound keeps no RSSI");
        assert_eq!(report.links[2].rssi, None);
        assert_eq!((report.inbound(), report.outbound()), (1, 2));
    }
}
