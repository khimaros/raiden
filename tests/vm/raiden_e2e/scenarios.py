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


def _array_check_names(stack: str) -> list[str]:
    """the array/fs health check name(s) doctor emits, which vary by stack: md/lvm
    and dm-integrity get two distinct checks ('md boot' + 'md root'); zfs ->
    'zfs status', btrfs -> 'btrfs status', bcachefs -> 'fs status' (btrfs device
    stats fails on it)."""
    if "zfs" in stack:
        return ["zfs status"]
    if "btrfs" in stack:
        return ["btrfs status"]
    if "bcachefs" in stack:
        return ["fs status"]
    return ["md boot", "md root"]  # dm-crypt~md~lvm~ext4/xfs and dm-integrity


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

    def raiden_check(self, args: str, timeout: int = 2400) -> tuple[int, str]:
        """run raiden and return (rc, out); fails the run loudly on a non-zero rc.
        for the read-only inspection commands (doctor, sync --dry-run) whose
        whole point is to succeed on a healthy installed system."""
        rc, out = self.raiden(args, timeout=timeout)
        if rc != 0:
            raise RuntimeError(f"raiden {args} failed (rc={rc}): {_tail(out)}")
        return rc, out

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


def doctor(s: Session) -> None:
    """`raiden doctor` walks every installed-system layer and must pass cleanly on
    a healthy system: disks, mounts, fstab, crypttab, luks headers, array, boot
    + esp mirrors, grub, initrd, kernel hooks, manifest. a single failing check
    means a layer raiden installed is not reporting healthy, so the whole scenario
    fails (not WARN)."""
    scen = "doctor healthy"
    rc, out = s.raiden_check("doctor")
    # every check category must be present (none silently skipped) -- the
    # enumeration lives here, in the controlled vm, rather than against a dev host.
    names = [
        "disk presence", "boot mount", "mount consistency", "fstab", "boot uuid",
        "esp uuid", "crypttab", "luks headers", "luks backup",
        *_array_check_names(s.cfg.stack), "boot mirrors",
        "boot drift", "boot bootloader", "esp bootloader", "grub", "initrd",
        "recover hook", "recover",
        "kernel hooks", "esp hook", "efibootmgr", "debconf", "manifest",
    ]
    missing = [n for n in names if n not in out]
    s.report.add(scen, "checks present", PASS if not missing else FAIL,
                 "all present" if not missing else f"missing: {missing}")
    # the summary line is the authoritative pass/fail signal.
    if "all checks passed" in out:
        s.report.add(scen, "report", PASS, _tail(out))
    else:
        s.report.add(scen, "report", FAIL, _tail(out))
        raise RuntimeError(f"doctor reported failures:\n{out}")


def sync_mirrors(s: Session) -> None:
    """`raiden sync boot` / `sync efi` re-sync the independent mirrors from the
    live primary. on a healthy system both must verify the source (default, no
    --force) and report the mirrors in --dry-run, then sync cleanly with --yes.
    corrupting one mirror's /boot and re-syncing must restore it (the whole point
    of the sync). skipped in boot.raid mode (mdadm handles /boot replication; sync
    boot is a no-op and there are no independent /boot mirrors to corrupt)."""
    scen = "sync mirrors"
    if s.cfg.boot_raid:
        s.report.add(scen, "sync", INFO, "skipped: boot.raid (mdadm replicates /boot)")
        return
    # dry-run resolves the source + mirrors without touching disks.
    rc, out = s.raiden_check("sync boot --dry-run")
    ok = "source:" in out and "mirrors:" in out and "rsync" in out
    s.report.add(scen, "boot dry-run", PASS if ok else FAIL, _tail(out))
    rc, out = s.raiden_check("sync efi --dry-run")
    ok = "source:" in out and "mirrors:" in out and "rsync" in out
    s.report.add(scen, "efi dry-run", PASS if ok else FAIL, _tail(out))
    # a real sync of both, with verification on (default).
    _, out = s.raiden_check("sync boot --yes")
    s.report.add(scen, "boot sync", PASS, _tail(out))
    _, out = s.raiden_check("sync efi --yes")
    s.report.add(scen, "efi sync", PASS, _tail(out))
    # corrupt CONTENT in a non-primary mirror's /boot (not the filesystem itself --
    # sync boot re-syncs into mountable mirrors; a destroyed fs is a `replace` job,
    # not a sync job). delete grub.cfg from the mirror, then confirm sync restores
    # it from the source (a content check, not just rc).
    #
    # pick a mirror that is NOT the live /boot source: the firmware may boot any
    # member's esp, so members()[0] is not reliably the source. findmnt gives the
    # device backing the mounted /boot; corrupt a different member's partition 2.
    rc, src_out = s.c.run("findmnt -n -o SOURCE /boot", timeout=30)
    src_dev = src_out.strip()
    # src_dev is like /dev/vdb2; derive the member name (vdb) and pick another.
    mirror = next(
        (m for m in s.members() if f"/dev/{m}2" != src_dev),
        None,
    )
    if mirror is None:
        s.report.add(scen, "repair", WARN, "only one /boot member; cannot test repair")
        return
    boot_dev = f"/dev/{mirror}2"
    log(f"deleting grub.cfg from mirror {boot_dev} (source is {src_dev}) to exercise sync repair")
    rc, out = s.c.run(
        f"m=$(mktemp -d) && mount {boot_dev} $m && "
        f"rm -f $m/grub/grub.cfg && ! test -e $m/grub/grub.cfg && echo DELETED; "
        f"umount $m; rmdir $m",
        timeout=120,
    )
    if rc != 0 or "DELETED" not in out:
        s.report.add(scen, "repair", FAIL, f"could not stage corruption: {_tail(out)}")
        return
    _, out = s.raiden_check("sync boot --yes")
    rc, out = s.c.run(
        f"m=$(mktemp -d) && mount {boot_dev} $m && "
        f"test -s $m/grub/grub.cfg && echo SYNC_OK; umount $m; rmdir $m",
        timeout=120,
    )
    s.report.add(
        scen, "repair", PASS if rc == 0 and "SYNC_OK" in out else FAIL, _tail(out)
    )


def doctor_fix(s: Session) -> None:
    """`raiden doctor --fix` repairs the auto-fixable checks, confirming each one
    individually (--yes auto-accepts; without it the per-fix prompt can be declined).
    exercises the repairs end to end: a removed boot-mirror kernel hook is
    reinstalled, and a mirror esp whose fs uuid has been skewed off the shared one
    is re-stamped back (the legacy-host esp-uuid migration). after each repair,
    doctor must report that layer healthy again; a declined fix must leave it as-is."""
    scen = "doctor fix"

    # -- a removed kernel hook is reinstalled by --fix --yes (independent /boot only;
    # in boot.raid mode mdadm replicates /boot and there is no such hook).
    if not s.cfg.boot_raid:
        hook = "/etc/kernel/postinst.d/zzz-raiden-boot-mirror"
        s.c.run(f"rm -f {hook}", check=True)
        _, out = s.raiden("doctor")  # missing hook is a warn -> rc stays 0
        detected = "kernel hooks" in out and "missing" in out
        s.report.add(scen, "hook detect", PASS if detected else FAIL, _tail(out))
        s.raiden("doctor --fix --yes")
        rc, out = s.c.run(f"test -x {hook} && echo REINSTALLED", timeout=30)
        ok = rc == 0 and "REINSTALLED" in out
        s.report.add(scen, "hook reinstall", PASS if ok else FAIL, _tail(out))

    # -- the recover bundle: the raiden recovery hook bakes raiden + the manifest
    # into the initrd. remove it and rebuild so the initrd loses them (the legacy-
    # install state), then --fix must REINSTALL the hook and re-bake -- a plain
    # rebuild cannot add raiden without the hook (the gap this fix closes).
    rhook = "/etc/initramfs-tools/hooks/raiden"
    latest = "$(ls -t /boot/initrd.img-* | head -1)"
    s.c.run(f"rm -f {rhook} && update-initramfs -u", check=True, timeout=900)
    _, out = s.raiden("doctor")  # initrd missing raiden -> recover warn (rc stays 0)
    detected = "recover" in out and "missing" in out
    s.report.add(scen, "recover detect", PASS if detected else FAIL, _tail(out))
    s.raiden("doctor --fix --yes", timeout=900)
    rc, out = s.c.run(
        f"test -x {rhook} && lsinitramfs {latest} | grep -q sbin/raiden && echo OK",
        timeout=300,
    )
    ok = rc == 0 and "OK" in out
    s.report.add(scen, "recover fix", PASS if ok else FAIL, _tail(out))

    # -- a skewed mirror esp uuid: detect -> decline leaves it -> --yes re-stamps it.
    # efi only (no esp in bios mode); pick a mirror that is NOT the live /boot/efi
    # source so skewing it cannot disturb the running mount.
    _, src_out = s.c.run("findmnt -n -o SOURCE /boot/efi", timeout=30)
    src_dev = src_out.strip()
    if not src_dev:
        s.report.add(scen, "esp uuid", INFO, "skipped: /boot/efi not mounted (bios mode)")
        return
    mirror = next((m for m in s.members() if f"/dev/{m}1" != src_dev), None)
    if mirror is None:
        s.report.add(scen, "esp uuid", WARN, "no mirror esp to skew; cannot test")
        return
    esp = f"/dev/{mirror}1"

    def esp_uuid(dev: str) -> str:
        return s.c.run(f"blkid -s UUID -o value {dev}", timeout=30)[1].strip()

    shared = esp_uuid(src_dev)
    log(f"skewing esp uuid on mirror {esp} (shared is {shared}, source {src_dev})")
    s.c.run(f"mkfs.msdos -F 32 -s 1 -n EFI -i DEADBEEF {esp}", check=True, timeout=120)
    _, out = s.raiden("doctor")  # uuid skew is a warn -> rc stays 0
    detected = "esp uuid" in out and "not shared" in out
    s.report.add(scen, "esp uuid detect", PASS if detected else FAIL, _tail(out))

    # preview: --fix --dry-run prints the fix FLOW (the exact mkfs command + the
    # device, in order), not the checks table, and changes nothing -- the
    # look-before-you-leap path for hardware.
    _, out = s.raiden("doctor --fix --dry-run")
    previewed = (
        "mkfs.msdos" in out and esp in out
        and "status  detail" not in out  # the table is suppressed in the flow view
        and esp_uuid(esp) != shared  # nothing changed
    )
    s.report.add(scen, "esp uuid preview", PASS if previewed else FAIL, _tail(out))

    # decline every fix (no --yes; feed 'n' to each per-fix prompt): the esp must
    # stay skewed, proving the prompt gates the destructive re-stamp.
    s.c.run(f"yes n | {RAIDEN} doctor --fix", timeout=300)
    declined = esp_uuid(esp) != shared
    s.report.add(scen, "esp uuid decline", PASS if declined else FAIL, f"{esp}={esp_uuid(esp)}")

    # now accept: --fix --yes re-stamps the mirror to the shared uuid and re-syncs.
    s.raiden("doctor --fix --yes")
    restamped = esp_uuid(esp) == shared and shared != ""
    s.report.add(
        scen, "esp uuid restamp",
        PASS if restamped else FAIL, f"{esp}={esp_uuid(esp)} shared={shared}",
    )
    # the divergence the fix targeted must be gone (robust against unrelated warns
    # left by earlier scenarios -- assert the specific layer, not a clean bill).
    _, out = s.raiden("doctor")
    s.report.add(scen, "post-fix doctor", PASS if "not shared" not in out else FAIL, _tail(out))


# every in-place scenario, in run order. selectable individually with --scenario.
INPLACE = [
    sysbench,
    corrupt_data_within,
    corrupt_headers,
    truncate_disks,
    corrupt_efiboot,
    replace_primary,
    doctor,
    sync_mirrors,
    doctor_fix,
]

# the default bundled run is resilience-only: the sysbench benchmark is opt-in
# (it is ~26min and orthogonal to correctness; add it with --benchmark or the
# `sysbench` scenario name). it also excludes corrupt_efiboot: run last after
# several corrupt+repair cycles, a boot failure there would be confounded by
# accumulated state rather than the boot damage itself, so it runs on its own (a
# fresh install) via `--scenario corrupt_efiboot` / `make test-vm-boot`. doctor
# and sync_mirrors are cheap, read-only checks included in every default pass.
DEFAULT_INPLACE = [
    corrupt_data_within,
    corrupt_headers,
    truncate_disks,
    doctor,
    sync_mirrors,
    doctor_fix,
]

# the livecd rescue flow (corrupt 4/4 + recover) is selectable by this name too.
RESCUE = "rescue"

# the costly fileio benchmark; off by default (orthogonal to the resilience
# checks), opt in with --benchmark or the `sysbench` scenario name.
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
