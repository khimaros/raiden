"""operator supervision for unexpected console state.

unattended runs use no supervisor: a stuck wait raises and the scenario is
graded as failed, then the run moves on. interactive runs use Operator, which
shows what was expected and the recent console output, then blocks until the
operator decides to retry, skip, or abort -- so the human chooses when to
resume. the operator can watch and intervene through the graphical console
(virt-viewer) while deciding; the serial channel stays owned by the harness."""

from __future__ import annotations

from .console import ABORT, RETRY, SKIP


class Operator:
    def on_stuck(self, console, patterns, exc) -> str:
        print("\n" + "=" * 70)
        print(f"UNEXPECTED STATE on vm {console.vm_name}")
        print(f"  waited for: {patterns!r}")
        print(f"  reason:     {type(exc).__name__}")
        print("  recent console output:")
        for line in console.recent().splitlines()[-20:]:
            print(f"    | {line}")
        print("-" * 70)
        print(f"  watch/fix via the graphical console:  virt-viewer {console.vm_name}")
        print("  (the serial channel is held by the harness; use the gui to intervene)")
        print("=" * 70)
        while True:
            choice = input("[r]etry / [s]kip this scenario / [a]bort run > ").strip().lower()
            if choice in ("r", "retry"):
                return RETRY
            if choice in ("s", "skip"):
                return SKIP
            if choice in ("a", "abort"):
                return ABORT
