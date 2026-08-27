#!/usr/bin/env python3
"""End-to-end PTY and signal verification for the built shue binary."""

from __future__ import annotations

import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "target" / "debug" / "shue"
TIMEOUT_SECONDS = 8.0


def fail(message: str) -> "NoReturn":
    raise AssertionError(message)


def wait_status(pid: int, master: int, output: bytearray) -> int:
    deadline = time.monotonic() + TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        readable, _, _ = select.select([master], [], [], 0.02)
        if readable:
            try:
                chunk = os.read(master, 65536)
                if chunk:
                    output.extend(chunk)
            except OSError as error:
                if error.errno not in (errno.EIO, errno.EBADF):
                    raise
        waited, status = os.waitpid(pid, os.WNOHANG)
        if waited == pid:
            if os.WIFEXITED(status):
                return os.WEXITSTATUS(status)
            if os.WIFSIGNALED(status):
                return 128 + os.WTERMSIG(status)
            fail(f"unexpected wait status {status}")
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    fail(f"shue did not exit within {TIMEOUT_SECONDS}s; output={bytes(output)!r}")


def read_until(master: int, output: bytearray, marker: bytes) -> None:
    deadline = time.monotonic() + TIMEOUT_SECONDS
    while marker not in output and time.monotonic() < deadline:
        readable, _, _ = select.select([master], [], [], 0.05)
        if not readable:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError as error:
            if error.errno == errno.EIO:
                break
            raise
        if not chunk:
            break
        output.extend(chunk)
    if marker not in output:
        fail(f"missing PTY marker {marker!r}; output={bytes(output)!r}")


def relevant_terminal_flags(attributes: list) -> tuple[int, int, int]:
    input_mask = termios.ICRNL | termios.IXON
    output_mask = termios.OPOST
    local_mask = termios.ECHO | termios.ICANON | termios.IEXTEN | termios.ISIG
    return (
        attributes[0] & input_mask,
        attributes[1] & output_mask,
        attributes[3] & local_mask,
    )


def spawn_interactive(config: Path, shell_source: str) -> tuple[int, int, tuple[int, int, int]]:
    master, slave = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 31, 101, 0, 0))
    original = relevant_terminal_flags(termios.tcgetattr(master))
    pid = os.fork()
    if pid == 0:
        try:
            os.setsid()
            if hasattr(termios, "TIOCSCTTY"):
                fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            for descriptor in (0, 1, 2):
                os.dup2(slave, descriptor)
            os.close(master)
            if slave > 2:
                os.close(slave)
            environment = os.environ.copy()
            environment.pop("NO_COLOR", None)
            environment["TERM"] = "xterm-256color"
            arguments = [
                str(BINARY),
                "--no-color",
                "--config",
                str(config),
                "--exec",
                "/bin/sh",
                "-c",
                shell_source,
            ]
            os.execve(BINARY, arguments, environment)
        except BaseException as error:  # pragma: no cover - child diagnostic
            os.write(2, f"PTY child setup failed: {error}\n".encode())
            os._exit(127)
    os.close(slave)
    return pid, master, original


def assert_terminal_restored(master: int, original: tuple[int, int, int]) -> None:
    restored = relevant_terminal_flags(termios.tcgetattr(master))
    if restored != original:
        fail(f"terminal flags were not restored: before={original!r}, after={restored!r}")


def verify_normal_session(config: Path) -> None:
    source = (
        "dims=$(/bin/stty size); printf 'PTY_READY:%s;' \"$dims\"; "
        "IFS= read -r line; printf '\\nPTY_ECHO:%s\\n' \"$line\"; exit 23"
    )
    pid, master, original = spawn_interactive(config, source)
    output = bytearray()
    try:
        read_until(master, output, b"PTY_READY:31 101;")
        os.write(master, b"hello-through-pty\r")
        read_until(master, output, b"PTY_ECHO:hello-through-pty")
        code = wait_status(pid, master, output)
        if code != 23:
            fail(f"interactive child exit did not propagate: {code}; output={bytes(output)!r}")
        assert_terminal_restored(master, original)
    finally:
        os.close(master)


def verify_signal_session(config: Path) -> None:
    source = "printf 'SIGNAL_READY;'; while :; do /bin/sleep 1; done"
    pid, master, original = spawn_interactive(config, source)
    output = bytearray()
    try:
        read_until(master, output, b"SIGNAL_READY;")
        os.kill(pid, signal.SIGTERM)
        code = wait_status(pid, master, output)
        if code != 128 + signal.SIGTERM:
            fail(f"SIGTERM exit was {code}, expected {128 + signal.SIGTERM}")
        assert_terminal_restored(master, original)
    finally:
        os.close(master)


def main() -> int:
    subprocess.run(["cargo", "build", "-p", "shue"], cwd=ROOT, check=True)
    with tempfile.TemporaryDirectory(prefix="shue-interactive-") as temporary:
        config = Path(temporary) / "empty.yaml"
        config.write_text("palette: {}\nrules: []\n", encoding="utf-8")
        verify_normal_session(config)
        verify_signal_session(config)
    print("interactive PTY verification passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())

