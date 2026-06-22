"""tiny timestamped logger, a tee, and a control-character stripper.

the live/install phase has no serial console, so without these messages the run
looks frozen. post-install, the serial console output is stripped of terminal
control sequences and tee'd to stdout and the log file at once."""

from __future__ import annotations

import datetime
import re
import sys

# terminal control sequences to drop from the serial stream so the logged output
# stays readable: csi/osc/other escape sequences, plus stray control chars (tab
# and newline are kept). carriage returns are covered by \x0b-\x1f.
_CTRL_RE = re.compile(
    r"\x1b\[[0-?]*[ -/]*[@-~]"  # csi (colors, cursor moves)
    r"|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"  # osc (eg. window title)
    r"|\x1b[@-Z\\-_]"  # other two-char escapes
    r"|[\x00-\x08\x0b-\x1f\x7f]"  # remaining control chars
)


def strip_control(s: str) -> str:
    return _CTRL_RE.sub("", s)


def log(msg: str) -> None:
    ts = datetime.datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] {msg}", flush=True)


class Tee:
    """fan writes out to several streams (eg. stdout + a log file)."""

    def __init__(self, *streams):
        self.streams = streams

    def write(self, data) -> int:
        for s in self.streams:
            s.write(data)
            s.flush()
        return len(data)

    def flush(self) -> None:
        for s in self.streams:
            s.flush()


class CleanStream:
    """strip terminal control sequences from a text stream before forwarding.

    line-buffered so an escape sequence split across two writes is not mangled:
    complete lines are stripped and emitted, the trailing partial line is held
    until the next newline (or close)."""

    def __init__(self, target):
        self.target = target
        self._buf = ""

    def write(self, data) -> int:
        self._buf += data
        *lines, self._buf = self._buf.split("\n")
        for line in lines:
            self.target.write(strip_control(line) + "\n")
        self.target.flush()
        return len(data)

    def flush(self) -> None:
        self.target.flush()

    def close(self) -> None:
        if self._buf:
            self.target.write(strip_control(self._buf))
            self._buf = ""
        self.target.flush()
