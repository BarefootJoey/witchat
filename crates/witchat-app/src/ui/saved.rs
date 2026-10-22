//! `/history on|off`: opt-in saved channel history (format in `crate::history`).

use super::App;
use crate::history;
use crate::inbox::Conversation;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Writes are batched while chat flows; at most this much is lost on exit.
const SAVE_EVERY: Duration = Duration::from_secs(3);

pub(super) struct SavedHistory {
    path: Option<PathBuf>,
    /// Off when the user has not opted in, or the file could not be read
    /// (then we never overwrite it).
    enabled: bool,
    saved_revision: Option<u64>,
    last_write: Option<Instant>,
}

impl Default for SavedHistory {
    fn default() -> Self {
        Self { path: history::default_path(), enabled: false, saved_revision: None, last_write: None }
    }
}

impl App {
    /// On start: show saved lines when the user opted in.
    pub(super) fn restore_history(&mut self) {
        let Some(path) = self.history.path.clone().filter(|_| self.social.keep_history) else {
            return;
        };
        match history::load(&path) {
            Ok(saved) => {
                self.inbox.restore(history::restore(saved));
                self.history.enabled = true;
                self.history.saved_revision = Some(self.inbox.revision());
            }
            Err(err) => self.set_error(format!("saved history not loaded (left untouched): {err:#}")),
        }
    }

    /// Called every tick; writes when lines changed and the batch window passed.
    pub(super) fn persist_history(&mut self) {
        let state = &self.history;
        let due = state.last_write.is_none_or(|t| t.elapsed() >= SAVE_EVERY);
        if !state.enabled || !due || state.saved_revision == Some(self.inbox.revision()) {
            return;
        }
        let Some(path) = state.path.clone() else {
            return;
        };
        let revision = self.inbox.revision();
        self.history.last_write = Some(Instant::now());
        match history::save(&path, &history::snapshot(self.inbox.entries())) {
            Ok(()) => self.history.saved_revision = Some(revision),
            Err(err) => {
                tracing::warn!(%err, "saving history failed");
                self.history.enabled = false;
                self.set_error(format!("could not save history: {err:#}"));
            }
        }
    }

    /// `/history`, `/history on`, `/history off`.
    pub(super) fn set_history(&mut self, arg: &str) {
        let conv = self.selected.clone();
        let Some(path) = self.history.path.clone() else {
            self.set_error("no data directory; history cannot be saved".into());
            return;
        };
        match arg.to_ascii_lowercase().as_str() {
            "on" => {
                self.social.keep_history = true;
                self.save_social();
                self.history.enabled = true;
                self.history.saved_revision = None;
                let note = format!(
                    "history on: #mesh and location channels are saved as plaintext in {} (DMs never are). /history off deletes it.",
                    path.display()
                );
                self.inbox.push_system(&conv, note);
            }
            "off" => {
                self.social.keep_history = false;
                self.save_social();
                self.history.enabled = false;
                match history::delete(&path) {
                    Ok(()) => self.inbox.push_system(&conv, "history off: saved lines deleted".into()),
                    Err(err) => self.set_error(format!("{err:#}")),
                }
            }
            "" => {
                let state = if self.social.keep_history { "on" } else { "off (messages vanish on exit)" };
                self.inbox.push_system(&conv, format!("history is {state}; /history on|off"));
            }
            _ => self.set_error("usage: /history on|off".into()),
        }
        if matches!(conv, Conversation::Dm(_)) && arg.eq_ignore_ascii_case("on") {
            self.inbox.push_system(&conv, "this private chat is not saved".into());
        }
    }

    /// Panic wipe: delete the file and stop saving.
    pub(super) fn wipe_history(&mut self) -> Option<String> {
        self.history.enabled = false;
        let path = self.history.path.clone()?;
        history::delete(&path).err().map(|err| format!("wipe: {err:#}"))
    }
}
