"""the full libvirt vm run, gated behind RAIDEN_VM_ISO and a present kvm device,
so it is skipped on hosts that cannot run it. set RAIDEN_VM_ISO to a debian live
iso path to enable it."""

import os
import shutil

import pytest

ISO = os.environ.get("RAIDEN_VM_ISO")


def test_resolved_level_reads_config_when_level_unset():
    # the report level must reflect what the install actually uses: with the
    # raid10 example and no --level, it is 10 -- not the md stack default of 6.
    from raiden_e2e.config import EXAMPLES_DIR, VMConfig

    raid10 = str(EXAMPLES_DIR / "dm-crypt~md~lvm~ext4.raid10.aead.toml")
    assert VMConfig(stack="dm-crypt~md~lvm~ext4", config_file=raid10).resolved_level() == "10"
    # an explicit --level still wins over the config file.
    assert VMConfig(stack="dm-crypt~md~lvm~ext4", level="0", config_file=raid10).resolved_level() == "0"
    # no config + no level -> the stack default.
    assert VMConfig(stack="dm-crypt~md~lvm~ext4").resolved_level() == "6"


def test_run_log_path_is_report_sibling():
    # the run log is persisted next to the report: same name, .log extension.
    from raiden_e2e.runner import _log_path

    assert _log_path("reports/dm-crypt~btrfs-20260622-143936.md") == (
        "reports/dm-crypt~btrfs-20260622-143936.log"
    )
    assert _log_path("reports/run") == "reports/run.log"


def test_scenario_selection():
    from raiden_e2e import scenarios as sc

    names = sc.scenario_names()
    assert names[0] == "sysbench" and names[-1] == "rescue"
    assert "corrupt_efiboot" in names  # still selectable...
    # ...but excluded from the default bundle, where its result would be confounded.
    assert sc.select_inplace([]) == sc.DEFAULT_INPLACE
    assert "corrupt_efiboot" not in [s.__name__ for s in sc.select_inplace([])]
    # a subset keeps INPLACE order regardless of how it was requested, and the
    # rescue flow is not an in-place scenario.
    picked = sc.select_inplace(["corrupt_efiboot", "truncate_disks", "rescue"])
    assert [s.__name__ for s in picked] == ["truncate_disks", "corrupt_efiboot"]


def test_skip_benchmark_drops_sysbench_keeps_resilience():
    # --skip-benchmark runs the default bundle minus the costly sysbench pass, so
    # fast correctness/troubleshooting runs skip it but still exercise the
    # resilience scenarios and the rescue flow.
    from raiden_e2e import scenarios as sc

    defaults = sc.default_scenario_names()
    assert sc.BENCHMARK == "sysbench" and defaults[0] == "sysbench"
    assert "rescue" in defaults  # rescue is part of the default bundle
    skipped = [n for n in defaults if n != sc.BENCHMARK]
    assert "sysbench" not in skipped
    assert {"corrupt_data_within", "corrupt_headers", "truncate_disks", "rescue"} <= set(skipped)


def test_render_config_from_example():
    import tomllib

    from raiden_e2e.config import EXAMPLES_DIR
    from raiden_e2e.vm import render_config

    # the zfs example wants plain crypt (no aead); the harness must preserve the
    # stack-specific keys and overlay only the test-specific ones.
    text = render_config(
        str(EXAMPLES_DIR / "dm-crypt~zfs.raidz2.toml"),
        ["vda", "vdb", "vdc"],
        boot_raid=True,
    )
    cfg = tomllib.loads(text)
    # preserved from the example unchanged:
    assert cfg["raid"]["stack"] == "dm-crypt~zfs"
    assert cfg["crypt"]["integrity"] == "none"
    assert cfg["crypt"]["cipher"] == "aes-xts-plain64"
    # overlaid for the test environment:
    assert cfg["install"]["serial_console"] is True
    assert cfg["disks"]["members"] == ["vda", "vdb", "vdc"]
    assert cfg["boot"]["raid"] is True
    # the benchmark package is installed for a default run...
    assert "sysbench" in cfg["install"]["extra_packages"]


def test_render_config_skips_benchmark_package():
    import tomllib

    from raiden_e2e.config import EXAMPLES_DIR
    from raiden_e2e.vm import render_config

    # ...but a --skip-benchmark run does not install sysbench (nothing else needs it).
    text = render_config(
        str(EXAMPLES_DIR / "dm-crypt~zfs.raidz2.toml"), ["vda", "vdb"], with_benchmark=False
    )
    cfg = tomllib.loads(text)
    assert "sysbench" not in cfg["install"]["extra_packages"]


def test_default_out_is_timestamped_under_reports():
    import datetime

    from raiden_e2e.run import REPORTS_DIR, default_out

    when = datetime.datetime(2026, 6, 21, 19, 30, 5)
    # the stack name keeps its ~ separators in the report filename.
    assert default_out("dm-crypt~zfs", "boot", when) == str(
        REPORTS_DIR / "dm-crypt~zfs-boot-20260621-193005.md"
    )
    # no tag -> no extra segment
    assert default_out("dm-crypt~zfs", "", when) == str(
        REPORTS_DIR / "dm-crypt~zfs-20260621-193005.md"
    )


def test_btrfs_has_initramfs_recovery():
    from raiden_e2e.config import INITRAMFS_RECOVERY

    cmds = INITRAMFS_RECOVERY["dm-crypt~btrfs"]
    assert any("mount -o degraded" in c for c in cmds)


def test_unlock_prompts_match_keyscript_and_plain_crypttab():
    # unlock() must answer both wordings: the keyscript stacks' "Caching passphrase
    # for X" and a plain crypttab entry's "Please unlock disk X" (the dm-integrity
    # stack's single md_root_crypt). missing the latter hung the dm-integrity boot.
    import re

    from raiden_e2e.console import UNLOCK_PROMPTS

    def matched(line):
        return any(re.search(p, line) for p in UNLOCK_PROMPTS)

    assert matched("Please unlock disk md_root_crypt:")
    assert matched("Caching passphrase for vda3_crypt:")
    assert matched("Please enter passphrase for disk vda3_crypt (vda3_crypt):")


def test_initramfs_recovery_runs_commands_then_exits():
    import pexpect

    from raiden_e2e.console import Console

    sent = []

    class FakeChild:
        def expect(self, patterns, timeout=None):
            return 0  # the (initramfs) prompt reappears after each command

        def sendline(self, s):
            sent.append(s)

    c = Console.__new__(Console)  # skip __init__ (no real pexpect spawn)
    c._pexpect = pexpect
    c.initramfs_recovery = ["btrfs device scan", "mount -o degraded d /root"]
    c.initramfs_recoveries = 0
    c.child = FakeChild()
    c._recover_in_initramfs()
    # every recovery command, then exit to resume the boot.
    assert sent == ["btrfs device scan", "mount -o degraded d /root", "exit"]


def test_initramfs_drop_without_recovery_aborts():
    import pytest

    from raiden_e2e.console import AbortRun, Console

    c = Console.__new__(Console)
    c.initramfs_recovery = []
    with pytest.raises(AbortRun):
        c._recover_in_initramfs()


def _console_with(child):
    import pexpect

    from raiden_e2e.console import Console

    c = Console.__new__(Console)
    c._pexpect = pexpect
    c.supervisor = None
    c.initramfs_recovery = ["mount -o degraded d /root"]
    c.initramfs_recoveries = 0
    c.child = child
    # don't spawn a real `virsh console` in unit tests; just record reconnects.
    c.reconnects = []
    c.reconnect = lambda: c.reconnects.append(1)
    return c


def test_reach_login_reconnects_and_nudges_serial_getty():
    # after a degraded boot the serial getty will not print its prompt over the
    # stale console connection; the harness must reconnect (fresh carrier) and
    # send a bare CR ("\r", what ENTER sends). this was the bug that hung the
    # degraded btrfs boot: a newline on the stale connection never triggers it.
    import pexpect

    sent = []

    class FakeChild:
        calls = 0

        def expect(self, patterns, timeout=None):
            self.calls += 1
            if self.calls == 1:
                raise pexpect.TIMEOUT("getty not ready")  # no prompt yet
            return 0  # login: appears once reconnected + nudged

        def send(self, s):
            sent.append(s)

        def sendline(self, s):
            sent.append(s)

    c = _console_with(FakeChild())
    c._reach_login(timeout=60)
    assert sent == ["\r"]  # a bare carriage return prodded the getty
    assert c.reconnects == [1]  # on a fresh connection


def test_reach_login_follows_through_initramfs():
    seq = [1, 0, 0]  # (initramfs) prompt, the recovery's prompt wait, then login:
    sent = []

    class FakeChild:
        def expect(self, patterns, timeout=None):
            return seq.pop(0)

        def send(self, s):
            sent.append(s)

        def sendline(self, s):
            sent.append(s)

    c = _console_with(FakeChild())
    c._reach_login(timeout=60)
    # runs recovery then matches login on the next poll; the getty, when it stalls,
    # is handled by a reconnect+nudge on timeout (covered by the test above).
    assert sent == ["mount -o degraded d /root", "exit"]
    assert c.reconnects == []


def test_submit_credentials_is_prompt_driven():
    # respond to whatever prompt appears: username on login:, password on Password:,
    # finish on a shell prompt. tolerant of the empty-username artifact left by
    # surfacing the getty, which would desync a fixed user-then-password sequence.
    seq = [2, 1, 0]  # login: (empty-username artifact) -> Password: -> shell prompt
    sent = []

    class FakeChild:
        def expect(self, patterns, timeout=None):
            return seq.pop(0)

        def send(self, s):
            sent.append(s)

        def sendline(self, s):
            sent.append(s)

    _console_with(FakeChild())._submit_credentials("root", "pw", timeout=60)
    # initial username, re-offered on the login: artifact, then the password.
    assert sent == ["root", "root", "pw"]


def test_grade_reboot_warns_only_when_boot_needed_intervention():
    # a boot that came up on its own is a clean PASS; one that only reached login
    # because the harness ran the initramfs recovery (manual intervention, eg.
    # mount -o degraded) is a WARN -- it did not boot unattended. stack-agnostic.
    from raiden_e2e.report import PASS, WARN, Report
    from raiden_e2e.scenarios import Session

    class FakeCfg:
        password = root_password = "test"

        def disk_names(self):
            return ["vda"]

    class FakeConsole:
        def __init__(self, intervene):
            self._intervene = intervene
            self.initramfs_recoveries = 0

        def reboot(self):
            pass

        def unlock(self, password):
            pass

        def login(self, user, password):
            if self._intervene:  # the boot dropped to the initramfs and was recovered
                self.initramfs_recoveries += 1

        def run(self, *a, **k):
            return (0, "")

    def grade(intervene):
        rep = Report("s", "l", "t")
        Session(None, FakeConsole(intervene), FakeCfg(), rep).grade_reboot("scen", "booted")
        return rep.results[-1]

    assert grade(intervene=False).status == PASS  # unattended boot
    warned = grade(intervene=True)
    assert warned.status == WARN and "intervention" in warned.detail


def test_recover_in_initramfs_tolerates_missing_prompt():
    # a recovery that brings the root online can let the boot resume with no
    # further (initramfs) prompt; the wait must be bounded, not block forever
    # (the failure that hung the corrupt_headers degraded boot in _expect).
    import pexpect

    sent = []

    class FakeChild:
        def expect(self, patterns, timeout=None):
            raise pexpect.TIMEOUT("no prompt after recovery")

        def sendline(self, s):
            sent.append(s)

    _console_with(FakeChild())._recover_in_initramfs()  # must not hang or raise
    assert sent[-1] == "exit"  # still resumes the boot


def test_reach_login_bounded_when_prompt_never_appears():
    # bounded so a boot that never reaches a prompt is graded, not hung forever
    # (the failure mode that left the run blocked for over an hour).
    import pexpect
    import pytest

    from raiden_e2e.console import AbortRun

    sent = []

    class FakeChild:
        def expect(self, patterns, timeout=None):
            raise pexpect.TIMEOUT("never")

        def send(self, s):
            sent.append(s)

        def sendline(self, s):
            sent.append(s)

    c = _console_with(FakeChild())
    with pytest.raises(AbortRun):
        c._reach_login(timeout=30)  # 30 // 15 = 2 polls
    assert sent == ["\r", "\r"]  # nudged twice with CRs, then gave up
    assert c.reconnects == [1, 1]  # each on a fresh connection


def _prereqs_present() -> bool:
    return bool(ISO) and os.path.exists("/dev/kvm") and all(
        shutil.which(t) for t in ("virsh", "qemu-img", "xorriso")
    )


@pytest.mark.skipif(not _prereqs_present(), reason="needs RAIDEN_VM_ISO, kvm, virsh, qemu-img, xorriso")
def test_full_install_and_resilience(tmp_path):
    from raiden_e2e.config import VMConfig
    from raiden_e2e.runner import run

    repo = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
    cfg = VMConfig(
        stack=os.environ.get("RAIDEN_VM_STACK", "dm-crypt~md~lvm~ext4"),
        iso=os.path.abspath(ISO),
        binary=os.path.join(repo, "target", "release", "raiden"),
        image_dir=str(tmp_path),
    )
    report = run(cfg)
    (tmp_path / "report.md").write_text(report.to_markdown())
    assert not report.failed(), report.to_markdown()
