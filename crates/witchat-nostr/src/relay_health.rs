//! How each relay has behaved this run. A relay that keeps failing is
//! benched with exponential backoff and skipped when picking a cell's
//! relays; one that refuses us by policy (NIP-01 `blocked:`, `restricted:`,
//! `auth-required:`, `pow:`) is benched for the rest of the run. Only state
//! changes are logged at warn, so one dead relay is one line, not hundreds.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const FIRST_BENCH: Duration = Duration::from_secs(60);
const MAX_BENCH: Duration = Duration::from_secs(60 * 60);
/// Failures in a row before a relay is benched.
const BENCH_AFTER: u32 = 3;
/// Several connections share a relay (DM inbox and each cell's DM inbox);
/// failures closer together than this are one outage, not a streak.
const SAME_OUTAGE: Duration = Duration::from_secs(5);
const POLICY_PREFIXES: [&str; 4] = ["blocked:", "restricted:", "auth-required:", "pow:"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayState {
    Healthy,
    Failing,
    /// Skipped until the backoff ends.
    Benched,
    /// Refused us by policy; skipped for the rest of the run.
    Refused,
}

impl RelayState {
    pub fn label(&self) -> &'static str {
        match self {
            RelayState::Healthy => "ok",
            RelayState::Failing => "failing",
            RelayState::Benched => "benched",
            RelayState::Refused => "refused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayHealth {
    pub url: String,
    pub state: RelayState,
    pub ok: u64,
    pub failures: u64,
    /// Failures since the last success.
    pub streak: u32,
    pub last_error: Option<String>,
    pub last_ok: Option<Instant>,
    pub benched_until: Option<Instant>,
    last_failure: Option<Instant>,
}

impl RelayHealth {
    fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            state: RelayState::Healthy,
            ok: 0,
            failures: 0,
            streak: 0,
            last_error: None,
            last_ok: None,
            benched_until: None,
            last_failure: None,
        }
    }
}

#[derive(Default)]
pub struct Registry {
    relays: HashMap<String, RelayHealth>,
}

impl Registry {
    fn entry(&mut self, url: &str) -> &mut RelayHealth {
        self.relays.entry(normalize(url)).or_insert_with(|| RelayHealth::new(&normalize(url)))
    }

    pub fn ok(&mut self, url: &str, now: Instant) {
        let entry = self.entry(url);
        if entry.state == RelayState::Refused {
            return;
        }
        if entry.state != RelayState::Healthy {
            tracing::info!(relay = %entry.url, after = entry.streak, "relay: working again");
        }
        entry.ok += 1;
        entry.streak = 0;
        entry.state = RelayState::Healthy;
        entry.benched_until = None;
        entry.last_ok = Some(now);
    }

    pub fn failed(&mut self, url: &str, why: &str, now: Instant) {
        let entry = self.entry(url);
        entry.failures += 1;
        entry.last_error = Some(why.chars().take(160).collect());
        if entry.state == RelayState::Refused {
            return;
        }
        if is_policy_refusal(why) {
            entry.state = RelayState::Refused;
            entry.benched_until = None;
            tracing::warn!(relay = %entry.url, %why, "relay: refuses us by policy; skipped for this run");
            return;
        }
        if entry.benched_until.is_some_and(|until| now < until) {
            return;
        }
        let same_outage = entry.last_failure.is_some_and(|t| now.saturating_duration_since(t) < SAME_OUTAGE);
        entry.last_failure = Some(now);
        if same_outage && entry.streak > 0 {
            return;
        }
        entry.streak += 1;
        if entry.streak < BENCH_AFTER {
            if entry.state == RelayState::Healthy {
                tracing::warn!(relay = %entry.url, %why, "relay: failing");
            } else {
                tracing::debug!(relay = %entry.url, %why, streak = entry.streak, "relay: still failing");
            }
            entry.state = RelayState::Failing;
            return;
        }
        let doublings = (entry.streak - BENCH_AFTER).min(6);
        let wait = FIRST_BENCH.saturating_mul(1 << doublings).min(MAX_BENCH);
        entry.state = RelayState::Benched;
        entry.benched_until = Some(now + wait);
        tracing::warn!(relay = %entry.url, %why, streak = entry.streak, bench_s = wait.as_secs(), "relay: benched");
    }

    pub fn usable(&mut self, url: &str, now: Instant) -> bool {
        let Some(entry) = self.relays.get_mut(&normalize(url)) else { return true };
        match entry.state {
            RelayState::Refused => false,
            RelayState::Benched if entry.benched_until.is_some_and(|until| now < until) => false,
            RelayState::Benched => {
                entry.state = RelayState::Failing;
                true
            }
            _ => true,
        }
    }

    pub fn snapshot(&self) -> Vec<RelayHealth> {
        let mut all: Vec<RelayHealth> = self.relays.values().cloned().collect();
        all.sort_by(|a, b| a.url.cmp(&b.url));
        all
    }

    pub fn clear(&mut self) {
        self.relays.clear();
    }
}

fn is_policy_refusal(why: &str) -> bool {
    let why = why.trim_start().to_ascii_lowercase();
    POLICY_PREFIXES.iter().any(|p| why.starts_with(p))
}

fn normalize(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn global() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

fn with<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut guard = global().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

pub fn record_ok(url: &str) {
    with(|r| r.ok(url, Instant::now()));
}

pub fn record_failure(url: &str, why: &str) {
    with(|r| r.failed(url, why, Instant::now()));
}

pub fn is_usable(url: &str) -> bool {
    with(|r| r.usable(url, Instant::now()))
}

/// Refused us by policy this run: posting there is pointless, but reading
/// may still work.
pub fn refuses_us(url: &str) -> bool {
    with(|r| r.relays.get(&normalize(url)).is_some_and(|e| e.state == RelayState::Refused))
}

pub fn snapshot() -> Vec<RelayHealth> {
    with(|r| r.snapshot())
}

/// Forget everything, e.g. after switching to or from Tor.
pub fn reset() {
    with(Registry::clear);
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "wss://relay.example/";

    #[test]
    fn benches_after_a_streak_and_backs_off() {
        let mut r = Registry::default();
        let t0 = Instant::now();
        r.failed(URL, "timeout", t0);
        r.failed(URL, "timeout", t0 + Duration::from_secs(10));
        let t = t0 + Duration::from_secs(20);
        assert!(r.usable(URL, t), "two failures only mark it failing");
        r.failed(URL, "relay not connected", t);
        assert!(!r.usable(URL, t + Duration::from_secs(59)));
        assert!(r.usable(URL, t + Duration::from_secs(60)), "bench over: try again");
        r.failed(URL, "timeout", t + Duration::from_secs(60));
        assert_eq!(r.snapshot()[0].benched_until, Some(t + Duration::from_secs(180)), "doubled");
        r.ok(URL, t + Duration::from_secs(200));
        let s = &r.snapshot()[0];
        assert_eq!((s.state.clone(), s.streak, s.ok, s.failures), (RelayState::Healthy, 0, 1, 4));
    }

    #[test]
    fn connections_sharing_a_relay_count_one_outage_once() {
        let mut r = Registry::default();
        let t = Instant::now();
        for _ in 0..3 {
            r.failed(URL, "not connected (Connecting)", t);
        }
        let s = &r.snapshot()[0];
        assert_eq!((s.state.clone(), s.streak, s.failures), (RelayState::Failing, 1, 3));
        r.failed(URL, "timeout", t + SAME_OUTAGE);
        assert_eq!(r.snapshot()[0].streak, 2, "a later failure still counts");
        assert_eq!(s.url, "wss://relay.example", "one entry however the url is spelled");
    }

    #[test]
    fn policy_refusals_stick_for_the_run() {
        let mut r = Registry::default();
        let t = Instant::now();
        r.failed(URL, "blocked: this relay is community funded", t);
        assert!(!r.usable(URL, t + MAX_BENCH * 2));
        r.ok(URL, t);
        assert_eq!(r.snapshot()[0].state, RelayState::Refused);
        assert!(is_policy_refusal(" Auth-Required: sign in"));
        assert!(!is_policy_refusal("rate-limited: slow down"));
        assert!(r.usable("wss://never.seen", t));
    }
}
