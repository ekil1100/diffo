use std::ops::Range;

/// Tracks fully styled, terminal-width rows, independently of the Git diff model.
#[derive(Default)]
pub(super) struct Screen {
    width: usize,
    lines: Vec<String>,
}

impl Screen {
    pub fn invalidate(&mut self) {
        self.lines.clear();
    }

    /// An overlay painted outside this cache must be erased on the next frame.
    pub fn invalidate_rows(&mut self, rows: Range<usize>) {
        for line in self.lines.iter_mut().take(rows.end).skip(rows.start) {
            line.clear();
        }
    }

    pub fn draw(&mut self, lines: Vec<String>, width: usize, body: Range<usize>) -> String {
        let mut out = String::from("\x1b[?2026h\x1b[?25l");
        let same_size = self.width == width && self.lines.len() == lines.len();
        if same_size {
            let shift = self.scroll_shift(&lines, body.clone());
            if shift != 0 {
                let count = shift.unsigned_abs();
                // DECSTBM is vertical-only: any sidebar cells moved with the code
                // are compared below and repainted at their original positions.
                out.push_str(&format!(
                    "\x1b[{};{}r\x1b[{count}{}\x1b[r",
                    body.start + 1,
                    body.end,
                    if shift > 0 { 'S' } else { 'T' },
                ));
                let previous = &mut self.lines[body.clone()];
                if shift > 0 {
                    previous.rotate_left(count);
                    self.invalidate_rows(body.end - count..body.end);
                } else {
                    previous.rotate_right(count);
                    self.invalidate_rows(body.start..body.start + count);
                }
            }
        }
        for (y, line) in lines.iter().enumerate() {
            // Chrome is always fully painted; body rows are already width-padded.
            if !same_size || y == 0 || y + 1 == lines.len() || self.lines[y] != *line {
                out.push_str(&format!("\x1b[{};1H{line}", y + 1));
            }
        }
        self.width = width;
        self.lines = lines;
        out
    }

    fn scroll_shift(&self, lines: &[String], body: Range<usize>) -> isize {
        let old = &self.lines[body.clone()];
        let new = &lines[body];
        let mut best = Self::paint_cost(old, new, 0, usize::MAX);
        if best == 0 {
            return 0;
        }
        let mut shift = 0;
        // Compare only terminal rows, not source lines. Stop costing a candidate
        // as soon as it cannot beat the current result; no scrolling on ties.
        for count in 1..old.len() {
            for delta in [count as isize, -(count as isize)] {
                let cost = 32 + Self::paint_cost(old, new, delta, best);
                if cost < best {
                    best = cost;
                    shift = delta;
                }
            }
        }
        shift
    }

    fn paint_cost(old: &[String], new: &[String], shift: isize, limit: usize) -> usize {
        let mut cost = 0;
        for (y, line) in new.iter().enumerate() {
            let previous = y.checked_add_signed(shift).and_then(|index| old.get(index));
            if previous != Some(line) {
                cost += line.len() + 16;
                if cost >= limit {
                    break;
                }
            }
        }
        cost
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui_text::fit_cell;

    // Retain complete styled rows so comparisons catch stale colors as well as text.
    struct Terminal {
        rows: Vec<String>,
        region: Range<usize>,
    }
    impl Terminal {
        fn new(height: usize) -> Self {
            Self {
                rows: vec![String::new(); height],
                region: 0..height,
            }
        }
        fn apply(&mut self, output: &str) {
            let mut row = 0;
            for part in output.split("\x1b[").skip(1) {
                let end = part
                    .bytes()
                    .position(|byte| byte.is_ascii_alphabetic())
                    .unwrap();
                let command = part.as_bytes()[end];
                let params = &part[..end];
                match command {
                    b'H' => {
                        row = params.split(';').next().unwrap().parse::<usize>().unwrap() - 1;
                        self.rows[row].clear();
                    }
                    b'm' => {
                        self.rows[row].push_str("\x1b[");
                        self.rows[row].push_str(&part[..=end]);
                    }
                    b'r' => {
                        self.region = if params.is_empty() {
                            0..self.rows.len()
                        } else {
                            let (top, bottom) = params.split_once(';').unwrap();
                            top.parse::<usize>().unwrap() - 1..bottom.parse().unwrap()
                        };
                    }
                    b'S' | b'T' => {
                        let count = params.parse::<usize>().unwrap();
                        let rows = &mut self.rows[self.region.clone()];
                        if command == b'S' {
                            rows.rotate_left(count);
                            let len = rows.len();
                            rows[len - count..].fill_with(String::new);
                        } else {
                            rows.rotate_right(count);
                            rows[..count].fill_with(String::new);
                        }
                    }
                    b'h' | b'l' => {}
                    _ => panic!("Unexpected terminal command: {part:?}"),
                }
                self.rows[row].push_str(&part[end + 1..]);
            }
        }
    }

    fn styled(text: &str, width: usize, selected: bool) -> String {
        format!(
            "\x1b[{}m{}\x1b[0m",
            if selected { 35 } else { 32 },
            fit_cell(text, width)
        )
    }

    #[test]
    fn scrolling_matches_full_frames_including_styles_sidebar_and_dock() {
        for sidebar in [false, true] {
            let mut screen = Screen::default();
            let mut terminal = Terminal::new(18);
            let mut saw_up = false;
            let mut saw_down = false;
            for first in (0..40).chain((0..40).rev()) {
                let mut lines = vec![styled("file.rs", 80, false)];
                for y in 1..14 {
                    let side = if sidebar && y < 4 {
                        format!("file {y}")
                    } else {
                        String::new()
                    };
                    lines.push(styled(
                        &format!("{side:8} | line {} 架构 e\u{301}", first + y),
                        80,
                        y == 13,
                    ));
                }
                lines.extend((0..3).map(|y| styled(&format!("comment row {y}"), 80, false)));
                lines.push(styled(&format!("CODE {}", first + 13), 80, false));
                let output = screen.draw(lines.clone(), 80, 1..14);
                saw_up |= output.contains("\x1b[1S");
                saw_down |= output.contains("\x1b[1T");
                terminal.apply(&output);
                assert_eq!(terminal.rows, lines, "first={first} sidebar={sidebar}");
                assert_eq!(terminal.region, 0..18);
            }
            assert!(saw_up && saw_down);
        }
    }

    #[test]
    fn resize_and_invalidation_remove_stale_rows() {
        let mut screen = Screen::default();
        for (width, height) in [(1, 3), (19, 8), (120, 30), (80, 24)] {
            let lines: Vec<_> = (0..height)
                .map(|y| styled(&format!("row {y}"), width, false))
                .collect();
            let mut terminal = Terminal::new(height);
            let output = screen.draw(lines.clone(), width, 1..height - 1);
            assert_eq!(output.matches(";1H").count(), height);
            terminal.apply(&output);
            assert_eq!(terminal.rows, lines);
            let unchanged = screen.draw(lines.clone(), width, 1..height - 1);
            assert_eq!(unchanged.matches(";1H").count(), 2);
            assert!(!unchanged.contains("\x1b[r"));
            terminal.apply(&unchanged);
            assert_eq!(terminal.rows, lines);
            let overlay = 1..height - 1;
            terminal.rows[overlay.clone()].fill("overlay text".into());
            screen.invalidate_rows(overlay);
            terminal.apply(&screen.draw(lines.clone(), width, 1..height - 1));
            assert_eq!(terminal.rows, lines);
            screen.invalidate();
            assert_eq!(
                screen
                    .draw(lines, width, 1..height - 1)
                    .matches(";1H")
                    .count(),
                height
            );
        }
    }
}
