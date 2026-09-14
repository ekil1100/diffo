use unicode_width::UnicodeWidthChar;

/// Width is additive per scalar, shared with the cell fitter and TUI wrapping.
/// Terminal escapes and raw control characters have no display width.
pub fn display_width(text: &str) -> usize {
    units(text)
        .map(|unit| match unit {
            Unit::Escape(_) => 0,
            Unit::Character(ch) => ch.width().unwrap_or(0),
        })
        .sum()
}

/// Fit a terminal cell without forwarding cursor commands or splitting UTF-8.
/// As in the original renderer, raw controls occupy one blank placeholder cell.
pub fn fit_cell(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut visible = 0;
    for unit in units(text) {
        match unit {
            Unit::Escape(seq) => {
                if is_sgr(seq.as_bytes()) {
                    out.push_str(seq);
                }
            }
            Unit::Character(ch) => {
                let ch = if ch.is_control() { ' ' } else { ch };
                let cells = ch.width().unwrap_or(0);
                if cells > width - visible {
                    break;
                }
                out.push(ch);
                visible += cells;
            }
        }
    }
    out.extend(std::iter::repeat_n(' ', width - visible));
    out
}

/// Keep SGR styling, remove other terminal commands and controls, and normalize tabs.
/// This is a single-line primitive; callers copying multiple lines join them afterward.
pub fn sanitize(text: &str) -> String {
    clean_text(text, true)
}

/// The same safety rules as `sanitize`, but with all styling removed for copying.
pub fn plain_text(text: &str) -> String {
    clean_text(text, false)
}

fn clean_text(text: &str, keep_sgr: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for unit in units(text) {
        match unit {
            Unit::Escape(seq) if keep_sgr && is_sgr(seq.as_bytes()) => out.push_str(seq),
            Unit::Escape(_) => {}
            Unit::Character('\t') => out.push(' '),
            Unit::Character(ch) if !ch.is_control() => out.push(ch),
            Unit::Character(_) => {}
        }
    }
    out
}

/// Byte length of an escape at the beginning of `bytes`, or zero if none starts there.
/// Handles CSI, OSC, ST-terminated strings (DCS/SOS/PM/APC), and ordinary ESC forms.
/// UTF-8 encoded C1 introducers are also recognized, never forwarded as styling.
pub fn ansi_seq_len(bytes: &[u8]) -> usize {
    let (intro, start) = match bytes {
        [0x1b, intro, ..] => (*intro, 2),
        [0x1b] => return 1,
        [0xc2, 0x9b, ..] => (b'[', 2),
        [0xc2, 0x9d, ..] => (b']', 2),
        [0xc2, 0x90, ..] => (b'P', 2),
        [0xc2, 0x98, ..] => (b'X', 2),
        [0xc2, 0x9e, ..] => (b'^', 2),
        [0xc2, 0x9f, ..] => (b'_', 2),
        _ => return 0,
    };
    match intro {
        b'[' => {
            let mut i = start;
            while i < bytes.len() && (0x30..=0x3f).contains(&bytes[i]) {
                i += 1;
            }
            while i < bytes.len() && (0x20..=0x2f).contains(&bytes[i]) {
                i += 1;
            }
            if i < bytes.len() && (0x40..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            i
        }
        b']' | b'P' | b'X' | b'^' | b'_' => {
            let mut i = start;
            while i < bytes.len() {
                if intro == b']' && bytes[i] == 0x07 {
                    return i + 1;
                }
                if bytes[i..].starts_with(b"\x1b\\") || bytes[i..].starts_with(&[0xc2, 0x9c]) {
                    return i + 2;
                }
                i += 1;
            }
            bytes.len()
        }
        _ => {
            let mut i = 1;
            while i < bytes.len() && (0x20..=0x2f).contains(&bytes[i]) {
                i += 1;
            }
            if i < bytes.len() && (0x30..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            i
        }
    }
}

/// Only ordinary SGR parameters can pass the untrusted-text boundary.
pub fn is_sgr(seq: &[u8]) -> bool {
    seq.len() >= 3
        && seq.starts_with(b"\x1b[")
        && seq.last() == Some(&b'm')
        && seq[2..seq.len() - 1]
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b';' | b':'))
}

#[derive(Clone, Copy, Debug)]
enum Unit<'a> {
    Escape(&'a str),
    Character(char),
}

fn units(mut text: &str) -> impl Iterator<Item = Unit<'_>> {
    std::iter::from_fn(move || {
        if text.is_empty() {
            return None;
        }
        let count = ansi_seq_len(text.as_bytes());
        if count > 0 {
            let (seq, rest) = text.split_at(count);
            text = rest;
            Some(Unit::Escape(seq))
        } else {
            let ch = text.chars().next()?;
            text = &text[ch.len_utf8()..];
            Some(Unit::Character(ch))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_width_handles_cjk() {
        assert_eq!(display_width("a"), 1);
        assert_eq!(display_width("架"), 2);
        assert_eq!(display_width("架构a"), 5);
    }

    #[test]
    fn fit_cell_handles_ansi_and_cjk_width() {
        assert_eq!(fit_cell("\x1b[31m架a\x1b[0m", 4), "\x1b[31m架a\x1b[0m ");
        assert_eq!(fit_cell("架a", 1), " ");
        assert_eq!(fit_cell("架a", 2), "架");
        assert_eq!(fit_cell("架a", 0), "");
    }

    #[test]
    fn display_width_handles_combining_marks_emoji_and_controls() {
        for text in [
            "\u{301}", "\u{200d}", "\u{200b}", "\u{feff}", "\t", "\r", "\u{7f}", "\u{85}",
        ] {
            assert_eq!(display_width(text), 0, "{text:?}");
        }
        assert_eq!(display_width("😀"), 2);
        assert_eq!(display_width("e\u{301}"), 1);
        assert_eq!(display_width("\x1b[31m架a\x1b[0m"), 3);
    }

    #[test]
    fn fit_cell_replaces_control_bytes_and_drops_non_sgr_escapes() {
        assert_eq!(fit_cell("a\tb\x1b[2Kc", 4), "a bc");
        assert_eq!(fit_cell("a\r\n\x07\x7fb", 6), "a    b");
    }

    #[test]
    fn fit_cell_forwards_sgr_but_strips_osc() {
        assert_eq!(fit_cell("\x1b[31mx\x1b]0;evil\x07y", 4), "\x1b[31mxy  ");
        assert_eq!(
            sanitize("\x1b[38:2::1:2:3mx\x1b[m"),
            "\x1b[38:2::1:2:3mx\x1b[m"
        );
    }

    #[test]
    fn ansi_seq_len_bounds_csi_osc_and_lone_escape() {
        for (text, count) in [
            ("\x1b[31m", 5),
            ("\x1b[2K", 4),
            ("\x1b", 1),
            ("\x1b]0;hi\x07", 7),
            ("\x1b]0;hi\x1b\\", 8),
            ("\x1b[", 2),
            ("\x1b[31", 4),
            ("\x1b(0", 3),
            ("\x1b7", 2),
            ("x", 0),
            ("", 0),
        ] {
            assert_eq!(ansi_seq_len(text.as_bytes()), count, "{text:?}");
        }
    }

    #[test]
    fn sanitizer_removes_terminal_injection_and_controls() {
        let text = "a\t\x1b[31mb\x1b[0m\r\n\x07\x7f\x1b[2J\x1b[Hc\x1b]52;c;ZXZpbA==\x07d";
        assert_eq!(sanitize(text), "a \x1b[31mb\x1b[0mcd");
        assert_eq!(plain_text(text), "a bcd");
        for sequence in [
            "\x1bPpayload\x1b\\",
            "\x1b_payload\x1b\\",
            "\x1b^payload\x1b\\",
            "\x1bXpayload\x1b\\",
            "\u{9d}52;c;ZXZpbA==\u{9c}",
            "\u{90}payload\u{9c}",
            "\u{9b}2J",
            "\x1b]unterminated",
        ] {
            assert_eq!(sanitize(sequence), "", "{sequence:?}");
            assert_eq!(display_width(sequence), 0);
        }
    }

    #[test]
    fn malformed_escapes_do_not_split_utf8_or_pass_non_sgr_commands() {
        for text in ["\x1b架", "\x1b[31架", "\x1b[ 架", "\x1b\x1b架"] {
            assert_eq!(sanitize(text), "架");
            assert_eq!(fit_cell(text, 2), "架");
        }
        assert!(!is_sgr(b"\x1b[?1m"));
        assert!(!is_sgr(b"\x1b[1 m"));
        assert!(!is_sgr(b"\x1b[31\x07m"));
    }

    #[test]
    fn trailing_combining_mark_and_reset_survive_exact_fit() {
        assert_eq!(
            fit_cell("\x1b[31me\u{301}\x1b[0m", 1),
            "\x1b[31me\u{301}\x1b[0m"
        );
        assert_eq!(fit_cell("\x1b[31m架\x1b[0m", 2), "\x1b[31m架\x1b[0m");
    }

    #[test]
    fn fitted_width_and_sanitization_are_consistent() {
        for text in [
            "",
            "abc",
            "架😀e\u{301}",
            "\t\rabc",
            "\x1b[31mx\x1b[0m",
            "x\x1b]0;evil\x07y",
            "\u{9b}2J架",
        ] {
            for width in 0..12 {
                let fitted = fit_cell(text, width);
                assert_eq!(display_width(&fitted), width, "{text:?}, {width}");
                assert_eq!(sanitize(&fitted), fitted);
                assert_eq!(display_width(&plain_text(&fitted)), width);
            }
            assert_eq!(sanitize(&sanitize(text)), sanitize(text));
        }
    }
}
