#!/usr/bin/env python3
"""Run PTY input and cleanup regression tests with isolated Git/HOME/state."""
import argparse
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import subprocess
import tempfile
import termios
import time


class Session:
    def __init__(self, binary, root, argv=(), frame=True):
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
            path.write_text('#!/bin/sh\nprintf "called\\n" >> "$HOME/clipboard-called"\ncat >/dev/null\n')
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
        (self.repo / "sample.txt").write_text("first\nsecond\nthird\n")
        self.master, self.slave = pty.openpty()
        self.original = termios.tcgetattr(self.slave)
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

    def comments(self):
        files = list(self.state.glob("diffo/repos/*/comments.json"))
        return json.loads(files[0].read_text())["comments"] if files else []

    def editor(self):
        start = len(self.output)
        self.send(b"c")
        self.wait_for(b"Shift+Enter newline", start)

    def save(self, expected):
        self.send(b"\r")
        deadline = time.monotonic() + 5
        while [comment["body"] for comment in self.comments()] != expected:
            assert time.monotonic() < deadline, self.comments()
            self.pump(0.02)

    def finish(self, code=0):
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
        assert b"\x1b]52;" not in self.output, "Unexpected clipboard operation"
        assert not (self.home / "clipboard-called").exists(), "Unexpected clipboard command"
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
            self.process.wait()
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
    cases = [("shift-enter", parity), ("editing-paste", editing), ("mouse-resize", mouse_resize), ("input-error", invalid_input)]
    for sig in [None, signal.SIGINT, signal.SIGTERM]:
        for editor in [False, True]:
            cases.append((f"interrupt-{sig}-editor-{editor}", lambda session, sig=sig, editor=editor: interrupt(session, sig, editor)))
    with tempfile.TemporaryDirectory(prefix="diffo-terminal-") as directory:
        root = Path(directory)
        for name, test in cases:
            session = Session(args.binary.resolve(), root / name)
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
