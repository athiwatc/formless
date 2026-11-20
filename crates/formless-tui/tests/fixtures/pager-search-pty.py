"""Exercise the isolated pager over a Linux PTY.

Pass the Rust test executable from cargo test --locked -p formless-tui --lib --no-run.
"""

import errno
import fcntl
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time

ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")


def winsize(fd, rows, columns):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))


def check(executable):
    master, slave = pty.openpty()
    child = None
    pending = bytearray()

    def receive(until, phase):
        deadline = time.monotonic() + 5
        while until not in pending:
            if not select.select([master], [], [], max(0, deadline - time.monotonic()))[0]:
                raise AssertionError(f"{phase} timed out: {pending!r}")
            try:
                data = os.read(master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                data = b""
            if not data:
                raise AssertionError(f"{phase} ended early: {pending!r}")
            pending.extend(data)
        end = pending.index(until) + len(until)
        frame = ANSI.sub(b"", pending[:end]).decode()
        del pending[:end]
        return frame

    def frame(phase, *expected):
        screen = receive(f"\r\nPAGER-FRAME-{phase}\r\n".encode(), phase)
        # Clear leaves blank cells, so Crossterm can skip over them with
        # cursor moves. Compare the emitted nonblank text in each frame.
        printed = screen.replace(" ", "")
        for part in expected:
            assert part.replace(" ", "") in printed, (
                f"phase {phase}: missing {part!r} in {screen!r}"
            )
        print(f"phase {phase}: {', '.join(expected)}")

    try:
        winsize(slave, 8, 80)
        child = subprocess.Popen(
            [
                executable,
                "tests::pager_search_owned_pty_fixture",
                "--exact",
                "--ignored",
                "--nocapture",
            ],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env={**os.environ, "TERM": "xterm-256color", "FORMLESS_TEST_PAGER_PTY": "1"},
            start_new_session=True,
        )
        os.close(slave)
        slave = None
        frame(0, "/ find")
        os.write(master, b"/")
        frame(1, "Enter find")
        os.write(master, b"\x1b[200~" + "ไทย".encode() + b"\x1b[201~")
        frame(2, "ไทย")
        os.write(master, b"\r")
        frame(3, "match 1/2", "ไทย first", "Enter/Shift+Enter")
        os.write(master, b"\r")
        frame(4, "match 2/2", "ไทย second")
        os.write(master, b"\x1b[13;2u")
        frame(5, "match 1/2", "ไทย first")
        winsize(master, 6, 24)
        child.send_signal(signal.SIGWINCH)
        frame(6, "match 1/2", "ไทย first", "n/N")
        os.write(master, b"/")
        frame(7, "Enter find")
        os.write(master, b"\x1b[27u")
        frame(8, "match 1/2", "ไทย first")
        os.write(master, b"\x1b[27u")
        frame(9)
        receive(b"test result: ok. 1 passed", "test result")
        assert child.wait(timeout=5) == 0
    finally:
        if child is not None and child.poll() is None:
            child.kill()
            child.wait()
        os.close(master)
        if slave is not None:
            os.close(slave)


if __name__ == "__main__":
    check(sys.argv[1])
