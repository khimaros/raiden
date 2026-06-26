"""shared fixtures: build the binary once and expose a runner bound to the repo.

these tests exercise raiden's planning, validation, and config handling via
--dry-run, so they run anywhere without root or real disks. a full install is
validated separately in a libvirt/qemu vm (see vm/README.md)."""

import os
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
BINARY = REPO / "target" / "debug" / "raiden"


@pytest.fixture(scope="session")
def raiden():
    subprocess.run(["cargo", "build"], cwd=REPO, check=True)

    def run(*args, expect_ok=True, env=None):
        result = subprocess.run(
            [str(BINARY), *args],
            cwd=REPO,
            capture_output=True,
            text=True,
            env={**os.environ, **env} if env else None,
        )
        if expect_ok:
            assert result.returncode == 0, f"raiden {args} failed:\n{result.stderr}"
        return result

    return run
