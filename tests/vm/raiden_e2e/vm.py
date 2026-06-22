"""transient libvirt vm lifecycle, mirroring raid-explorations.

raw member disks plus the live iso boot one transient, uniquely-named domain via
the boot-order trick (blank disks -> firmware falls through to the cdrom and
boots the installer; after install the disks are bootable). multiple runs use
distinct names and image dirs, so they never clash.

the live/install phase is driven with send-key; completion is detected by
observing the domain state (the install command ends in poweroff), not by a
timer. everything post-install is driven over the serial console."""

from __future__ import annotations

import os
import secrets
import shutil
import subprocess
import tomllib
from pathlib import Path

from . import domain, sendkeys
from .config import VMConfig
from .log import log

# raw, sparse images under the libvirt session image pool, like raid-explorations;
# raw is required for the whole-disk truncation scenario.
DEFAULT_IMAGE_BASE = Path.home() / ".local" / "share" / "libvirt" / "images"

# packages the resilience scenarios need on the installed system regardless of
# what the example config requests. sysbench is only for the benchmark scenario,
# so it is dropped on a --skip-benchmark run (see render_config with_benchmark).
BENCHMARK_PACKAGE = "sysbench"
TEST_PACKAGES = [BENCHMARK_PACKAGE]


def sh(*args: str, check: bool = True, capture: bool = False) -> subprocess.CompletedProcess:
    return subprocess.run(list(args), check=check, text=True, capture_output=capture)


def _toml_value(v) -> str:
    if isinstance(v, bool):  # bool is a subclass of int, so test it first
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, str):
        return f'"{v}"'
    if isinstance(v, list):
        return "[" + ", ".join(_toml_value(x) for x in v) + "]"
    raise TypeError(f"unsupported toml value: {v!r}")


def _to_toml(cfg: dict) -> str:
    """emit a shallow config dict (tables of scalars and string lists) as toml.
    sufficient for raiden configs; not a general toml writer."""
    out = []
    for table, body in cfg.items():
        out.append(f"[{table}]")
        out.extend(f"{key} = {_toml_value(val)}" for key, val in body.items())
        out.append("")
    return "\n".join(out).strip() + "\n"


def summarize_config(toml_text: str) -> list[tuple[str, str]]:
    """the reproduction-relevant fields of a rendered config, for the report's
    header table. the full config is the example file in the repo."""
    d = tomllib.loads(toml_text)
    install, raid = d.get("install", {}), d.get("raid", {})
    crypt, boot = d.get("crypt", {}), d.get("boot", {})
    members = d.get("disks", {}).get("members", [])
    rows = [
        ("stack", raid.get("stack", "")),
        ("raid level", raid.get("level", "")),
        ("release", install.get("release", "")),
        ("boot mode", install.get("boot_mode", "")),
        ("/boot", "md raid1" if boot.get("raid") else "independent"),
        ("members", f"{len(members)}: {', '.join(members)}"),
        ("crypt cipher", crypt.get("cipher", "")),
        ("crypt key size", str(crypt.get("key_size", ""))),
        ("crypt sector size", str(crypt.get("sector_size", ""))),
        ("crypt integrity", crypt.get("integrity", "")),
    ]
    if raid.get("stack") == "dm-crypt~btrfs":
        rows.append(("btrfs csum", d.get("btrfs", {}).get("csum", "")))
    if raid.get("stack") == "dm-integrity~md~dm-crypt~lvm~ext4":
        rows.append(("integrity algorithm", d.get("integrity", {}).get("algorithm", "")))
    return rows


def render_config(
    example_path: str, members, level: str = "", boot_raid: bool = False, with_benchmark: bool = True
) -> str:
    """load an example config and overlay only the test-specific keys: serial
    console on (the harness drives over serial), the vm's member disks, the raid
    level (only when explicitly chosen), the /boot mode, and the scenario
    packages. stack, cipher, integrity, and filesystem options are unchanged.
    with_benchmark=False drops sysbench (only the benchmark scenario needs it), so
    a --skip-benchmark run does not install it."""
    cfg = tomllib.loads(Path(example_path).read_text())
    install = cfg.setdefault("install", {})
    install["serial_console"] = True
    test_pkgs = TEST_PACKAGES if with_benchmark else [p for p in TEST_PACKAGES if p != BENCHMARK_PACKAGE]
    pkgs = list(install.get("extra_packages", []))
    install["extra_packages"] = pkgs + [p for p in test_pkgs if p not in pkgs]
    cfg.setdefault("disks", {})["members"] = list(members)
    if level:
        cfg.setdefault("raid", {})["level"] = level
    cfg.setdefault("boot", {})["raid"] = boot_raid
    return _to_toml(cfg)


class VM:
    def __init__(self, cfg: VMConfig):
        if not cfg.token:
            cfg.token = secrets.token_hex(4)
        self.cfg = cfg
        base = Path(cfg.image_dir) if cfg.image_dir else DEFAULT_IMAGE_BASE
        self.workdir = base / cfg.name
        self.workdir.mkdir(parents=True, exist_ok=True)
        self.payload = self.workdir / "payload"
        self.payload.mkdir(exist_ok=True)
        self.nvram = str(self.workdir / "nvram.fd")
        self.console_log = self.workdir / "console.log"
        self.disks = [(dev, str(self.workdir / f"{dev}.raw")) for dev in cfg.disk_names()]
        # the exact config we install, rendered once; the report shows a summary.
        self.config_text = self._render_config()
        self.config_rows = summarize_config(self.config_text)

    # -- setup ----------------------------------------------------------------

    def prepare(self, fresh: bool = True) -> None:
        if fresh:
            size = self.cfg.disk_size_gb * 1024 * 1024 * 1024
            log(f"creating {len(self.disks)} raw disk(s) of {self.cfg.disk_size_gb}G in {self.workdir}")
            for _dev, path in self.disks:
                with open(path, "wb") as f:  # sparse raw image
                    f.truncate(size)
            if self.cfg.resolved_boot_mode() != "bios":  # efi needs per-vm nvram; bios (seabios) does not
                shutil.copyfile(self.cfg.ovmf_vars, self.nvram)
        else:
            for _dev, path in self.disks:
                if not os.path.exists(path):
                    raise RuntimeError(
                        f"--skip-install but disk {path} is missing; "
                        f"run an install first with --keep --name {self.cfg.token}"
                    )
            log(f"reusing existing disks in {self.workdir}")
        self._stage_payload()

    def _stage_payload(self) -> None:
        """assemble the virtiofs share: the raiden binary, the livecd driver the
        live phase runs raiden through, a generated config, the password, and the
        guest test scripts."""
        shutil.copyfile(self.cfg.binary, self.payload / "raiden")
        (self.payload / "raiden").chmod(0o755)
        livecd = Path(__file__).resolve().parents[3] / "livecd.sh"
        shutil.copyfile(livecd, self.payload / "livecd.sh")
        (self.payload / "livecd.sh").chmod(0o755)
        (self.payload / "raiden.toml").write_text(self.config_text)
        (self.payload / "password").write_text(self.cfg.password)
        guest_src = Path(__file__).resolve().parent.parent / "guest"
        guest_dst = self.payload / "guest"
        if guest_dst.exists():
            shutil.rmtree(guest_dst)
        shutil.copytree(guest_src, guest_dst)

    def _render_config(self) -> str:
        from . import scenarios as sc

        # the benchmark runs only if selected (or in the default bundle when no
        # subset is chosen); skip its package otherwise.
        with_benchmark = (not self.cfg.scenarios) or sc.BENCHMARK in self.cfg.scenarios
        return render_config(
            self.cfg.resolved_config_file(),
            self.cfg.disk_names(),
            level=self.cfg.level,
            boot_raid=self.cfg.boot_raid,
            with_benchmark=with_benchmark,
        )

    # -- lifecycle ------------------------------------------------------------

    def start(self, cdrom_first: bool = False) -> None:
        xml = domain.build_xml(
            self.cfg,
            disks=self.disks,
            nvram_path=self.nvram,
            payload_dir=str(self.payload),
            cdrom_first=cdrom_first,
        )
        xml_path = self.workdir / "domain.xml"
        xml_path.write_text(xml)
        log(f"creating transient domain {self.cfg.name} (cdrom_first={cdrom_first})")
        sh("virsh", "create", str(xml_path))
        log(f"  watch the graphical console with: virt-viewer {self.cfg.name}")

    def destroy(self) -> None:
        log(f"destroying domain {self.cfg.name}")
        sh("virsh", "destroy", self.cfg.name, check=False)

    def domstate(self) -> str:
        r = sh("virsh", "domstate", self.cfg.name, check=False, capture=True)
        return (r.stdout or "").strip()

    def send_line(self, line: str) -> None:
        sendkeys.send_line(self.cfg.name, line)

    def cleanup(self) -> None:
        self.destroy()
        if not self.cfg.keep:
            shutil.rmtree(self.workdir, ignore_errors=True)
