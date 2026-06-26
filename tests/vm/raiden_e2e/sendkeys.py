"""drive the graphical console with `virsh send-key`, for the live/install phase
only (the stock live iso has no serial console). a char maps to one chord of
linux keycodes; everything post-install uses the serial console instead.

ported from raid-explorations' string_to_keycodes.py."""

from __future__ import annotations

import subprocess
import time

# `virsh send-key` is fire-and-forget: without a holdtime the chord's keys can be
# released faster than the guest registers them, so a modifier (eg. LEFTSHIFT)
# races its key and characters drop or garble. hold each chord, then pace between
# chords so the guest input pipeline keeps up. tuned for reliability over speed --
# the live phase types one short (~250 char) command, not the config.
HOLDTIME_MS = 50
KEY_INTERVAL_S = 0.05


def _table() -> dict[str, list[str]]:
    tt = {
        "\n": ["KEY_ENTER"],
        ".": ["KEY_DOT"],
        ">": ["KEY_LEFTSHIFT", "KEY_DOT"],
        "&": ["KEY_LEFTSHIFT", "KEY_7"],
        "/": ["KEY_SLASH"],
        " ": ["KEY_SPACE"],
        "-": ["KEY_MINUS"],
        "_": ["KEY_LEFTSHIFT", "KEY_MINUS"],
        "|": ["KEY_LEFTSHIFT", "KEY_BACKSLASH"],
        "@": ["KEY_LEFTSHIFT", "KEY_2"],
        "?": ["KEY_LEFTSHIFT", "KEY_SLASH"],
        "'": ["KEY_APOSTROPHE"],
        '"': ["KEY_LEFTSHIFT", "KEY_APOSTROPHE"],
        "=": ["KEY_EQUAL"],
        "~": ["KEY_LEFTSHIFT", "KEY_GRAVE"],
        "^": ["KEY_LEFTSHIFT", "KEY_6"],
        "*": ["KEY_LEFTSHIFT", "KEY_8"],
        ";": ["KEY_SEMICOLON"],
        ":": ["KEY_LEFTSHIFT", "KEY_SEMICOLON"],
        ",": ["KEY_COMMA"],
        "+": ["KEY_LEFTSHIFT", "KEY_EQUAL"],
        "$": ["KEY_LEFTSHIFT", "KEY_4"],
        "#": ["KEY_LEFTSHIFT", "KEY_3"],
        "%": ["KEY_LEFTSHIFT", "KEY_5"],
        "!": ["KEY_LEFTSHIFT", "KEY_1"],
        "(": ["KEY_LEFTSHIFT", "KEY_9"],
        ")": ["KEY_LEFTSHIFT", "KEY_0"],
        "<": ["KEY_LEFTSHIFT", "KEY_COMMA"],
        "[": ["KEY_LEFTBRACE"],
        "]": ["KEY_RIGHTBRACE"],
        "{": ["KEY_LEFTSHIFT", "KEY_LEFTBRACE"],
        "}": ["KEY_LEFTSHIFT", "KEY_RIGHTBRACE"],
        "\\": ["KEY_BACKSLASH"],
        "`": ["KEY_GRAVE"],
    }
    for c in "0123456789":
        tt[c] = [f"KEY_{c}"]
    for c in "abcdefghijklmnopqrstuvwxyz":
        tt[c] = [f"KEY_{c.upper()}"]
    for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ":
        tt[c] = ["KEY_LEFTSHIFT", f"KEY_{c}"]
    return tt


TABLE = _table()


def press(vm_name: str, *codes: str) -> None:
    """send one chord of raw linux keycodes (eg. KEY_END, or KEY_LEFTCTRL KEY_X),
    held for HOLDTIME_MS so the guest registers the whole chord before release."""
    subprocess.run(
        ["virsh", "send-key", vm_name, "--holdtime", str(HOLDTIME_MS), *codes],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def send_text(vm_name: str, text: str) -> None:
    for ch in text:
        codes = TABLE.get(ch)
        if not codes:
            raise KeyError(f"no keycode mapping for character {ch!r}")
        press(vm_name, *codes)
        time.sleep(KEY_INTERVAL_S)


def send_line(vm_name: str, line: str) -> None:
    send_text(vm_name, line + "\n")
