"""resilience scenarios run against the installed system over the serial console.

each scenario is self-contained: it induces a fault, observes the result, grades
it, and (where possible) recovers the array to a clean state before returning, so
scenarios can run in sequence. the sequencing here is intentionally simple --
induce, detect, survive, recover, confirm -- rather than following the old
harness's order."""

from __future__ import annotations

import os

from .log import log
from .report import FAIL, INFO, PASS, WARN

WORKDIR = "/srv/raiden"
RAIDEN = f"{WORKDIR}/raiden"
PWFILE = f"{WORKDIR}/password"
GUEST = f"{WORKDIR}/guest"

DETECT_KEYWORDS = (
    "error", "corrupt", "cksum", "mismatch", "read error", "faulty", "degraded",
    "removed", "failed", "unrecoverable",
)


def _tail(text: str, n: int = 8) -> str:
    return " / ".join(line.strip() for line in text.strip().splitlines()[-n:] if line.strip())


def _detected(text: str) -> bool:
    low = text.lower()
    return any(k in low for k in DETECT_KEYWORDS)


class Session:
    def __init__(self, vm, console, cfg, report):
        self.vm = vm
        self.c = console
        self.cfg = cfg
        self.report = report
        self.reboot_used_initramfs = False  # set by reboot_login per boot

    # -- guest helpers --------------------------------------------------------

    def mount_payload(self) -> None:
        self.c.run(f"mkdir -p {WORKDIR}", check=True)
        self.c.run(f"mountpoint -q {WORKDIR} || mount -t virtiofs payload {WORKDIR}", check=True)

    def raiden(self, args: str, timeout: int = 2400) -> tuple[int, str]:
        log(f"raiden {args}")
        # --verbose by default in the harness: echo exact commands + full output.
        return self.c.run(f"{RAIDEN} --verbose {args}", timeout=timeout)

    def status(self) -> str:
        return self.raiden("status")[1]

    def survival(self) -> int:
        log("survival read (full filesystem md5sum)")
        return self.c.run(f"bash {GUEST}/survival.sh", timeout=5400)[0]

    def scrub(self) -> None:
        log("scrubbing the array (may take a while)")
        rc, out = self.raiden("scrub --yes", timeout=10800)
        if rc != 0:
            raise RuntimeError(f"scrub failed (rc={rc}): {_tail(out)}")

    def replace(self, disks: list[str], parts: str = "") -> None:
        log(f"replacing disks: {', '.join(disks)} {parts}".rstrip())
        args = f"replace --yes --password-file {PWFILE} --disks {','.join(disks)} {parts}".rstrip()
        rc, out = self.raiden(args, timeout=10800)
        if rc != 0:
            raise RuntimeError(f"replace failed (rc={rc}): {_tail(out)}")

    def corrupt_random(self, parts: list[str], nbytes: int) -> None:
        log(f"writing {nbytes} random bytes to: {', '.join(parts)}")
        for p in parts:
            self.c.run(f"python3 {GUEST}/random_write.py /dev/{p} {nbytes}", timeout=900)

    def corrupt_dd(self, parts: list[str], count: int) -> None:
        log(f"overwriting headers of: {', '.join(parts)}")
        for p in parts:
            self.c.run(f"dd if=/dev/urandom of=/dev/{p} count={count}", timeout=300)

    def reboot_login(self) -> None:
        log("rebooting; waiting for the serial unlock prompt, then login")
        before = self.c.initramfs_recoveries
        self.c.reboot()
        self.c.unlock(self.cfg.password)
        self.c.login("root", self.cfg.root_password)
        # record whether this boot needed the manual mount-o-degraded follow-through.
        self.reboot_used_initramfs = self.c.initramfs_recoveries > before
        self.mount_payload()

    def grade_reboot(self, scenario: str, ok_detail: str) -> None:
        """reboot, then grade reaching login. a boot that only came up because the
        harness intervened at the initramfs (running the stack's recovery commands,
        eg. a manual 'mount -o degraded') is a WARN, not a clean PASS -- it did not
        boot unattended. a boot that reaches login on its own is a clean PASS."""
        self.reboot_login()
        if self.reboot_used_initramfs:
            self.report.add(
                scenario,
                "reboot",
                WARN,
                f"{ok_detail}, but only after manual intervention at the initramfs "
                "(the boot did not come up unattended; recovery commands were run)",
            )
        else:
            self.report.add(scenario, "reboot", PASS, ok_detail)

    def members(self) -> list[str]:
        return self.cfg.disk_names()

    def grade_survival(self, scenario: str, within_params: bool) -> None:
        rc = self.survival()
        if rc == 0:
            self.report.add(scenario, "survive", PASS, "no read errors")
        else:
            self.report.add(scenario, "survive", FAIL if within_params else WARN, f"rc={rc}")


# -- scenarios ----------------------------------------------------------------


def sysbench(s: Session) -> None:
    # the fsync-bound fileio workload now lives in the binary (`raiden benchmark`),
    # so the harness no longer ships a script; the summary table is tailed in.
    rc, out = s.c.run(f"{RAIDEN} benchmark", timeout=7200)
    s.report.add("sysbench", "run", INFO if rc == 0 else WARN, _tail(out, 12))


def corrupt_data_within(s: Session) -> None:
    """random bitrot on two of the data members, within raid redundancy."""
    scen = "corrupt 2/4 data"
    parts = [f"{d}3" for d in s.members()[1:3]]
    s.corrupt_random(parts, 50000)
    st = s.status()
    s.report.add(scen, "detect", PASS if _detected(st) else WARN, _tail(st))
    s.grade_survival(scen, within_params=True)
    s.scrub()
    s.grade_survival(scen, within_params=True)
    s.report.add(scen, "clean", PASS, "scrub completed")
    s.grade_reboot(scen, "reached login")


def corrupt_headers(s: Session) -> None:
    """overwrite the start of two members, then recover by replacing them."""
    scen = "corrupt 2/4 headers"
    targets = s.members()[1:3]
    s.corrupt_dd([f"{d}3" for d in targets], 10000)
    s.grade_survival(scen, within_params=True)
    s.grade_reboot(scen, "degraded array still booted")
    s.replace(targets)
    s.scrub()
    s.grade_survival(scen, within_params=True)
    s.report.add(scen, "clean", PASS, "replaced and scrubbed")
    s.reboot_login()


def truncate_disks(s: Session) -> None:
    """simulate whole-disk loss by truncating two images on the host."""
    scen = "truncate 2/4 disk"
    targets = s.members()[1:3]
    target_paths = [path for dev, path in s.vm.disks if dev in targets]
    for path in target_paths:
        size = os.path.getsize(path)
        os.truncate(path, 0)
        os.truncate(path, size)
    st = s.status()
    s.report.add(scen, "detect", PASS if _detected(st) else WARN, _tail(st))
    s.grade_survival(scen, within_params=True)
    s.grade_reboot(scen, "degraded array still booted")
    s.replace(targets)
    s.scrub()
    s.report.add(scen, "clean", PASS, "replaced and scrubbed")
    s.reboot_login()


def corrupt_efiboot(s: Session) -> None:
    """destroy the first disk's esp and /boot member; firmware should boot a
    surviving mirror, then replace restores the disk."""
    scen = "corrupt boot partition"
    first = s.members()[0]
    s.corrupt_dd([f"{first}1", f"{first}2"], 10000)
    st = s.status()
    s.report.add(scen, "detect", PASS if _detected(st) else WARN, _tail(st))
    s.grade_reboot(scen, "booted from a surviving esp")
    s.replace([first])
    s.scrub()
    s.report.add(scen, "clean", PASS, "esp and array restored")
    s.reboot_login()


def replace_primary(s: Session) -> None:
    """replace the PRIMARY disk on a healthy system. its esp is mounted at
    /boot/efi, so replace must unmount it before mkfs -- without that fix,
    mkfs.msdos fails with 'contains a mounted filesystem'. the other scenarios
    only replace non-primary disks (whose esp mirrors are unmounted under option
    a), so this is the only coverage. --esp --boot rebuilds just the boot region
    (no slow root resilver), which is where the bug lives; the root array stays
    intact."""
    scen = "replace healthy primary"
    primary = s.members()[0]
    _, out = s.c.run("mountpoint -q /boot/efi && echo MOUNTED || echo absent")
    s.report.add(scen, "esp mounted", PASS if "MOUNTED" in out else WARN, _tail(out))
    s.replace([primary], "--esp --boot")  # raises on the bug
    s.report.add(scen, "replace", PASS, "primary boot region rebuilt with esp mounted")
    s.reboot_login()
    s.report.add(scen, "reboot", PASS, "booted after primary replace")


# every in-place scenario, in run order. selectable individually with --scenario.
INPLACE = [
    sysbench,
    corrupt_data_within,
    corrupt_headers,
    truncate_disks,
    corrupt_efiboot,
    replace_primary,
]

# the default bundled run excludes corrupt_efiboot: run last after several
# corrupt+repair cycles, a boot failure there would be confounded by accumulated
# state rather than the boot damage itself. it is run on its own (a fresh install)
# via `--scenario corrupt_efiboot` / `make test-vm-boot`.
DEFAULT_INPLACE = [sysbench, corrupt_data_within, corrupt_headers, truncate_disks]

# the livecd rescue flow (corrupt 4/4 + recover) is selectable by this name too.
RESCUE = "rescue"

# the costly fileio benchmark; --skip-benchmark drops it for fast correctness /
# troubleshooting runs (it is orthogonal to the resilience checks).
BENCHMARK = sysbench.__name__


def scenario_names() -> list[str]:
    """every selectable scenario name for --scenario, in run order."""
    return [s.__name__ for s in INPLACE] + [RESCUE]


def default_scenario_names() -> list[str]:
    """the default bundled run's scenario names (in run order), including rescue."""
    return [s.__name__ for s in DEFAULT_INPLACE] + [RESCUE]


def select_inplace(names: list[str]) -> list:
    """the in-place scenarios to run: the default bundle when names is empty, else
    the requested subset (from any in-place scenario), in INPLACE order."""
    if not names:
        return list(DEFAULT_INPLACE)
    return [s for s in INPLACE if s.__name__ in names]
