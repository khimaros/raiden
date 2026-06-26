"""run configuration for a single vm test run."""

from __future__ import annotations

import string
import tomllib
from dataclasses import dataclass, field
from pathlib import Path


def _config_level(path: str) -> str:
    """the raid level declared in a config file, or "" if absent/unreadable."""
    try:
        return tomllib.loads(Path(path).read_text()).get("raid", {}).get("level", "")
    except (OSError, tomllib.TOMLDecodeError):
        return ""


def _config_boot_mode(path: str) -> str:
    """the boot mode (efi/bios) declared in a config file; defaults to efi."""
    try:
        mode = tomllib.loads(Path(path).read_text()).get("install", {}).get("boot_mode", "")
    except (OSError, tomllib.TOMLDecodeError):
        mode = ""
    return mode or "efi"

# the repo's example config catalog (raiden_e2e is tests/vm/raiden_e2e).
EXAMPLES_DIR = Path(__file__).resolve().parents[3] / "examples"

# a valid raid level per stack, for runs that do not override it.
DEFAULT_LEVEL = {
    "dm-crypt~md~lvm~ext4": "6",
    "dm-crypt~md~lvm~xfs": "6",
    "dm-crypt~btrfs": "raid1c3",
    "dm-crypt~bcachefs": "3",
    "dm-crypt~zfs": "raidz2",
    "dm-integrity~md~dm-crypt~lvm~ext4": "6",
}

# the example config installed for each stack (under the repo's examples/ dir).
# names follow raid-explorations' <stack>.<level>[.<variant>] convention. the
# harness loads the file and overlays only the test-specific keys; everything
# else -- cipher, integrity, filesystem options -- comes from the example.
EXAMPLE_CONFIG = {
    "dm-crypt~md~lvm~ext4": "dm-crypt~md~lvm~ext4.raid6.aead.toml",
    "dm-crypt~md~lvm~xfs": "dm-crypt~md~lvm~xfs.raid6.aead.toml",
    "dm-crypt~btrfs": "dm-crypt~btrfs.raid1c3.toml",
    "dm-crypt~bcachefs": "dm-crypt~bcachefs.replicas3.toml",
    "dm-crypt~zfs": "dm-crypt~zfs.raidz2.toml",
    "dm-integrity~md~dm-crypt~lvm~ext4": "dm-integrity~md~dm-crypt~lvm~ext4.raid6.toml",
}

# shell command that recovers a boot which drops to the initramfs rescue shell.
# `raiden recover` (baked into the initrd by install.initramfs_recovery) brings a
# degraded root online for any stack, generalizing the old per-stack `mount -o
# degraded` commands; it is a no-op when the root is already mounted, so it is
# harmless for stacks (md/zfs) that assemble degraded on their own. invoked by
# absolute path -- the hook copies the binary to /sbin/raiden in the initrd.
INITRAMFS_RECOVERY = ["/sbin/raiden recover --yes"]


@dataclass
class VMConfig:
    # what to install
    stack: str = "dm-crypt~md~lvm~ext4"
    level: str = ""  # empty -> DEFAULT_LEVEL[stack]
    boot_raid: bool = False  # default: independent /boot; true -> md raid1 /boot
    # which post-install scenarios to run; empty runs them all. see
    # scenarios.scenario_names() for the selectable names.
    scenarios: tuple[str, ...] = ()
    disks: int = 4
    disk_size_gb: int = 5

    # vm resources
    memory_mb: int = 4096
    vcpus: int = 4

    # host inputs
    iso: str = ""  # path to a debian live iso
    binary: str = ""  # path to the built raiden binary
    config_file: str = ""  # raiden config to install from; empty -> examples/<stack>
    image_dir: str = ""  # dir for per-run disk images (temp dir if empty)
    ovmf_code: str = "/usr/share/OVMF/OVMF_CODE_4M.fd"
    ovmf_vars: str = "/usr/share/OVMF/OVMF_VARS_4M.fd"

    # behavior
    password: str = "test"  # disk encryption password
    root_password: str = "test"  # root account on the installed system
    live_user: str = "user"  # default unprivileged user on the debian live iso
    live_password: str = "live"  # its password (also used for sudo)
    keep: bool = False  # leave the vm and disks in place after the run
    skip_install: bool = False  # reuse existing disks; jump to the scenarios
    install_only: bool = False  # stop after a successful install (skip scenarios)
    interactive: bool = False  # pause for the operator on unexpected console state
    name_prefix: str = "raiden-e2e"
    token: str = ""  # unique suffix; set via --name for reproducible/reusable runs

    # the stock live iso has no serial console, so the short live/install phase is
    # driven by send-key after coarse waits (the grub prompt comes up fast, like
    # raid-explorations). its output is streamed to the host via the shared log.
    # everything post-install is serial-driven and observes console state.
    grub_seconds: int = 12  # wait for the live grub menu, then press enter
    live_boot_seconds: int = 25  # then wait for the live shell before typing
    poll_seconds: int = 5  # interval for observing domain state / tailing the log
    install_timeout: int = 3600  # max wait for the install (ends in poweroff)
    rescue_timeout: int = 2400

    def resolved_level(self) -> str:
        # an explicit --level wins; else the level the install will actually use
        # (from the config file, eg. the raid10 example); else the stack default.
        return (
            self.level
            or _config_level(self.resolved_config_file())
            or DEFAULT_LEVEL.get(self.stack, "6")
        )

    def resolved_config_file(self) -> str:
        # an explicit --config wins; otherwise install from the stack's example.
        return self.config_file or str(EXAMPLES_DIR / EXAMPLE_CONFIG[self.stack])

    def resolved_boot_mode(self) -> str:
        # the firmware the vm must present (bios -> seabios, efi -> ovmf) matches
        # what the install lays down, read from the config file.
        return _config_boot_mode(self.resolved_config_file())

    def disk_names(self) -> list[str]:
        # vda, vdb, ... one per configured disk.
        return [f"vd{string.ascii_lowercase[i]}" for i in range(self.disks)]

    @property
    def name(self) -> str:
        return f"{self.name_prefix}-{self.token}"
