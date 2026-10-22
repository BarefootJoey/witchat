//! When to try attaching a peripheral again. A phone that walked away is
//! retried quickly; a device that advertises bitchat but never completes a
//! GATT link (wrong firmware, a background app without its server) is retried
//! exponentially less often, so it cannot monopolize the scan loop.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Wait after a link drops, or after a first failed attach. Short, so a
/// phone walking back into range is picked up quickly.
pub(crate) const ATTACH_COOLDOWN: Duration = Duration::from_secs(6);
/// Ceiling for devices that keep failing.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// A device with no bitchat service starts as if it had failed this often
/// (a 24 s wait).
const NO_SERVICE_FAILURES: u32 = 3;
/// A phone that refused access this many times in a row is retried at the
/// cooldown before the doubling starts: refusals are usually momentary
/// (the phone is mid-connection with someone else).
const BUSY_RETRIES: u32 = 4;
/// Wait after a link went silent: long enough not to churn a backgrounded
/// phone, short enough to recover a stuck one.
pub(crate) const SILENT_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct AttachBackoff {
    /// Not before this instant, and how many attaches failed in a row.
    entries: HashMap<String, (Instant, u32)>,
}

impl AttachBackoff {
    pub(crate) fn ready(&self, id: &str, now: Instant) -> bool {
        self.entries.get(id).is_none_or(|(at, _)| now >= *at)
    }

    /// Time left before the next try, when still waiting.
    pub(crate) fn wait(&self, id: &str, now: Instant) -> Option<Duration> {
        self.entries.get(id).and_then(|(at, _)| at.checked_duration_since(now)).filter(|d| !d.is_zero())
    }

    /// An attach failed; returns how long until the next try.
    pub(crate) fn failed(&mut self, id: &str, now: Instant) -> Duration {
        self.failed_at_least(id, now, 1)
    }

    /// The device answered that it has no bitchat GATT service at all, so
    /// it is not a phone that was merely busy: skip the quick retries.
    pub(crate) fn failed_without_service(&mut self, id: &str, now: Instant) -> Duration {
        self.failed_at_least(id, now, NO_SERVICE_FAILURES)
    }

    /// The phone was there but refused for now; see [`BUSY_RETRIES`].
    pub(crate) fn busy(&mut self, id: &str, now: Instant) -> Duration {
        let failures = self.entries.get(id).map_or(0, |(_, n)| *n) + 1;
        let wait = if failures <= BUSY_RETRIES { ATTACH_COOLDOWN } else { delay(failures - BUSY_RETRIES) };
        self.entries.insert(id.to_string(), (now + wait, failures));
        wait
    }

    fn failed_at_least(&mut self, id: &str, now: Instant, floor: u32) -> Duration {
        let failures = (self.entries.get(id).map_or(0, |(_, n)| *n) + 1).max(floor);
        let wait = delay(failures);
        self.entries.insert(id.to_string(), (now + wait, failures));
        wait
    }

    /// A working link closed (peer walked away): retry soon, forget failures.
    pub(crate) fn dropped(&mut self, id: &str, now: Instant) {
        self.entries.insert(id.to_string(), (now + ATTACH_COOLDOWN, 0));
    }

    /// Leave a device alone for `hold`, e.g. a duplicate link we retired.
    pub(crate) fn hold(&mut self, id: &str, now: Instant, hold: Duration) {
        self.entries.insert(id.to_string(), (now + hold, 0));
    }

    pub(crate) fn succeeded(&mut self, id: &str) {
        self.entries.remove(id);
    }
}

fn delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    ATTACH_COOLDOWN.saturating_mul(1 << doublings).min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_failures_back_off_to_a_ceiling() {
        let mut b = AttachBackoff::default();
        let t = Instant::now();
        assert!(b.ready("x", t));
        assert_eq!(b.failed("x", t), Duration::from_secs(6));
        assert!(!b.ready("x", t + Duration::from_secs(5)));
        assert_eq!(b.wait("x", t + Duration::from_secs(5)), Some(Duration::from_secs(1)));
        assert!(b.ready("x", t + Duration::from_secs(6)));
        assert_eq!(b.wait("x", t + Duration::from_secs(6)), None);
        assert_eq!(b.wait("y", t), None);
        assert_eq!(b.failed("x", t), Duration::from_secs(12));
        assert_eq!(b.failed("x", t), Duration::from_secs(24));
        for _ in 0..20 {
            b.failed("x", t);
        }
        assert_eq!(b.failed("x", t), MAX_BACKOFF);
    }

    #[test]
    fn a_device_without_the_service_skips_quick_retries() {
        let mut b = AttachBackoff::default();
        let t = Instant::now();
        assert_eq!(b.failed_without_service("x", t), Duration::from_secs(24));
        assert_eq!(b.failed_without_service("x", t), Duration::from_secs(48));
        assert_eq!(b.failed("x", t), Duration::from_secs(96));
    }

    #[test]
    fn a_busy_phone_gets_flat_retries_before_doubling() {
        let mut b = AttachBackoff::default();
        let t = Instant::now();
        for _ in 0..BUSY_RETRIES {
            assert_eq!(b.busy("x", t), ATTACH_COOLDOWN);
        }
        assert_eq!(b.busy("x", t), Duration::from_secs(6));
        assert_eq!(b.busy("x", t), Duration::from_secs(12));
        b.succeeded("x");
        assert_eq!(b.busy("x", t), ATTACH_COOLDOWN);
    }

    #[test]
    fn a_working_link_resets_the_penalty() {
        let mut b = AttachBackoff::default();
        let t = Instant::now();
        b.failed("x", t);
        b.failed("x", t);
        b.succeeded("x");
        assert!(b.ready("x", t));
        b.dropped("x", t);
        assert!(!b.ready("x", t));
        assert_eq!(b.failed("x", t), Duration::from_secs(6), "drop does not count as a failure");
    }
}
