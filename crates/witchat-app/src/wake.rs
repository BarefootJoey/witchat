//! Wakes the GUI when something arrives for it (worker events, tray and
//! toast clicks), so it can idle on a slow timer instead of polling fast.
//! Every iced update redraws the window, so idle wakeups cost real CPU.

use iced::futures::stream::{self, Stream};
use tokio::sync::Notify;

static WAKE: Notify = Notify::const_new();

/// Ask the GUI to look at its channels soon. Wakes before the GUI listens
/// are kept (one permit), so none is lost.
pub fn wake() {
    WAKE.notify_one();
}

/// One item per [`wake`]; several wakes before the GUI runs count as one.
pub fn wakes() -> impl Stream<Item = ()> {
    stream::unfold((), |()| async {
        WAKE.notified().await;
        Some(((), ()))
    })
}
