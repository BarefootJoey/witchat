//! Composer behavior: where a typed line goes, and the slash-command
//! suggestion list (Tab completes, Up/Down pick).

use super::files::SlashKey;
use super::{composer_id, slash, App, Message};
use crate::inbox::Conversation;
use crate::mesh_worker::{self, Command};
use iced::widget::text_input;
use iced::Task;

impl App {
    pub(super) fn command_for(&self, line: &str) -> Option<Command> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        if line.starts_with('/') {
            return mesh_worker::parse_command_line(line);
        }
        match &self.selected {
            Conversation::Mesh => Some(Command::Public(line.into())),
            Conversation::Dm(id) => Some(Command::Private { to: id.clone(), content: line.into() }),
            Conversation::Geo(cell) => {
                Some(Command::Geo { cell: cell.clone(), text: line.into(), teleported: self.is_teleported(cell) })
            }
            Conversation::GeoDm { cell, pubkey } => {
                Some(Command::GeoDm { cell: cell.clone(), pubkey: pubkey.clone(), text: line.into() })
            }
        }
    }

    pub(super) fn slash_suggestions(&self) -> Vec<&'static slash::SlashCommand> {
        slash::suggestions(&self.draft, &self.selected)
    }

    /// Put suggestion `index` in the draft. Returns true when it takes arguments.
    pub(super) fn accept_suggestion(&mut self, index: usize) -> bool {
        let Some(command) = self.slash_suggestions().get(index).copied() else {
            return false;
        };
        self.draft = slash::complete(command);
        self.slash_pick = 0;
        !command.args.is_empty()
    }

    pub(super) fn on_slash_key(&mut self, key: SlashKey) -> Task<Message> {
        let count = self.slash_suggestions().len();
        if count == 0 {
            return Task::none();
        }
        match key {
            SlashKey::Up => self.slash_pick = (self.slash_pick + count - 1) % count,
            SlashKey::Down => self.slash_pick = (self.slash_pick + 1) % count,
            SlashKey::Complete => {
                self.accept_suggestion(self.slash_pick);
                return focus_composer();
            }
        }
        Task::none()
    }
}

/// Keep typing after a completion: focus the composer, cursor at the end.
pub(super) fn focus_composer() -> Task<Message> {
    Task::batch([text_input::focus(composer_id()), text_input::move_cursor_to_end(composer_id())])
}
