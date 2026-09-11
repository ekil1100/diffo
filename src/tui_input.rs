// One Unix stdin consumer for the protocols diffo uses. Crossterm still owns terminal
// modes and event types, but its parser cannot represent all legacy Shift+Enter forms.
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::{
    io,
    os::fd::RawFd,
    time::{Duration, Instant},
};

const ESCAPE_TIMEOUT: Duration = Duration::from_millis(35);
const SEQUENCE_LIMIT: usize = 256;
const PASTE_LIMIT: usize = 1024 * 1024;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Default)]
pub(super) struct Input {
    decoder: Decoder,
    escape_since: Option<Instant>,
}
impl Input {
    pub fn read(&mut self, timeout: Duration) -> io::Result<Option<Event>> {
        self.read_fd(libc::STDIN_FILENO, timeout)
    }

    fn read_fd(&mut self, fd: RawFd, timeout: Duration) -> io::Result<Option<Event>> {
        if let Some(event) = self.decoder.next(false)? {
            self.escape_since = None;
            return Ok(Some(event));
        }
        let wait = if self.decoder.bytes == b"\x1b" {
            let since = *self.escape_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= ESCAPE_TIMEOUT {
                self.escape_since = None;
                return self.decoder.next(true);
            }
            timeout.min(ESCAPE_TIMEOUT.saturating_sub(since.elapsed()))
        } else {
            self.escape_since = None;
            timeout
        };
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pollfd points to one initialized descriptor for the duration of poll.
        let ready = unsafe {
            libc::poll(
                &mut pollfd,
                1,
                wait.as_millis().max(1).min(i32::MAX as u128) as i32,
            )
        };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        if ready == 0 {
            return Ok(None);
        }
        if pollfd.revents & libc::POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        let mut bytes = [0; 4096];
        // SAFETY: bytes is writable for its full length; the TUI alone reads stdin.
        // Read on HUP too, so queued bytes are delivered before reporting EOF.
        let count = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Terminal input closed",
            ));
        }
        self.decoder
            .bytes
            .extend_from_slice(&bytes[..count as usize]);
        self.decoder.next(false)
    }
}

#[derive(Default)]
struct Decoder {
    bytes: Vec<u8>,
    paste_scan: usize,
}
impl Decoder {
    fn consume(&mut self, count: usize) {
        self.bytes.drain(..count);
        self.paste_scan = 0;
    }

    fn next(&mut self, expire_escape: bool) -> io::Result<Option<Event>> {
        while !self.bytes.is_empty() {
            if self.bytes.starts_with(PASTE_START) {
                let start = self.paste_scan.max(PASTE_START.len());
                if let Some(end) = self.bytes[start..]
                    .windows(PASTE_END.len())
                    .position(|w| w == PASTE_END)
                {
                    let end = start + end;
                    if end - PASTE_START.len() > PASTE_LIMIT {
                        return Err(input_limit());
                    }
                    let text =
                        String::from_utf8_lossy(&self.bytes[PASTE_START.len()..end]).into_owned();
                    self.consume(end + PASTE_END.len());
                    return Ok(Some(Event::Paste(text)));
                }
                if self.bytes.len() > PASTE_LIMIT + PASTE_START.len() + PASTE_END.len() {
                    return Err(input_limit());
                }
                self.paste_scan = self.bytes.len().saturating_sub(PASTE_END.len() - 1);
                return Ok(None);
            }
            if self.bytes[0] != 27 {
                let Some((len, event)) = character(&self.bytes) else {
                    return Ok(None);
                };
                self.consume(len);
                if event.is_some() {
                    return Ok(event);
                }
                continue;
            }
            if self.bytes.len() == 1 {
                if !expire_escape {
                    return Ok(None);
                }
                self.consume(1);
                return Ok(Some(key(KeyCode::Esc, KeyModifiers::NONE)));
            }
            match self.bytes[1] {
                b'[' | b'O' => {
                    let Some(end) = self.bytes[2..]
                        .iter()
                        .position(|b| *b < 0x20 || (0x40..=0x7e).contains(b))
                        .map(|i| i + 2)
                    else {
                        if self.bytes.len() > SEQUENCE_LIMIT {
                            return Err(input_limit());
                        }
                        // A fragmented CSI/SS3 is never downgraded to Escape or text.
                        return Ok(None);
                    };
                    if end >= SEQUENCE_LIMIT {
                        return Err(input_limit());
                    }
                    if self.bytes[end] < 0x20 {
                        // An interrupted sequence must not trap Ctrl-C or a fresh Escape.
                        self.consume(end);
                        continue;
                    }
                    let event = if self.bytes[1] == b'[' {
                        csi(&self.bytes[2..end], self.bytes[end])
                    } else {
                        navigation(self.bytes[end]).map(|code| key(code, KeyModifiers::NONE))
                    };
                    self.consume(end + 1);
                    if event.is_some() {
                        return Ok(event);
                    }
                }
                b']' | b'P' | b'_' | b'^' => {
                    // Ignore terminal replies atomically, never replay their payload as keys.
                    let end = (2..self.bytes.len()).find_map(|i| {
                        if self.bytes[i] == 3 {
                            Some(i)
                        } else if self.bytes[i] == 7 {
                            Some(i + 1)
                        } else if self.bytes[i..].starts_with(b"\x1b\\") {
                            Some(i + 2)
                        } else {
                            None
                        }
                    });
                    if let Some(end) = end {
                        if end > SEQUENCE_LIMIT {
                            return Err(input_limit());
                        }
                        self.consume(end);
                    } else if self.bytes.len() > SEQUENCE_LIMIT {
                        return Err(input_limit());
                    } else {
                        return Ok(None);
                    }
                }
                27 => {
                    self.consume(1);
                    return Ok(Some(key(KeyCode::Esc, KeyModifiers::NONE)));
                }
                _ => {
                    let Some((len, event)) = character(&self.bytes[1..]) else {
                        return Ok(None);
                    };
                    self.consume(len + 1);
                    if let Some(Event::Key(mut event)) = event {
                        event.modifiers.insert(KeyModifiers::ALT);
                        return Ok(Some(Event::Key(event)));
                    }
                }
            }
        }
        Ok(None)
    }
}
fn input_limit() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Terminal input sequence exceeds size limit",
    )
}
fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}
fn character(bytes: &[u8]) -> Option<(usize, Option<Event>)> {
    let first = bytes[0];
    let code = match first {
        b'\r' | b'\n' => KeyCode::Enter,
        b'\t' => KeyCode::Tab,
        8 | 127 => KeyCode::Backspace,
        0..=26 => {
            return Some((
                1,
                Some(key(
                    KeyCode::Char(if first == 0 {
                        ' '
                    } else {
                        (b'a' + first - 1) as char
                    }),
                    KeyModifiers::CONTROL,
                )),
            ));
        }
        28..=31 => {
            return Some((
                1,
                Some(key(
                    KeyCode::Char((first + 64) as char),
                    KeyModifiers::CONTROL,
                )),
            ));
        }
        32..=126 => KeyCode::Char(first as char),
        _ => {
            let len = match first {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Some((1, None)),
            };
            // Reject invalid prefixes promptly without swallowing a following control byte.
            match std::str::from_utf8(&bytes[..bytes.len().min(len)]) {
                Ok(text) => {
                    return Some((
                        len,
                        text.chars()
                            .next()
                            .map(|c| key(KeyCode::Char(c), KeyModifiers::NONE)),
                    ));
                }
                Err(error) if error.error_len().is_none() => return None,
                Err(_) => return Some((1, None)),
            }
        }
    };
    Some((1, Some(key(code, KeyModifiers::NONE))))
}
fn navigation(final_byte: u8) -> Option<KeyCode> {
    Some(match final_byte {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        _ => return None,
    })
}
fn modifiers(value: u32) -> KeyModifiers {
    let bits = value.saturating_sub(1);
    let mut result = KeyModifiers::NONE;
    for (bit, modifier) in [
        (1, KeyModifiers::SHIFT),
        (2, KeyModifiers::ALT),
        (4, KeyModifiers::CONTROL),
        (8, KeyModifiers::SUPER),
        (16, KeyModifiers::HYPER),
        (32, KeyModifiers::META),
    ] {
        if bits & bit != 0 {
            result.insert(modifier);
        }
    }
    result
}
fn csi(params: &[u8], final_byte: u8) -> Option<Event> {
    let params = std::str::from_utf8(params).ok()?;
    if params.starts_with('<') && matches!(final_byte, b'M' | b'm') {
        return mouse(&params[1..], final_byte == b'm');
    }
    // Zig accepted both separators and any Shift-bearing modifier for Enter.
    // Normalize these to Shift+Enter before interpreting Kitty alternate-key fields.
    let values: Vec<u16> = params
        .split([';', ':'])
        .filter(|s| !s.is_empty())
        .take(6)
        .map(|s| s.parse().unwrap_or(0))
        .collect();
    let shift_enter = values.len() >= 2
        && modifiers(values[1] as u32).contains(KeyModifiers::SHIFT)
        && (matches!(values[0], 10 | 13)
            || (values.len() >= 3 && values[0] == 27 && matches!(values[2], 10 | 13)));
    let fields: Vec<&str> = params.split(';').collect();
    let number =
        |index: usize| -> Option<u32> { fields.get(index)?.split(':').next()?.parse().ok() };
    let kind = match fields.get(1).and_then(|s| s.split(':').nth(1)) {
        Some("2") => KeyEventKind::Repeat,
        Some("3") => KeyEventKind::Release,
        _ => KeyEventKind::Press,
    };
    if matches!(final_byte, b'u' | b'~') && shift_enter {
        return Some(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::SHIFT,
            kind,
        )));
    }
    let mut mods = modifiers(number(1).unwrap_or(1));
    let mut code = if let Some(code) = navigation(final_byte) {
        code
    } else {
        match final_byte {
            b'Z' => {
                mods.insert(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            b'~' => match number(0)? {
                1 | 7 => KeyCode::Home,
                4 | 8 => KeyCode::End,
                3 => KeyCode::Delete,
                5 => KeyCode::PageUp,
                6 => KeyCode::PageDown,
                27 => unicode_key(number(2)?)?,
                _ => return None,
            },
            b'u' => unicode_key(number(0)?)?,
            b'I' if params.is_empty() => return Some(Event::FocusGained),
            b'O' if params.is_empty() => return Some(Event::FocusLost),
            _ => return None,
        }
    };
    if final_byte == b'u'
        && mods.contains(KeyModifiers::SHIFT)
        && let Some(shifted) = fields[0]
            .split(':')
            .nth(1)
            .and_then(|value| value.parse().ok())
            .and_then(char::from_u32)
    {
        code = KeyCode::Char(shifted);
    }
    Some(Event::Key(KeyEvent::new_with_kind(code, mods, kind)))
}
fn unicode_key(code: u32) -> Option<KeyCode> {
    Some(match code {
        10 | 13 => KeyCode::Enter,
        9 => KeyCode::Tab,
        27 => KeyCode::Esc,
        8 | 127 => KeyCode::Backspace,
        57350 => KeyCode::Left,
        57351 => KeyCode::Right,
        57352 => KeyCode::Up,
        57353 => KeyCode::Down,
        57354 => KeyCode::PageUp,
        57355 => KeyCode::PageDown,
        57356 => KeyCode::Home,
        57357 => KeyCode::End,
        57349 => KeyCode::Delete,
        // Other Kitty functional keys are not text and have no diffo binding.
        0xe000..=0xf8ff => return None,
        _ => KeyCode::Char(char::from_u32(code)?),
    })
}
fn mouse(params: &str, release: bool) -> Option<Event> {
    let mut fields = params.split(';');
    let bits: u16 = fields.next()?.parse().ok()?;
    let column = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let row = fields.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    if fields.next().is_some() || bits > 127 {
        return None;
    }
    let button = match bits & 3 {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        _ => MouseButton::Right,
    };
    let kind = if bits & 64 != 0 {
        match bits & 3 {
            0 => MouseEventKind::ScrollUp,
            1 => MouseEventKind::ScrollDown,
            2 => MouseEventKind::ScrollLeft,
            _ => MouseEventKind::ScrollRight,
        }
    } else if release {
        MouseEventKind::Up(button)
    } else if bits & 3 == 3 {
        MouseEventKind::Moved
    } else if bits & 32 != 0 {
        MouseEventKind::Drag(button)
    } else {
        MouseEventKind::Down(button)
    };
    let mut mods = KeyModifiers::NONE;
    for (bit, modifier) in [
        (4, KeyModifiers::SHIFT),
        (8, KeyModifiers::ALT),
        (16, KeyModifiers::CONTROL),
    ] {
        if bits & bit != 0 {
            mods.insert(modifier);
        }
    }
    Some(Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: mods,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::editor::{Action, Editor};
    use super::*;
    use std::{io::Write, os::fd::AsRawFd, os::unix::net::UnixStream};

    fn events(bytes: &[u8]) -> Vec<Event> {
        let mut decoder = Decoder {
            bytes: bytes.to_vec(),
            ..Decoder::default()
        };
        let mut result = Vec::new();
        while let Some(event) = decoder.next(false).unwrap() {
            result.push(event);
        }
        assert!(decoder.bytes.is_empty(), "Incomplete test input");
        result
    }

    #[test]
    fn all_legacy_shift_enter_forms_reach_editor_at_every_split() {
        for sequence in [
            "13;2u", "10;2u", "13:2u", "13;2~", "10;2~", "27;2;13~", "27;2;10~", "13;6u",
            "27:4:10~", "13;2:1u",
        ] {
            let sequence = format!("\x1b[{sequence}");
            for split in 0..=sequence.len() {
                let mut decoder = Decoder::default();
                let mut editor = Editor::default();
                editor.insert("before");
                decoder
                    .bytes
                    .extend_from_slice(&sequence.as_bytes()[..split]);
                let first = decoder.next(false).unwrap();
                if split < sequence.len() {
                    assert_eq!(first, None);
                }
                decoder
                    .bytes
                    .extend_from_slice(&sequence.as_bytes()[split..]);
                let event = first.or_else(|| decoder.next(false).unwrap()).unwrap();
                assert_eq!(
                    event,
                    key(KeyCode::Enter, KeyModifiers::SHIFT),
                    "{sequence:?}"
                );
                assert_eq!(editor.key(event.as_key_event().unwrap()), Action::Continue);
                editor.insert("after");
                assert_eq!(editor.body, "before\nafter");
                assert_eq!(editor.key(KeyEvent::from(KeyCode::Enter)), Action::Save);
                assert_eq!(decoder.next(false).unwrap(), None);
            }
        }
    }

    #[test]
    fn utf8_paste_mouse_and_keys_are_invariant_under_fragmentation() {
        let stream = "c架\x1b[D\x1b[200~paste\r\nqyc\x03\x1b[13:2u🦀\x1b[201~\x1b[<0;3;4M\x1b[<32;7;8M\x1b[<0;7;8m\x1b[<64;7;8M\x1b[<65;7;8M\x1b[97:65;2u\r";
        let expected = events(stream.as_bytes());
        assert_eq!(expected[0], key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert_eq!(expected[1], key(KeyCode::Char('架'), KeyModifiers::NONE));
        assert_eq!(
            expected[3],
            Event::Paste("paste\r\nqyc\x03\x1b[13:2u🦀".into())
        );
        assert_eq!(expected[9].as_key_event().unwrap().code, KeyCode::Char('A'));
        for size in 1..=stream.len() {
            let mut decoder = Decoder::default();
            let mut actual = Vec::new();
            for chunk in stream.as_bytes().chunks(size) {
                decoder.bytes.extend_from_slice(chunk);
                while let Some(event) = decoder.next(false).unwrap() {
                    actual.push(event);
                }
            }
            assert_eq!(actual, expected, "chunk size {size}");
            assert!(decoder.bytes.is_empty());
        }
    }

    #[test]
    fn escape_timeout_only_applies_to_a_bare_escape() {
        let mut decoder = Decoder {
            bytes: b"\x1b".to_vec(),
            ..Decoder::default()
        };
        assert_eq!(decoder.next(false).unwrap(), None);
        assert_eq!(
            decoder.next(true).unwrap(),
            Some(key(KeyCode::Esc, KeyModifiers::NONE))
        );
        for sequence in [b"\x1b[13;".as_slice(), b"\x1bO", b"\xe6\x9e"] {
            decoder.bytes = sequence.to_vec();
            assert_eq!(decoder.next(true).unwrap(), None);
            assert_eq!(decoder.bytes, sequence);
        }
    }

    #[test]
    fn unused_sequences_and_invalid_utf8_do_not_turn_into_commands() {
        let decoded = events(b"\x1b[13~\x1b[13;5u\x1b[5;13u\x1b[?1;2c\x1bOP\x1b]qyc\x07\x1bPqyc\x1b\\\xff\xe6\x03\x1b[999;\x03\x1b]broken\x03");
        let mut editor = Editor::default();
        editor.insert("body");
        let mut quits = 0;
        for event in decoded {
            if editor.key(event.as_key_event().unwrap()) == Action::Quit {
                quits += 1;
            }
        }
        assert_eq!(quits, 3);
        assert_eq!(editor.body, "body");
    }

    #[test]
    fn navigation_ctrl_keys_and_mouse_have_explicit_mappings() {
        for (sequence, code) in [
            ("\x1b[A", KeyCode::Up),
            ("\x1bOB", KeyCode::Down),
            ("\x1bOC", KeyCode::Right),
            ("\x1b[D", KeyCode::Left),
            ("\x1b[3~", KeyCode::Delete),
            ("\x1b[5~", KeyCode::PageUp),
            ("\x1b[6~", KeyCode::PageDown),
            ("\x1bOH", KeyCode::Home),
            ("\x1bOF", KeyCode::End),
            ("\x1b[7~", KeyCode::Home),
            ("\x1b[8~", KeyCode::End),
            ("\x7f", KeyCode::Backspace),
            ("\x08", KeyCode::Backspace),
            ("\t", KeyCode::Tab),
            ("\n", KeyCode::Enter),
        ] {
            assert_eq!(events(sequence.as_bytes()), [key(code, KeyModifiers::NONE)]);
        }
        for sequence in [b"\x03".as_slice(), b"\x1b[99;5u", b"\x1b[27;5;99~"] {
            assert_eq!(
                events(sequence),
                [key(KeyCode::Char('c'), KeyModifiers::CONTROL)]
            );
        }
        assert_eq!(
            events(b"\x1b[<28;3;4M"),
            [Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 3,
                modifiers: KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL,
            })]
        );
        assert!(events(b"\x1b[<0;0;2M\x1b[<128;3;4M").is_empty());
        let mut editor = Editor::default();
        for event in events(b"\x1b[13;2:3u\x1b[13;1:3u\x1b[99;5:3u\x04") {
            assert_eq!(editor.key(event.as_key_event().unwrap()), Action::Continue);
        }
        assert!(editor.body.is_empty());
    }

    #[test]
    fn oversized_sequences_and_pastes_fail_instead_of_replaying_input() {
        for bytes in [
            [b"\x1b[".as_slice(), &vec![b'1'; SEQUENCE_LIMIT]].concat(),
            [b"\x1b]".as_slice(), &vec![b'q'; SEQUENCE_LIMIT], b"\x07"].concat(),
            [PASTE_START, &vec![b'y'; PASTE_LIMIT + 1], PASTE_END].concat(),
            [PASTE_START, &vec![b'y'; PASTE_LIMIT + 20]].concat(),
        ] {
            let mut decoder = Decoder {
                bytes,
                ..Decoder::default()
            };
            assert_eq!(
                decoder.next(false).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        let bytes = [PASTE_START, &vec![b'a'; PASTE_LIMIT], PASTE_END].concat();
        assert_eq!(events(&bytes), [Event::Paste("a".repeat(PASTE_LIMIT))]);
    }

    #[test]
    fn reader_preserves_queued_events_before_eof_and_reports_bad_fd() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"c\x1b[13:2u\x03").unwrap();
        drop(writer);
        let mut input = Input::default();
        for expected in [
            key(KeyCode::Char('c'), KeyModifiers::NONE),
            key(KeyCode::Enter, KeyModifiers::SHIFT),
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            assert_eq!(
                input.read_fd(reader.as_raw_fd(), Duration::ZERO).unwrap(),
                Some(expected)
            );
        }
        assert_eq!(
            input
                .read_fd(reader.as_raw_fd(), Duration::ZERO)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        // An out-of-range positive descriptor cannot be reused by a parallel test.
        assert_eq!(
            input
                .read_fd(i32::MAX, Duration::ZERO)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EBADF)
        );
    }

    #[test]
    #[ignore = "Requires a controlling PTY; run tests/terminal.py --test-binary"]
    fn terminal_input_error_probe() {
        use super::super::TerminalGuard;
        use crossterm::terminal;
        // Exercise real read EOF/error paths while retaining a PTY for restoration checks.
        for eof in [true, false] {
            let (reader, writer) = UnixStream::pair().unwrap();
            drop(writer);
            let result = (|| -> io::Result<()> {
                let mut guard = TerminalGuard::enter()?;
                guard.editor_keys(true)?;
                Input::default().read_fd(
                    if eof { reader.as_raw_fd() } else { i32::MAX },
                    Duration::ZERO,
                )?;
                Ok(())
            })();
            let error = result.unwrap_err();
            if eof {
                assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
            } else {
                assert_eq!(error.raw_os_error(), Some(libc::EBADF));
            }
            assert!(!terminal::is_raw_mode_enabled().unwrap());
        }
    }
}
