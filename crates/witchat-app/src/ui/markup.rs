//! Message body styling, as iOS `ChatMessageFormatter`: `@mentions` (orange
//! when they name you), geohash `#tags` that join that location channel,
//! and http(s) links. A message that mentions you is bold.

use super::view::BOLD;
use super::Message;
use crate::theme;
use iced::widget::span;
use iced::widget::text::Span;
use iced::{Color, Font};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Token<'a> {
    Text(&'a str),
    /// `@name` plus an optional `#abcd` disambiguating suffix.
    Mention {
        name: &'a str,
        suffix: &'a str,
    },
    /// A standalone `#geohash`, without the `#`.
    Geohash(&'a str),
    Url(&'a str),
}

const GEOHASH_ALPHABET: &str = "0123456789bcdefghjkmnpqrstuvwxyz";

/// iOS `Patterns.mention`: `@([\p{L}0-9_]+(?:#[a-fA-F0-9]{4})?)`. Unlike iOS
/// we skip `@` inside a word, so e-mail addresses stay plain text.
pub(super) fn tokenize(s: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut plain = 0;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let prev = s[..i].chars().next_back();
        let at_word_start = prev.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let found = if (rest.starts_with("https://") || rest.starts_with("http://")) && at_word_start {
            url_len(rest).map(|n| (n, Token::Url(&rest[..n])))
        } else if rest.starts_with('@') && at_word_start {
            mention_len(rest)
                .map(|(n, name_end)| (n, Token::Mention { name: &rest[1..name_end], suffix: &rest[name_end..n] }))
        } else if rest.starts_with('#') && prev.is_none_or(char::is_whitespace) {
            geohash_len(rest).map(|n| (n, Token::Geohash(&rest[1..n])))
        } else {
            None
        };
        match found {
            Some((len, token)) => {
                if plain < i {
                    out.push(Token::Text(&s[plain..i]));
                }
                out.push(token);
                i += len;
                plain = i;
            }
            None => i += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    if plain < s.len() {
        out.push(Token::Text(&s[plain..]));
    }
    out
}

fn url_len(rest: &str) -> Option<usize> {
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let trimmed = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '\'', '"']);
    (trimmed.len() > "https://".len()).then_some(trimmed.len())
}

/// `(token length, end of the name part)`.
fn mention_len(rest: &str) -> Option<(usize, usize)> {
    let name_len: usize = rest[1..]
        .chars()
        .take_while(|c| c.is_alphabetic() || c.is_ascii_digit() || *c == '_')
        .map(char::len_utf8)
        .sum();
    if name_len == 0 {
        return None;
    }
    let name_end = 1 + name_len;
    let suffix = &rest[name_end..];
    let has_suffix = suffix.len() >= 5
        && suffix.starts_with('#')
        && suffix[1..5].chars().all(|c| c.is_ascii_hexdigit())
        && suffix[5..].chars().next().is_none_or(|c| !c.is_alphanumeric());
    Some((if has_suffix { name_end + 5 } else { name_end }, name_end))
}

fn geohash_len(rest: &str) -> Option<usize> {
    let body: usize = rest[1..].chars().take_while(|c| c.is_ascii_alphanumeric()).count();
    let tag = &rest[1..1 + body];
    let valid = (2..=12).contains(&body) && tag.chars().all(|c| GEOHASH_ALPHABET.contains(c.to_ascii_lowercase()));
    valid.then_some(1 + body)
}

/// Spans for a message body in `color`; `me` is our nickname.
pub(super) fn body_spans(content: &str, color: Color, me: &str, mentioned: bool) -> Vec<Span<'static, Message>> {
    let font = if mentioned { BOLD } else { Font::MONOSPACE };
    tokenize(content)
        .into_iter()
        .map(|token| match token {
            Token::Text(t) => span(t.to_string()).color(color).font(font),
            Token::Mention { name, suffix } => {
                let to_me = match me.split_once('#') {
                    Some(_) => format!("{name}{suffix}").eq_ignore_ascii_case(me),
                    None => !me.is_empty() && name.eq_ignore_ascii_case(me),
                };
                let tint = if to_me { theme::ORANGE } else { color };
                span(format!("@{name}{suffix}")).color(tint).font(BOLD)
            }
            Token::Geohash(cell) => span(format!("#{cell}"))
                .color(theme::GEO)
                .font(font)
                .underline(true)
                .link(Message::JoinGeohash(cell.to_ascii_lowercase())),
            Token::Url(url) => {
                span(url.to_string()).color(color).font(font).underline(true).link(Message::OpenUrl(url.to_string()))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use Token::*;

    #[test]
    fn finds_mentions_tags_and_links() {
        assert_eq!(
            tokenize("hi @bob#a1b2, meet at #9q8yy https://x.org/a."),
            [
                Text("hi "),
                Mention { name: "bob", suffix: "#a1b2" },
                Text(", meet at "),
                Geohash("9q8yy"),
                Text(" "),
                Url("https://x.org/a"),
                Text("."),
            ]
        );
    }

    #[test]
    fn leaves_lookalikes_as_text() {
        assert_eq!(tokenize("mail a@b.c"), [Text("mail a@b.c")]);
        assert_eq!(tokenize("#hello and x#9q8"), [Text("#hello and x#9q8")], "l/o are not geohash letters");
        assert_eq!(tokenize("@ alone"), [Text("@ alone")]);
        assert_eq!(tokenize("@émile"), [Mention { name: "émile", suffix: "" }]);
    }
}
