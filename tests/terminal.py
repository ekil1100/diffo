#!/usr/bin/env python3
"""Run PTY input and cleanup regression tests with isolated Git/HOME/state."""
import argparse
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import sys
import subprocess
import tempfile
import termios
import time
import unicodedata


class Screen:
    """Replay the cursor, erase and vertical-scroll commands emitted by diffo."""
    def resize(self, width, height):
        self.width, self.height = width, height
        self.rows = [[" "] * width for _ in range(height)]
        self.x = self.y = 0
        self.top, self.bottom = 0, height
        self.last_cell = None

    def feed(self, text):
        for part in re.split(r"(\x1b\[[0-?]*[ -/]*[@-~])", text):
            if part.startswith("\x1b["):
                command, raw = part[-1], part[2:-1]
                if raw.startswith(("?", ">", "<")):
                    continue
                args = [int(n) if n else 0 for n in raw.split(";")]
                if command == "H":
                    self.y = min(max(1, args[0]) - 1, self.height - 1)
                    self.x = min(max(1, args[1] if len(args) > 1 else 1) - 1, self.width - 1)
                    self.last_cell = None
                elif command == "J" and args == [2]:
                    self.rows = [[" "] * self.width for _ in range(self.height)]
                elif command == "K":
                    self.rows[self.y][self.x:] = [" "] * (self.width - self.x)
                elif command == "r":
                    self.top = max(1, args[0]) - 1
                    self.bottom = args[1] if len(args) > 1 else self.height
                elif command in ("S", "T"):
                    count = min(max(1, args[0]), self.bottom - self.top)
                    region = self.rows[self.top:self.bottom]
                    blank = [[" "] * self.width for _ in range(count)]
                    self.rows[self.top:self.bottom] = region[count:] + blank if command == "S" else blank + region[:-count]
            else:
                for char in part:
                    if ord(char) < 32:
                        continue
                    if unicodedata.combining(char):
                        if self.last_cell:
                            y, x = self.last_cell
                            self.rows[y][x] += char
                        continue
                    width = 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
                    self.rows[self.y][self.x] = char
                    self.last_cell = self.y, self.x
                    if width == 2 and self.x + 1 < self.width:
                        self.rows[self.y][self.x + 1] = ""
                    self.x = min(self.width - 1, self.x + width)

    def lines(self):
        return ["".join(row) for row in self.rows]


SAMPLE = "first\nsecond\nthird\n"


class Session:
    def __init__(self, binary, root, argv=(), frame=True, content=SAMPLE):
        self.root = root
        root.mkdir(parents=True, exist_ok=True)
        self.repo = root / "repo"
        self.repo.mkdir()
        self.home = root / "home"
        self.home.mkdir()
        self.state = root / "state"
        stub = root / "bin"
        stub.mkdir()
        for command in ["wl-copy", "pbcopy", "xclip", "xsel", "tmux"]:
            path = stub / command
            path.write_text('#!/bin/sh\nprintf "called\\n" >> "$HOME/clipboard-called"\ncat >"$HOME/clipboard-content"\n')
            path.chmod(0o755)
        self.env = {
            "PATH": str(stub) + os.pathsep + os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": str(self.home), "XDG_STATE_HOME": str(self.state),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
            "TERM": "xterm-256color", "COLORTERM": "truecolor",
            "LANG": "en_US.UTF-8", "USER": "pty-test",
        }
        subprocess.run(["git", "init", "-q", str(self.repo)], env=self.env, check=True)
        files = content if isinstance(content, dict) else {"sample.txt": content}
        for name, body in files.items():
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(body)
        self.master, self.slave = pty.openpty()
        self.original = termios.tcgetattr(self.slave)
        self.screen = Screen()
        self.screen_offset = 0
        self.resize(100, 24)

        def setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        status_read, status_write = os.pipe()
        release_read, self.release_write = os.pipe()
        # Keep the controlling session alive to inspect termios after diffo exits on macOS.
        # The supervisor never reads stdin; its handshake uses separate pipes.
        supervisor = """
import os, subprocess, sys
status, release = int(sys.argv[1]), int(sys.argv[2])
child = subprocess.Popen(sys.argv[3:])
os.write(status, (str(child.pid) + "\\n").encode())
code = child.wait()
os.write(status, (str(code) + "\\n").encode())
os.read(release, 1)
"""
        self.process = subprocess.Popen(
            [sys.executable, "-c", supervisor, str(status_write), str(release_read), str(binary), *argv],
            cwd=self.repo, env=self.env, stdin=self.slave, stdout=self.slave, stderr=self.slave,
            preexec_fn=setup, pass_fds=(status_write, release_read),
        )
        os.close(status_write)
        os.close(release_read)
        self.status = os.fdopen(status_read, "rb", buffering=0)
        self.pid = int(self.status.readline())
        self.output = bytearray()
        try:
            self.wait_for(b"\x1b[?2004h")
            if frame:
                self.wait_for(b"\x1b[?2026l")
        except BaseException:
            self.close()
            raise

    def resize(self, width, height):
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
        self.screen.resize(width, height)

    def pump(self, seconds=0.1):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], max(0, deadline - time.monotonic()))[0]:
                try:
                    data = os.read(self.master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not data:
                    break
                self.output.extend(data)

    def wait_for(self, marker, start=0, timeout=5):
        deadline = time.monotonic() + timeout
        while marker not in self.output[start:]:
            assert time.monotonic() < deadline, (marker, bytes(self.output[-500:]))
            self.pump(0.02)

    def send(self, data):
        os.write(self.master, data)

    def navigate(self, keys, marker):
        start = len(self.output)
        self.send(keys)
        self.wait_for(marker, start)
        self.wait_for(b"\x1b[?2026l", self.output.rfind(marker, start))
        return self.screen_lines()

    def screen_lines(self):
        end = self.output.rfind(b"\x1b[?2026l")
        if end >= self.screen_offset:
            end += len(b"\x1b[?2026l")
            self.screen.feed(self.output[self.screen_offset:end].decode())
            self.screen_offset = end
        return self.screen.lines()

    def comments(self):
        files = list(self.state.glob("diffo/repos/*/comments.json"))
        return json.loads(files[0].read_text())["comments"] if files else []

    def editor(self):
        start = len(self.output)
        self.send(b"c")
        self.wait_for(b"Shift+Enter newline", start)
        self.wait_for(b"\x1b[?2026l", self.output.rfind(b"Shift+Enter newline", start))

    def save(self, expected):
        self.send(b"\r")
        deadline = time.monotonic() + 5
        while [comment["body"] for comment in self.comments()] != expected:
            assert time.monotonic() < deadline, self.comments()
            self.pump(0.02)

    def finish(self, code=0, clipboard=False):
        deadline = time.monotonic() + 5
        while not select.select([self.status], [], [], 0)[0]:
            assert time.monotonic() < deadline, "Process did not exit"
            self.pump(0.02)
        actual_code = int(self.status.readline())
        self.pump()
        assert actual_code == code, (actual_code, bytes(self.output[-500:]))
        for marker in [b"\x1b[?2004l", b"\x1b[?1006l", b"\x1b[?25h", b"\x1b[?7h", b"\x1b[?1049l"]:
            assert marker in self.output, marker
        restored = termios.tcgetattr(self.slave)
        # PENDIN is a kernel-maintained input retype flag, not a raw-mode setting.
        restored[3] &= ~getattr(termios, "PENDIN", 0)
        original = self.original.copy()
        original[3] &= ~getattr(termios, "PENDIN", 0)
        assert restored == original, "Terminal attributes not restored"
        assert b"\x1b[?2026l\x1b[r\x1b[0m" in self.output, "Scroll margins not restored on exit"
        assert b"\x1b]52;" not in self.output, "Unexpected clipboard operation"
        assert (self.home / "clipboard-called").exists() == clipboard, "Unexpected clipboard command state"
        if b"\x1b[>1u" in self.output:
            assert b"\x1b[<u\x1b[>4;0m\x1b[>4f" in self.output, "Editor keys not restored"
        os.write(self.release_write, b"x")
        self.process.wait(timeout=5)

    def close(self):
        if self.process.poll() is None:
            try:
                os.kill(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            self.process.kill()
            # macOS session teardown can wait for queued PTY output to drain.
            self.pump(0.1)
            self.process.wait(timeout=5)
        self.status.close()
        os.close(self.release_write)
        os.close(self.master)
        os.close(self.slave)


def parity(session):
    expected = []
    sequences = [b"13;2u", b"10;2u", b"13:2u", b"13;2~", b"10;2~", b"27;2;13~", b"27;2;10~"]
    for fragmented in [False, True]:
        for sequence in sequences:
            session.editor()
            session.send(b"before")
            if fragmented:
                session.send(b"\x1b")
                session.pump(0.005)
                session.send(b"[" + sequence[:-2])
                session.pump(0.08)
                session.send(sequence[-2:])
            else:
                session.send(b"\x1b[" + sequence)
            session.send(b"after")
            session.pump(0.1)
            assert len(session.comments()) == len(expected), (sequence, "Shift+Enter saved prematurely")
            expected.append("before\nafter")
            session.save(expected)
    # Keys queued after Save must survive the editor/review consumer transition.
    session.send(b"cone\rctwo\r")
    expected += ["one", "two"]
    deadline = time.monotonic() + 5
    while len(session.comments()) != len(expected):
        assert time.monotonic() < deadline, session.comments()
        session.pump(0.02)
    assert [c["body"] for c in session.comments()] == expected
    session.send(b"q")
    session.finish()


def editing(session):
    session.editor()
    session.send(b"ab\xe6")
    session.pump(0.08)
    session.send(b"\x9e")
    session.pump(0.08)
    session.send(b"\xb6\x1b[D\x1b[DX\x1b[3~\x7f\x1b[FE\x1b[HH")
    session.send(b"\x1b[13~\x1b[13;5u\x1b[5;13u\x04")
    session.save(["Ha架E"])
    session.editor()
    # Paste includes command-looking text and controls; clipboard binaries are stubbed too.
    body = "qyc架\n" * 900
    paste = b"\x1b[200~" + body.encode() + b"\x1b]52;c;evil\x07\x03\x1b[201~"
    for chunk in [paste[:3], paste[3:4000], paste[4000:-3], paste[-3:]]:
        session.send(chunk)
        session.pump(0.06)
    session.send(b"tail")
    session.pump(0.1)
    assert len(session.comments()) == 1
    session.save(["Ha架E", body + "tail"])
    session.editor()
    session.send(b"discard")
    start = len(session.output)
    session.send(b"\x1b")
    session.wait_for(b"\x1b[<u", start)
    assert len(session.comments()) == 2
    session.send(b"q")
    session.finish()


def cursor_first(session):
    rows = session.navigate(b"j", b"CODE new:2")
    first_row = rows[1]
    assert not first_row.startswith("> "), rows
    assert sum(row.startswith("> ") for row in rows[1:-1]) == 1, rows
    rows = session.navigate(b"Vj", b"SELECT 2 rows  new:3")
    assert rows[1] == first_row, "Viewport moved before cursor reached its edge"
    assert sum(row.startswith("| ") for row in rows[1:-1]) == 1, rows
    session.navigate(b"y", b"copied 2 lines to clipboard")
    assert (session.home / "clipboard-content").read_text() == "+line 2\n+line 3"
    session.navigate(b"V", b"CODE new:3")
    session.editor()
    session.send(b"cursor target")
    session.save(["cursor target"])
    anchor = session.comments()[0]["anchor"]
    assert (anchor["start_line"], anchor["end_line"]) == (3, 3), anchor
    session.navigate(b"gg", b"CODE file")
    start = len(session.output)
    session.navigate(b"c", b"select a code line to comment")
    assert b"Shift+Enter newline" not in session.output[start:]
    session.navigate(b"j", b"CODE new:1")
    session.navigate(b"\x1b[6~", b"CODE new:22")
    session.navigate(b"\x1b[5~", b"CODE new:1")
    session.navigate(b"G", b"CODE new:80")
    session.navigate(b"gg", b"CODE file")
    session.navigate(b"j", b"CODE new:1")
    start = len(session.output)
    session.resize(120, 12)
    os.kill(session.pid, signal.SIGWINCH)
    session.wait_for(b"\x1b[12;1H", start)
    session.navigate(b"\x1b[6~", b"CODE new:10")
    session.navigate(b"\x1b[<65;5;3M", b"CODE new:13")
    session.navigate(b"V\x1b[5~", b"SELECT 10 rows  new:4")
    session.editor()
    session.send(b"paged range")
    session.save(["cursor target", "paged range"])
    anchor = session.comments()[1]["anchor"]
    assert (anchor["start_line"], anchor["end_line"]) == (4, 13), anchor
    session.navigate(b"v", b"split/unfold")
    session.navigate(b"G", b"CODE new:80")
    session.send(b"q")
    session.finish(clipboard=True)


def repeated_navigation(session):
    for key, target in [(b"j", 61), (b"k", 1)]:
        start = len(session.output)
        rows = session.navigate(key * 60, f"CODE new:{target} ".encode())
        emitted = bytes(session.output[start:])
        assert len(emitted) < 150_000, (len(emitted), "Key repeat repainted the whole screen")
        assert f"\x1b[1{'S' if key == b'j' else 'T'}".encode() in emitted, "Code scrolling did not reuse terminal rows"
        cursor = [row for row in rows[1:-1] if row.startswith("> ")]
        assert len(cursor) == 1 and f"value_{target:05}" in cursor[0], cursor
        numbers = [int(match.group(1)) for row in rows[1:-1] if (match := re.search(r"value_(\d+)", row))]
        assert numbers == list(range(numbers[0], numbers[0] + len(numbers))), "Scrolled code rows are stale or out of order"
        assert "sample.rs" in rows[0] and f"CODE new:{target}" in rows[-1], "Scrolling moved the chrome"
    session.send(b"q")
    session.finish()


def wheel_bursts(session):
    down, up = b"\x1b[<65;50;15M", b"\x1b[<64;50;15M"
    for sequence, target in [(down, 289), (up, 1)]:
        start = len(session.output)
        rows = session.navigate(sequence * 96, f"CODE new:{target} ".encode())
        emitted = bytes(session.output[start:])
        frames = emitted.count(b"\x1b[?2026h")
        assert frames <= 8, (frames, "Wheel bursts created a frame per event")
        cursor = [row for row in rows[1:-1] if row.startswith("> ")]
        assert len(cursor) == 1 and f"value_{target:05}" in cursor[0], cursor
    session.navigate(down * 4 + b"c", b"Shift+Enter newline")
    body = "wheel anchor\n" + "\n".join(f"note {n:02}" for n in range(1, 31))
    session.send(b"\x1b[200~" + body.encode() + b"\x1b[201~")
    session.save([body])
    assert session.comments()[0]["anchor"]["start_line"] == 13, "Coalescing reordered the comment key"
    rows = session.navigate(b"\r", b"COMMENTS  new:13")
    code = rows[1:15]
    rows = session.navigate(b"\x1b[<65;50;18M" * 16, b"COMMENTS  j/k scroll")
    assert rows[1:15] == code and "note 30" in rows[22], "Comment scrolling moved code or lost its limit"
    rows = session.navigate(b"\x1b[<64;50;18M" * 16, b"COMMENTS  j/k scroll")
    assert rows[1:15] == code and "wheel anchor" in rows[17], "Comment scrolling failed to return to the start"
    session.navigate(b"\x1b", b"CODE new:13 ")
    session.navigate(up * 2, b"CODE new:7 ")
    session.navigate(b"gg", b"CODE file")
    session.navigate(up * 2 + down * 2, b"CODE new:6 ")
    session.send(b"q")
    session.finish()


def workbench(session):
    session.navigate(b"j", b"CODE new:2")
    session.navigate(b"\t", b"FILES  1/2")
    rows = session.navigate(b"j", b"FILES  2/2")
    assert "a.txt" in rows[0], rows
    assert not any("let result" in row for row in rows), "Picker loaded candidate code before confirmation"
    session.navigate(b"\x1b", b"CODE new:2")
    session.navigate(b"\tj", b"FILES  2/2")
    rows = session.navigate(b"\r", b"CODE new:1")
    assert "src/b.rs" in rows[0], rows
    session.navigate(b"j", b"CODE new:2")
    session.navigate(b"K", b"CODE new:2")
    session.navigate(b"G", b"CODE new:80")
    session.navigate(b"J", b"CODE new:2")
    session.navigate(b"v", b"split/unfold")
    rows = session.navigate(b"K", b"CODE new:80")
    assert "stacked/unfold" in rows[0], rows
    start = len(session.output)
    session.resize(140, 24)
    os.kill(session.pid, signal.SIGWINCH)
    session.wait_for(b"\x1b[24;1H", start)
    rows = session.navigate(b"\t", b"FILES  1/2")
    assert rows[1].startswith(" FILES"), rows
    assert all("│" in row[:35] for row in rows[1:-1]), rows
    rows = session.navigate(b"\x1b[<0;2;4M", b"CODE new:2")
    assert "src/b.rs" in rows[0], rows
    session.navigate(b"Vj", b"SELECT 2 rows  new:3")
    session.editor()
    rows = session.screen_lines()
    assert "COMMENT  new:2-3" in rows[15], rows
    assert "src/b.rs" in rows[15], rows
    assert b"\x1b[17;36H" in bytes(session.output).rsplit(b"\x1b[?2026h", 1)[-1], "Editor is not aligned below the code pane"
    session.send(b"review note")
    start = len(session.output)
    session.save(["review note"])
    session.wait_for(b"comment added", start)
    assert not any("review note" in row for row in session.screen_lines()), "Comment expanded inside the diff"
    anchor = session.comments()[0]["anchor"]
    assert (anchor["start_line"], anchor["end_line"]) == (2, 3), anchor
    rows = session.navigate(b"\r", b"COMMENTS  new:3")
    assert "COMMENTS  new:3" in rows[15], rows
    assert any("review note" in row for row in rows[16:-1]), rows
    before = rows[1:15]
    rows = session.navigate(b"j", b"COMMENTS  j/k scroll")
    assert rows[1:15] == before, "Scrolling comments moved code"
    session.navigate(b"K", b"CODE new:80")
    session.send(b"q")
    session.finish()


def mouse_resize(session):
    start = len(session.output)
    session.resize(120, 20)
    os.kill(session.pid, signal.SIGWINCH)
    session.wait_for(b"\x1b[20;1H", start)
    session.send(b"\x1b[<65;5;3M\x1b[<64;5;3M\x1b[<0;5;3M\x1b[<32;5;")
    session.pump(0.08)
    session.send(b"5M\x1b[<0;5;5m")
    session.editor()
    start = len(session.output)
    session.resize(80, 12)
    os.kill(session.pid, signal.SIGWINCH)
    session.wait_for(b"\x1b[12;1H", start)
    session.send(b"selected")
    session.save(["selected"])
    comment = session.comments()[0]
    assert (comment["anchor"]["start_line"], comment["anchor"]["end_line"]) == (1, 3), comment
    session.send(b"q")
    session.finish()


def interrupt(session, sig, editor):
    if editor:
        session.editor()
        # Interrupt while a fragmented sequence is pending, not just while idle.
        session.send(b"unsaved\x1b[13;")
        session.pump(0.05)
    if sig is None:
        session.send(b"\x03")
    else:
        os.kill(session.pid, sig)
    session.finish(0 if sig is None else 128 + sig)
    assert session.comments() == []


def invalid_input(session):
    session.editor()
    session.send(b"\x1b[" + b"1" * 300)
    session.finish(1)
    assert b"Terminal input sequence exceeds size limit" in session.output
    assert session.comments() == []


def main():
    parser = argparse.ArgumentParser(description="Run isolated terminal input smoke tests")
    parser.add_argument("binary", nargs="?", default="target/debug/diffo", type=Path)
    parser.add_argument("--test-binary", type=Path, help="Library test executable for PTY panic/EOF/error probes")
    args = parser.parse_args()
    cases = [
        ("cursor-first", cursor_first, "".join(f"line {n}\n" for n in range(1, 81))),
        ("key-repeat", repeated_navigation, {"sample.rs": "".join(f"    let value_{n:05} = calculate_value(input, options, context);\n" for n in range(1, 601))}),
        ("wheel-bursts", wheel_bursts, {"sample.rs": "".join(f"    let value_{n:05} = calculate_value(input, options, context);\n" for n in range(1, 601))}),
        ("workbench", workbench, {"a.txt": "".join(f"line {n}\n" for n in range(1, 81)), "src/b.rs": "fn work() {\n    let result = 1;\n}\n"}),
        ("shift-enter", parity, SAMPLE), ("editing-paste", editing, SAMPLE),
        ("mouse-resize", mouse_resize, SAMPLE), ("input-error", invalid_input, SAMPLE),
    ]
    for sig in [None, signal.SIGINT, signal.SIGTERM]:
        for editor in [False, True]:
            cases.append((f"interrupt-{sig}-editor-{editor}", lambda session, sig=sig, editor=editor: interrupt(session, sig, editor), SAMPLE))
    with tempfile.TemporaryDirectory(prefix="diffo-terminal-") as directory:
        root = Path(directory)
        for name, test, content in cases:
            session = Session(args.binary.resolve(), root / name, content=content)
            try:
                test(session)
                print(f"PASS: {name}")
            finally:
                session.close()
        if args.test_binary:
            for name in ["tui::tests::terminal_panic_probe", "tui::input::tests::terminal_input_error_probe"]:
                session = Session(args.test_binary.resolve(), root / name.replace(":", "-"),
                                  [name, "--exact", "--ignored", "--nocapture", "--test-threads=1"], frame=False)
                try:
                    session.finish()
                    assert b"1 passed" in session.output
                    print(f"PASS: {name}")
                finally:
                    session.close()
    print("PASS: terminal input parity and cleanup")


if __name__ == "__main__":
    main()
