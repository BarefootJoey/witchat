//! Relays held back by a short random delay (iOS `RelayController`), so
//! neighbors who heard the same packet do not all repeat it at once, and a
//! copy relayed by someone else first can make ours unnecessary.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use tokio::task::AbortHandle;

/// iOS keeps at most this many relays waiting.
const MAX_PENDING: usize = 512;

#[derive(Default)]
pub struct RelaySchedule {
    pending: HashMap<String, AbortHandle>,
    /// Waiting relays dropped because a neighbor's copy reached us first.
    pub cancelled: u64,
}

impl RelaySchedule {
    /// Run `relay` after `delay` unless [`cancel`](Self::cancel) is called for `key` first.
    pub fn schedule<F>(&mut self, key: String, delay: Duration, relay: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.pending.retain(|_, task| !task.is_finished());
        if self.pending.len() >= MAX_PENDING {
            self.pending.clear();
        }
        let task = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            relay.await;
        });
        if let Some(older) = self.pending.insert(key, task.abort_handle()) {
            older.abort();
        }
    }

    /// Drop the waiting relay for `key`; true if one was still waiting.
    pub fn cancel(&mut self, key: &str) -> bool {
        let Some(task) = self.pending.remove(key) else { return false };
        let waiting = !task.is_finished();
        task.abort();
        if waiting {
            self.cancelled += 1;
        }
        waiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn counter(sent: &Arc<AtomicUsize>) -> impl Future<Output = ()> + Send + 'static {
        let sent = sent.clone();
        async move {
            sent.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn a_duplicate_before_the_delay_cancels_the_relay() {
        let sent = Arc::new(AtomicUsize::new(0));
        let mut schedule = RelaySchedule::default();
        schedule.schedule("a".into(), Duration::from_millis(20), counter(&sent));
        schedule.schedule("b".into(), Duration::from_millis(20), counter(&sent));
        assert!(schedule.cancel("a"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(sent.load(Ordering::SeqCst), 1);
        assert_eq!(schedule.cancelled, 1);
        assert!(!schedule.cancel("b"), "already sent");
        assert_eq!(schedule.cancelled, 1);
    }
}
