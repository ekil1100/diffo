use super::{Layout, plain_text, style_cell};
use crate::{
    theme::{Ansi, ThemeTokens},
    tui_text::{display_width, fit_cell},
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Default, Debug)]
pub(super) struct Editor {
    pub body: String,
    pub cursor: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Continue,
    Save,
    Cancel,
    Quit,
}

impl Editor {
    pub fn insert(&mut self, text: &str) {
        self.body.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let clean = text
            .split('\n')
            .map(plain_text)
            .collect::<Vec<_>>()
            .join("\n");
        self.insert(&clean);
    }

    fn previous(&self) -> usize {
        self.body[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.body[self.cursor..]
            .chars()
            .next()
            .map_or(self.body.len(), |c| self.cursor + c.len_utf8())
    }

    fn line_start(&self, cursor: usize) -> usize {
        self.body[..cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, cursor: usize) -> usize {
        self.body[cursor..]
            .find('\n')
            .map_or(self.body.len(), |i| cursor + i)
    }

    fn vertical(&mut self, down: bool) {
        let start = self.line_start(self.cursor);
        let end = self.line_end(self.cursor);
        let column = display_width(&self.body[start..self.cursor]);
        let (start, end) = if down {
            if end == self.body.len() {
                return;
            }
            (end + 1, self.line_end(end + 1))
        } else {
            if start == 0 {
                return;
            }
            (self.line_start(start - 1), start - 1)
        };
        let mut width = 0;
        self.cursor = start;
        for c in self.body[start..end].chars() {
            let next = width + display_width(&c.to_string());
            if next > column {
                break;
            }
            width = next;
            self.cursor += c.len_utf8();
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::Continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return if key.code == KeyCode::Char('c') {
                Action::Quit
            } else {
                Action::Continue
            };
        }
        match key.code {
            KeyCode::Esc => return Action::Cancel,
            KeyCode::Enter | KeyCode::Char('\n') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.insert("\n")
            }
            KeyCode::Enter if key.modifiers.is_empty() => return Action::Save,
            KeyCode::Backspace => {
                let start = self.previous();
                self.body.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            KeyCode::Delete => {
                self.body.replace_range(self.cursor..self.next(), "");
            }
            KeyCode::Left => self.cursor = self.previous(),
            KeyCode::Right => self.cursor = self.next(),
            KeyCode::Up => self.vertical(false),
            KeyCode::Down => self.vertical(true),
            KeyCode::Home => self.cursor = self.line_start(self.cursor),
            KeyCode::End => self.cursor = self.line_end(self.cursor),
            KeyCode::Tab => self.insert("\t"),
            KeyCode::Char(c) if !c.is_control() && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert(&c.to_string())
            }
            _ => {}
        }
        Action::Continue
    }

    pub fn draw(&self, terminal: Layout, ansi: Ansi, palette: ThemeTokens) -> String {
        let layout = EditorLayout::new(terminal);
        if layout.width < 3 || layout.height == 0 {
            return String::new();
        }
        let rows = visual_lines(&self.body, layout.content_width);
        let cursor_row = rows
            .iter()
            .position(|line| line.contains(self.cursor))
            .unwrap_or(rows.len() - 1);
        let first = (cursor_row + 1).saturating_sub(layout.body_rows);
        let mut out = String::new();
        for row in 0..layout.height {
            let text = rows
                .get(first + row)
                .map_or("", |line| &self.body[line.start..line.end]);
            let raw = format!(" {}", fit_cell(text, layout.content_width));
            put(
                &mut out,
                layout.x,
                layout.y + row,
                &style_cell(
                    &raw,
                    layout.width,
                    ansi,
                    palette.bg_panel,
                    palette.fg_default,
                ),
            );
        }
        let line = &rows[cursor_row];
        let column = display_width(&self.body[line.start..self.cursor.min(line.end)])
            .min(layout.content_width);
        out.push_str(&format!(
            "\x1b[{};{}H\x1b[?25h",
            layout.y + 1 + cursor_row - first,
            layout.x + 2 + column
        ));
        out
    }
}

fn put(out: &mut String, x: usize, y: usize, text: &str) {
    out.push_str(&format!("\x1b[{};{}H{}", y + 1, x + 1, text));
}

#[derive(Debug)]
struct EditorLayout {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    content_width: usize,
    body_rows: usize,
}
impl EditorLayout {
    fn new(terminal: Layout) -> Self {
        let width = terminal.main_width;
        let height = terminal.dock_height.saturating_sub(1);
        Self {
            x: terminal.main_x,
            y: terminal.dock_y + 1,
            width,
            height,
            content_width: width.saturating_sub(2).max(1),
            body_rows: height,
        }
    }
}

#[derive(Debug)]
struct VisualLine {
    start: usize,
    end: usize,
    next: usize,
}
impl VisualLine {
    fn contains(&self, cursor: usize) -> bool {
        cursor >= self.start && cursor <= self.end && !(self.next > self.end && cursor == self.next)
    }
}

fn visual_lines(body: &str, width: usize) -> Vec<VisualLine> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start < body.len() {
        let mut end = start;
        let mut visible = 0;
        for c in body[start..].chars() {
            if c == '\n' {
                break;
            }
            let cells = display_width(&c.to_string());
            if visible > 0 && visible + cells > width.max(1) {
                break;
            }
            end += c.len_utf8();
            visible += cells;
        }
        let next = if body.as_bytes().get(end) == Some(&b'\n') {
            end + 1
        } else {
            end
        };
        lines.push(VisualLine { start, end, next });
        start = next;
    }
    if lines.is_empty() || body.ends_with('\n') {
        lines.push(VisualLine {
            start: body.len(),
            end: body.len(),
            next: body.len(),
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn utf8_insertion_backspace_and_delete() {
        let mut e = Editor::default();
        e.insert("ab架");
        e.key(key(KeyCode::Backspace));
        assert_eq!(e.body, "ab");
        e.key(key(KeyCode::Left));
        e.insert("字");
        e.key(key(KeyCode::Delete));
        assert_eq!(e.body, "a字");
        assert_eq!(e.cursor, 4);
    }

    #[test]
    fn multiline_actions_and_paste_are_not_commands() {
        let mut e = Editor::default();
        e.insert("first");
        assert_eq!(
            e.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            Action::Continue
        );
        e.paste("second\r\nq\x1b]52;c;evil\x07");
        assert_eq!(e.body, "first\nsecond\nq");
        assert_eq!(e.key(key(KeyCode::Enter)), Action::Save);
        assert_eq!(e.key(key(KeyCode::Esc)), Action::Cancel);
        assert_eq!(
            e.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        let body = e.body.clone();
        assert_eq!(
            e.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Action::Continue
        );
        assert_eq!(e.body, body);
    }

    #[test]
    fn vertical_arrows_preserve_cell_column() {
        let mut e = Editor::default();
        e.insert("abc\n架de\nxy");
        e.cursor = 7;
        e.vertical(false);
        assert_eq!(e.cursor, 2);
        e.vertical(true);
        assert_eq!(e.cursor, 7);
        e.vertical(true);
        assert_eq!(e.cursor, 12);
    }

    #[test]
    fn editor_docks_and_wraps_cjk_and_empty_tail() {
        let state = super::super::State {
            comment_open: true,
            focus: super::super::Focus::Editor,
            ..Default::default()
        };
        let l = EditorLayout::new(Layout::new(120, 40).with_panels(&state));
        assert_eq!(
            (l.x, l.y, l.width, l.height, l.content_width, l.body_rows),
            (0, 32, 120, 7, 118, 7)
        );
        let lines = visual_lines("架abc", 3);
        assert_eq!((lines[0].start, lines[0].end, lines[1].start), (0, 4, 4));
        assert_eq!(visual_lines("one\n", 3).len(), 2);
        let rows = visual_lines("abcdefghi", 4);
        assert_eq!(rows.iter().position(|l| l.contains(9)), Some(2));
        assert_eq!(rows.iter().position(|l| l.contains(4)), Some(0));
    }

    #[test]
    fn editor_content_width_is_exact_and_cursor_tracks_tail() {
        let mut e = Editor::default();
        e.insert("架ab\nlast");
        let state = super::super::State {
            comment_open: true,
            focus: super::super::Focus::Editor,
            ..Default::default()
        };
        let screen = e.draw(
            Layout::new(60, 8).with_panels(&state),
            Ansi {
                enabled: false,
                true_color: false,
            },
            crate::theme::catppuccin_mocha(),
        );
        assert!(screen.contains("架ab"));
        assert!(screen.ends_with("\x1b[?25h"));
        for line in screen.split('H').filter(|s| s.starts_with(' ')) {
            assert_eq!(display_width(line.split('\x1b').next().unwrap()), 60);
        }
        assert!(screen.starts_with("\x1b[6;1H"));
    }
}
