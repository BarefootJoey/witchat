//! The slash-command catalog: suggestions shown above the composer while the
//! user types a command (as Telegram does), Tab completion, and `/help`.

use crate::inbox::Conversation;

/// Where a command makes sense; suggestions hide the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    Anywhere,
    MeshOrDm,
    Dm,
    Geo,
}

impl Scope {
    fn allows(self, conv: &Conversation) -> bool {
        match self {
            Self::Anywhere => true,
            Self::MeshOrDm => matches!(conv, Conversation::Mesh | Conversation::Dm(_)),
            Self::Dm => matches!(conv, Conversation::Dm(_)),
            Self::Geo => matches!(conv, Conversation::Geo(_)),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SlashCommand {
    pub name: &'static str,
    pub args: &'static str,
    pub about: &'static str,
    pub scope: Scope,
}

pub(super) const COMMANDS: &[SlashCommand] = &[
    cmd("/msg", "<nick> <text>", "private message someone nearby", Scope::Anywhere),
    cmd("/send", "<path>", "send a photo, voice note or PDF (max 1 MiB)", Scope::MeshOrDm),
    cmd("/sendvisible", "<path>", "send unencrypted to a peer without private media", Scope::Dm),
    cmd("/fav", "<nick>", "favorite: reach them over Nostr when out of range", Scope::Anywhere),
    cmd("/unfav", "<nick>", "remove a favorite", Scope::Anywhere),
    cmd("/pay", "<token>", "send a cashu token in this chat (carrier only)", Scope::Anywhere),
    cmd("/hug", "<nick>", "send someone a hug", Scope::Anywhere),
    cmd("/slap", "<nick>", "slap someone around with a large trout", Scope::Anywhere),
    cmd("/nick", "<name>", "change your nickname", Scope::Anywhere),
    cmd("/geo", "<geohash>", "join a location channel", Scope::Anywhere),
    cmd("/leave", "", "leave this location channel", Scope::Geo),
    cmd("/block", "[nick]", "hide a peer and refuse their files", Scope::Anywhere),
    cmd("/unblock", "[nick]", "undo /block", Scope::Anywhere),
    cmd("/clear", "", "clear this conversation", Scope::Anywhere),
    cmd("/history", "[on|off]", "keep #mesh and location lines across restarts", Scope::Anywhere),
    cmd("/who", "", "announce yourself and refresh who is nearby", Scope::Anywhere),
    cmd("/ping", "<nick>", "measure the round trip to a mesh peer", Scope::MeshOrDm),
    cmd("/trace", "<nick>", "estimated mesh path to a peer", Scope::MeshOrDm),
    cmd("/help", "", "list commands", Scope::Anywhere),
];

const fn cmd(name: &'static str, args: &'static str, about: &'static str, scope: Scope) -> SlashCommand {
    SlashCommand { name, args, about, scope }
}

/// Commands matching what is typed so far, while the command word itself is
/// still being typed (no space yet).
pub(super) fn suggestions(draft: &str, conv: &Conversation) -> Vec<&'static SlashCommand> {
    let draft = draft.trim_start();
    if !draft.starts_with('/') || draft.contains(char::is_whitespace) {
        return Vec::new();
    }
    let typed = draft.to_ascii_lowercase();
    let matches: Vec<_> = COMMANDS.iter().filter(|c| c.scope.allows(conv) && c.name.starts_with(&typed)).collect();
    // Fully typed and nothing longer shares the prefix: nothing to suggest.
    if matches.len() == 1 && matches[0].name == typed {
        return Vec::new();
    }
    matches
}

/// The draft after accepting a suggestion: ready for its arguments.
pub(super) fn complete(command: &SlashCommand) -> String {
    if command.args.is_empty() {
        command.name.to_string()
    } else {
        format!("{} ", command.name)
    }
}

pub(super) fn help_text() -> String {
    let list: Vec<String> = COMMANDS
        .iter()
        .map(|c| if c.args.is_empty() { c.name.to_string() } else { format!("{} {}", c.name, c.args) })
        .collect();
    format!(
        "{} · type / for suggestions (Tab completes) · + or drag a file in to send · \
         click the witchat/ title for settings and info (triple-click: panic wipe)",
        list.join(" · ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(draft: &str, conv: &Conversation) -> Vec<&'static str> {
        suggestions(draft, conv).iter().map(|c| c.name).collect()
    }

    #[test]
    fn prefix_matches_respect_the_conversation() {
        let dm = Conversation::Dm("aa".into());
        let geo = Conversation::Geo("9q8yy".into());
        assert_eq!(names("/se", &dm), ["/send", "/sendvisible"]);
        assert_eq!(names("/se", &Conversation::Mesh), ["/send"], "visible fallback is DM-only");
        assert!(names("/se", &geo).is_empty(), "location channels are text only");
        assert_eq!(names("/le", &geo), ["/leave"]);
        assert!(names("/le", &dm).is_empty());
        assert_eq!(names("/", &dm).len(), COMMANDS.len() - 1, "all but /leave");
    }

    #[test]
    fn suggestions_stop_once_the_command_is_typed() {
        let dm = Conversation::Dm("aa".into());
        assert!(names("/send C:\\x.jpg", &dm).is_empty());
        assert!(names("/clear", &dm).is_empty());
        assert_eq!(names("/send", &dm), ["/send", "/sendvisible"], "longer match still offered");
        assert!(names("hello", &dm).is_empty());
        assert_eq!(names("/FA", &dm), ["/fav"]);
        assert_eq!(names("/pa", &dm), ["/pay"]);
    }

    #[test]
    fn completion_leaves_room_for_arguments() {
        let send = COMMANDS.iter().find(|c| c.name == "/send").unwrap();
        let clear = COMMANDS.iter().find(|c| c.name == "/clear").unwrap();
        assert_eq!(complete(send), "/send ");
        assert_eq!(complete(clear), "/clear");
        assert!(help_text().contains("/sendvisible <path>"));
        assert!(help_text().contains("/pay <token>"));
    }
}
