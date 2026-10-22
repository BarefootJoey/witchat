//! What the sidebar says while nobody is listed, including what to try when
//! no phone has turned up a while after the radio started.

use std::time::{Duration, Instant};

/// Long enough for a phone in range to be found and linked.
const PATIENCE: Duration = Duration::from_secs(120);

/// Since when the radio has been up with nobody online.
#[derive(Default)]
pub(super) struct Alone {
    since: Option<Instant>,
}

impl Alone {
    pub(super) fn update(&mut self, alone: bool, now: Instant) {
        self.since = if alone { Some(self.since.unwrap_or(now)) } else { None };
    }

    fn waited(&self, now: Instant) -> bool {
        self.since.is_some_and(|t| now.duration_since(t) >= PATIENCE)
    }
}

/// Sidebar lines under PEOPLE when nobody is online.
pub(super) fn people_hint(
    radio_ready: bool,
    radio_error: bool,
    alone: &Alone,
    now: Instant,
) -> &'static [&'static str] {
    if !radio_ready {
        return if radio_error { &["bluetooth off - enable it, witchat retries"] } else { &["starting radio…"] };
    }
    if !alone.waited(now) {
        return &["nobody around yet…"];
    }
    // iOS moves a backgrounded app's service UUID into an overflow area only
    // other Apple devices read, so Windows and Linux never see it.
    &[
        "no phones found yet. try:",
        "· bitchat open on a phone nearby",
        "  (a backgrounded iPhone can't be found)",
        "· bluetooth off and on on this PC",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tips_appear_only_after_waiting_alone() {
        let t = Instant::now();
        let mut alone = Alone::default();
        assert_eq!(people_hint(false, false, &alone, t), ["starting radio…"]);
        assert_eq!(people_hint(false, true, &alone, t)[0], "bluetooth off - enable it, witchat retries");
        alone.update(true, t);
        assert_eq!(people_hint(true, false, &alone, t + PATIENCE / 2), ["nobody around yet…"]);
        alone.update(true, t + PATIENCE / 2);
        assert_eq!(people_hint(true, false, &alone, t + PATIENCE)[0], "no phones found yet. try:");
        alone.update(false, t + PATIENCE);
        alone.update(true, t + PATIENCE);
        assert_eq!(people_hint(true, false, &alone, t + PATIENCE), ["nobody around yet…"], "someone came and went");
    }
}
