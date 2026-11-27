//! Selectable chat text. iced's `rich_text` cannot be selected, so runs of
//! plain chat lines are shown in a read-only `text_editor`: click and drag,
//! double-click a word, triple-click a line, Ctrl+A, Ctrl+C. A highlighter
//! colors each range exactly as the spans would have been colored.

use super::Message;
use crate::theme;
use iced::advanced::text::highlighter::{self, Highlighter};
use iced::widget::text::{LineHeight, Span, Wrapping};
use iced::widget::text_editor::{self, Action, Binding, Content};
use iced::{Color, Element, Font, Theme};
use std::ops::Range;
use std::sync::Arc;

/// The style of one range of a line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Mark {
    color: Color,
    font: Font,
}

/// Per-line ranges, indexed by the editor's line number.
pub(super) type LineMarks = Vec<Vec<(Range<usize>, Mark)>>;

#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct Marks(Arc<LineMarks>);

pub(super) struct LineHighlighter {
    marks: Arc<LineMarks>,
    current: usize,
}

impl Highlighter for LineHighlighter {
    type Settings = Marks;
    type Highlight = Mark;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Mark)>;

    fn new(settings: &Marks) -> Self {
        Self { marks: settings.0.clone(), current: 0 }
    }

    fn update(&mut self, settings: &Marks) {
        self.marks = settings.0.clone();
        self.current = 0;
    }

    fn change_line(&mut self, line: usize) {
        self.current = self.current.min(line);
    }

    fn highlight_line(&mut self, _line: &str) -> Self::Iterator<'_> {
        let marks = self.marks.get(self.current).cloned().unwrap_or_default();
        self.current += 1;
        marks.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current
    }
}

fn to_format(mark: &Mark, _theme: &Theme) -> highlighter::Format<Font> {
    highlighter::Format { color: Some(mark.color), font: Some(mark.font) }
}

/// What a run of chat lines flattens to.
#[derive(Default)]
pub(super) struct Flat {
    pub text: String,
    pub marks: LineMarks,
    /// `(line, byte range, message)` for `#geohash` tags and links.
    pub links: Vec<(usize, Range<usize>, Message)>,
}

/// Join lines of spans into editor text. Every byte gets a mark with an
/// explicit color, which is what lets the editor's own text color be
/// transparent (that color also paints the caret).
pub(super) fn flatten(lines: &[Vec<Span<'static, Message>>]) -> Flat {
    let mut flat = Flat::default();
    let mut line = 0usize;
    let mut col = 0usize;
    flat.marks.push(Vec::new());
    for (i, spans) in lines.iter().enumerate() {
        if i > 0 {
            flat.text.push('\n');
            flat.marks.push(Vec::new());
            line += 1;
            col = 0;
        }
        for span in spans {
            let mut color = span.color.unwrap_or_else(theme::green);
            if span.link.is_some() && span.underline && color == theme::green() {
                color = theme::green_soft();
            }
            let mark = Mark { color, font: span.font.unwrap_or(Font::MONOSPACE) };
            // A message can carry its own line breaks; each becomes an
            // editor line of its own.
            for (n, piece) in span.text.split('\n').enumerate() {
                if n > 0 {
                    flat.text.push('\n');
                    flat.marks.push(Vec::new());
                    line += 1;
                    col = 0;
                }
                let piece = piece.trim_end_matches('\r');
                if piece.is_empty() {
                    continue;
                }
                let range = col..col + piece.len();
                flat.text.push_str(piece);
                flat.marks[line].push((range.clone(), mark));
                if let Some(link) = &span.link {
                    flat.links.push((line, range, link.clone()));
                }
                col += piece.len();
            }
        }
    }
    flat
}

/// One editor's worth of consecutive chat lines, kept across frames so the
/// selection survives redraws.
pub(super) struct Block {
    pub text: String,
    marks: Marks,
    links: Vec<(usize, Range<usize>, Message)>,
    content: Content,
}

impl Block {
    pub fn new(flat: Flat) -> Self {
        Self {
            content: Content::with_text(&flat.text),
            text: flat.text,
            marks: Marks(Arc::new(flat.marks)),
            links: flat.links,
        }
    }

    /// Keep the content (and its selection) when the text is unchanged;
    /// colors and links are refreshed either way.
    pub fn refresh(&mut self, flat: Flat) {
        if flat.text != self.text {
            *self = Self::new(flat);
            return;
        }
        let marks = Marks(Arc::new(flat.marks));
        if marks != self.marks {
            self.marks = marks;
        }
        self.links = flat.links;
    }

    /// Apply a selection gesture; edits never reach the content. Returns
    /// the link under a single click, if any.
    pub fn perform(&mut self, action: Action) -> Option<Message> {
        match action {
            Action::Edit(_) | Action::Scroll { .. } => return None,
            Action::Click(_) => {
                self.content.perform(action);
                let (line, at) = self.content.cursor_position();
                return self
                    .links
                    .iter()
                    .find(|(l, range, _)| *l == line && range.start <= at && at < range.end)
                    .map(|(_, _, msg)| msg.clone());
            }
            _ => self.content.perform(action),
        }
        None
    }

    pub fn view(&self, index: usize, size: f32) -> Element<'_, Message> {
        iced::widget::text_editor(&self.content)
            .on_action(move |action| Message::ThreadText(index, action))
            .key_binding(read_only_keys)
            .highlight_with::<LineHighlighter>(self.marks.clone(), to_format)
            .font(Font::MONOSPACE)
            .size(size)
            .line_height(LineHeight::Relative(1.45))
            .wrapping(Wrapping::WordOrGlyph)
            .padding(0)
            .style(theme::thread_text)
            .into()
    }
}

/// Copy, select-all and selection moves; nothing that types or deletes.
fn read_only_keys(press: text_editor::KeyPress) -> Option<Binding<Message>> {
    match Binding::from_key_press(press)? {
        b @ (Binding::Copy
        | Binding::SelectAll
        | Binding::Select(_)
        | Binding::Move(_)
        | Binding::SelectWord
        | Binding::SelectLine
        | Binding::Unfocus) => Some(b),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::span;

    #[test]
    fn flattening_marks_every_byte_and_splits_breaks() {
        let lines = vec![
            vec![span("<@a> ").color(Color::WHITE), span("hi\nthere").color(Color::BLACK)],
            vec![span("#9q8yy").color(Color::WHITE).link(Message::JoinGeohash("9q8yy".into()))],
        ];
        let flat = flatten(&lines);
        assert_eq!(flat.text, "<@a> hi\nthere\n#9q8yy");
        assert_eq!(flat.marks.len(), 3);
        for (line, text) in flat.text.split('\n').enumerate() {
            let covered: usize = flat.marks[line].iter().map(|(r, _)| r.len()).sum();
            assert_eq!(covered, text.len(), "line {line}");
        }
        assert_eq!(flat.links.len(), 1);
        assert_eq!((flat.links[0].0, flat.links[0].1.clone()), (2, 0..6));
    }
}
