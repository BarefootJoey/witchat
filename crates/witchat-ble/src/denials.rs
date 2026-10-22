//! Spots the PC failing every phone at once. One phone refusing access or
//! not answering is routine (it is busy with someone else); several
//! different phones failing within minutes, with no link succeeding in
//! between, points at this PC: another program holding the phones, or a
//! wedged Bluetooth stack. The user can fix both, so they hear about it, at
//! most every half hour.

use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(5 * 60);
const DISTINCT: usize = 3;
const REPEAT: Duration = Duration::from_secs(30 * 60);

pub(crate) const HINT: &str = "Bluetooth on this PC is refusing every bitchat phone. Close any other copy of \
                               witchat or Bluetooth tool; if that is not it, turn Bluetooth off and on.";

/// Every nearby phone connects but never answers: Windows kept a dead
/// connection from an earlier run, and only a radio reset drops it.
pub(crate) const STALL_HINT: &str = "Phones are in range but none answer: Bluetooth on this PC kept a stuck \
                                     connection from an earlier run. Press \"reset Bluetooth\" to turn it off \
                                     and on and restart witchat.";

pub(crate) struct DenialWatch {
    distinct: usize,
    recent: Vec<(String, Instant)>,
    last_hint: Option<Instant>,
}

impl Default for DenialWatch {
    fn default() -> Self {
        Self::needing(DISTINCT)
    }
}

impl DenialWatch {
    /// Hint once this many different phones have failed.
    pub(crate) fn needing(distinct: usize) -> Self {
        Self { distinct, recent: Vec::new(), last_hint: None }
    }

    /// A phone refused access; true when the user should be told now.
    pub(crate) fn denied(&mut self, id: &str, now: Instant) -> bool {
        self.recent.retain(|(seen, at)| seen != id && now.duration_since(*at) < WINDOW);
        self.recent.push((id.to_string(), now));
        if self.recent.len() < self.distinct || self.last_hint.is_some_and(|t| now.duration_since(t) < REPEAT) {
            return false;
        }
        self.last_hint = Some(now);
        true
    }

    /// A link came up, so the stack works.
    pub(crate) fn linked(&mut self) {
        self.recent.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn several_phones_refused_together_raise_one_hint() {
        let mut w = DenialWatch::default();
        let t = Instant::now();
        assert!(!w.denied("a", t));
        assert!(!w.denied("a", t), "the same phone again is not a pattern");
        assert!(!w.denied("b", t));
        assert!(w.denied("c", t));
        assert!(!w.denied("d", t + Duration::from_secs(60)), "quiet for a while after a hint");
        let later = t + REPEAT;
        assert!(!w.denied("e", later));
        assert!(!w.denied("f", later));
        assert!(w.denied("g", later));
    }

    #[test]
    fn old_refusals_and_a_working_link_reset_the_count() {
        let mut w = DenialWatch::default();
        let t = Instant::now();
        w.denied("a", t);
        w.denied("b", t);
        assert!(!w.denied("c", t + WINDOW), "a and b are too old");
        w.linked();
        assert!(!w.denied("d", t + WINDOW));
        assert!(!w.denied("e", t + WINDOW));
        assert!(w.denied("f", t + WINDOW));
    }

    #[test]
    fn a_lower_threshold_hints_sooner() {
        let mut w = DenialWatch::needing(2);
        let t = Instant::now();
        assert!(!w.denied("a", t));
        assert!(!w.denied("a", t), "one phone failing twice is not a pattern");
        assert!(w.denied("b", t));
    }
}
