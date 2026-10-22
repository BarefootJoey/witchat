//! `/ping` and `/trace`: mesh-only, as on iOS. Replies land in the
//! conversation the command was typed in, even after switching chats.

use super::App;
use crate::inbox::Conversation;
use crate::mesh_worker::Command;

impl App {
    pub(super) fn mesh_diagnostic(&mut self, cmd: &str, arg: &str) {
        let verb = &cmd[1..];
        if matches!(self.selected, Conversation::Geo(_) | Conversation::GeoDm { .. }) {
            self.set_error(format!("{verb} only works for mesh peers in #mesh"));
            return;
        }
        let target = arg.trim().trim_start_matches('@');
        if target.is_empty() {
            self.set_error(format!("usage: {cmd} <nickname>"));
            return;
        }
        let Some((id, nick)) = self.roster.find(target).map(|p| (p.id.clone(), p.nick.clone())) else {
            self.set_error(format!("cannot {verb} {target}: not found on mesh"));
            return;
        };
        self.diag_replies.insert(id.clone(), self.selected.clone());
        if cmd == "/ping" {
            self.inbox.push_system(&self.selected.clone(), format!("pinging {nick}…"));
            self.send(Command::Ping(id));
        } else {
            self.send(Command::Trace(id));
        }
    }

    pub(super) fn on_diagnostic(&mut self, id: &str, text: String) {
        let conv = self.diag_replies.get(id).cloned().unwrap_or_else(|| self.selected.clone());
        self.inbox.push_system(&conv, text);
    }
}
