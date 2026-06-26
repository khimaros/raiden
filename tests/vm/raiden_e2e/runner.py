"""orchestrate a full run, with detailed logging at every step.

the short live/install phase is driven by send-key (the stock live iso has no
serial console): press enter at the fast grub prompt, then type one command that
runs raiden, redirects its output to the virtiofs share, records a result, and
powers off. the host observes the domain state to know when it finished.

the installed system enables a serial console, so every post-install phase is
driven over serial with pexpect, waiting on real console state.

the harness logs what it is doing to stdout. the serial console (control
sequences stripped) and the live install output are shown in the harness output
too and saved to console.log in the run directory."""

from __future__ import annotations

import datetime
import sys
import time

from . import scenarios as sc
from .config import INITRAMFS_RECOVERY, VMConfig
from .console import AbortRun, Console, SkipScenario
from .log import CleanStream, Tee, log
from .report import FAIL, PASS, WARN, Report
from .supervisor import Operator
from .vm import VM

# single-quoted so the live shell does not expand $? before sudo; tee keeps the
# output on the vm console (watchable in virt-viewer) while also saving it to the
# share. the braces capture raiden's real exit code (not tee's) into the result.
# the raiden invocation runs through the staged livecd.sh (its install/rescue
# subcommands, RAIDEN_BIN pointed at the staged binary) -- the same entrypoint a
# human uses on real hardware -- so --verbose and the exact flags live in one
# place; only the vm glue (mount, tee, result file, poweroff) stays here.
_INSTALL = (
    "sudo sh -c '"
    "mkdir -p /srv/raiden; mount -t virtiofs payload /srv/raiden; "
    "{ RAIDEN_BIN=/srv/raiden/raiden sh /srv/raiden/livecd.sh install "
    "--yes --config /srv/raiden/raiden.toml --password-file /srv/raiden/password; "
    "echo $? > /srv/raiden/install.result; } "
    "2>&1 | tee /srv/raiden/live.log; sync; poweroff'"
)
_RESCUE = (
    "sudo sh -c '"
    "mkdir -p /srv/raiden; mount -t virtiofs payload /srv/raiden; "
    "{ RAIDEN_BIN=/srv/raiden/raiden sh /srv/raiden/livecd.sh rescue "
    "--yes --config /srv/raiden/raiden.toml --password-file /srv/raiden/password; "
    "echo $? > /srv/raiden/rescue.result; } "
    "2>&1 | tee /srv/raiden/live.log; "
    "ls -A /mnt > /srv/raiden/rescue.files 2>&1; sync; poweroff'"
)


def _now() -> str:
    return datetime.datetime.now().isoformat(timespec="seconds")


def _log_path(report_out: str) -> str:
    """the run log sits next to the report: same name, .log instead of .md."""
    return report_out[:-3] + ".log" if report_out.endswith(".md") else report_out + ".log"


def run(cfg: VMConfig, out: str | None = None) -> Report:
    vm = VM(cfg)
    report = Report(
        cfg.stack, cfg.resolved_level(), _now(), config_rows=vm.config_rows, out_path=out
    )
    supervisor = Operator() if cfg.interactive else None
    # tee everything (phase log, install output, serial console) to a run log kept
    # next to the report -- the workdir console.log is removed with the vm on
    # cleanup, so the report's sibling .log is the durable record of the run.
    real_stdout = sys.stdout
    runlog = open(_log_path(out), "w") if out else None
    if runlog:
        sys.stdout = Tee(real_stdout, runlog)
    logf = open(vm.console_log, "a")
    # the serial console is shown in the harness output and saved to console.log,
    # with terminal control sequences stripped from both.
    console_out = CleanStream(Tee(logf, sys.stdout))
    log(f"run: stack={cfg.stack} level={cfg.resolved_level()} name={cfg.name}")
    log(f"images + console log under: {vm.workdir}")
    try:
        vm.prepare(fresh=not cfg.skip_install)
        if cfg.skip_install:
            log("skip-install: jumping straight to the post-install scenarios")
        elif not _install(cfg, vm, report):
            log("install failed; stopping")
            return report
        if cfg.install_only:
            log("install-only: install succeeded; stopping before the scenarios")
            report.mark_complete()
            return report
        _run_scenarios(cfg, vm, report, supervisor, console_out)
        if not cfg.scenarios or sc.RESCUE in cfg.scenarios:
            _rescue_flow(cfg, vm, report, supervisor, console_out)
        report.mark_complete()  # reached the end of the flow: not a partial run
    except AbortRun as exc:
        report.add("run", "aborted", FAIL, str(exc))
    except KeyboardInterrupt:
        # the report has already been flushed after each check; record the
        # interruption, then let the finally block tear the vm down.
        log("interrupted (ctrl-c); writing the partial report and cleaning up")
        report.add("run", "interrupted", FAIL, "operator sent ctrl-c")
    finally:
        console_out.close()
        logf.close()
        vm.cleanup()
        sys.stdout = real_stdout
        if runlog:
            runlog.close()
    log("run complete")
    return report


def _read(vm: VM, name: str) -> str:
    path = vm.payload / name
    return path.read_text().strip() if path.exists() else ""


def _live_run(cfg: VMConfig, vm: VM, command: str, marker: str, timeout: int) -> str:
    """boot the livecd, type one command over send-key, stream its shared log,
    and return the marker file's contents once the guest powers off."""
    for stale in ("live.log", marker):
        p = vm.payload / stale
        if p.exists():
            p.unlink()
    vm.start(cdrom_first=True)
    log(f"waiting {cfg.grub_seconds}s for the grub prompt, then booting the live entry")
    time.sleep(cfg.grub_seconds)
    vm.send_line("")  # enter: boot the default (live) entry
    log(f"waiting {cfg.live_boot_seconds}s for the live shell, then sending the command")
    time.sleep(cfg.live_boot_seconds)
    vm.send_line(command)
    log(f"live phase running (also watchable via virt-viewer {cfg.name}); raiden output:")
    result = _await_live(cfg, vm, marker, timeout)
    vm.destroy()
    return result


def _await_live(cfg: VMConfig, vm: VM, marker: str, timeout: int) -> str:
    """stream the guest's shared log to stdout and observe the domain state until
    the guest powers off. the log is tee'd by the guest, so it also shows on the
    vm console."""
    logpath = vm.payload / "live.log"
    pos = 0

    def drain() -> None:
        nonlocal pos
        if logpath.exists():
            data = logpath.read_text(errors="replace")
            if len(data) > pos:
                sys.stdout.write(data[pos:])
                sys.stdout.flush()
                pos = len(data)

    waited = 0
    while waited < timeout:
        drain()
        if vm.domstate() in ("", "shut off"):
            drain()
            log("guest powered off")
            return _read(vm, marker)
        time.sleep(cfg.poll_seconds)
        waited += cfg.poll_seconds
    log(f"timed out after {timeout}s waiting for the guest to power off")
    return _read(vm, marker)


def _install(cfg: VMConfig, vm: VM, report: Report) -> bool:
    log("=== install (live, send-key driven) ===")
    rc = _live_run(cfg, vm, _INSTALL, "install.result", cfg.install_timeout)
    passed = rc == "0"
    report.add("install", "run", PASS if passed else FAIL, f"exit={rc or 'none'}")
    log(f"install result: {'PASS' if passed else 'FAIL'} (exit={rc or 'none'})")
    return passed


def _connect(cfg, vm, supervisor, console_out) -> Console:
    log(f"connecting to the serial console (also saved to {vm.console_log})")
    log("waiting for the unlock prompt")
    con = Console(
        cfg.name,
        logfile=console_out,
        supervisor=supervisor,
        initramfs_recovery=INITRAMFS_RECOVERY,
    )
    con.unlock(cfg.password)
    log("unlocked; waiting for login")
    con.login("root", cfg.root_password)
    log("logged in over serial")
    return con


def _run_scenarios(cfg, vm, report, supervisor, console_out) -> None:
    log("=== boot installed system + resilience scenarios (serial driven) ===")
    vm.start()
    con = _connect(cfg, vm, supervisor, console_out)
    try:
        report.add("install", "boot", PASS, "unlocked and logged in over serial")
        session = sc.Session(vm, con, cfg, report)
        session.mount_payload()
        for scenario in sc.select_inplace(list(cfg.scenarios)):
            name = scenario.__name__
            log(f"--- scenario: {name} ---")
            try:
                scenario(session)
            except SkipScenario as exc:
                report.add(name, "run", WARN, f"skipped: {exc}")
            except AbortRun:
                raise
            except Exception as exc:  # state may be unreliable but keep going
                log(f"scenario {name} errored: {exc}")
                report.add(name, "run", FAIL, str(exc)[:200])
    finally:
        con.close()
        vm.destroy()


def _rescue_flow(cfg, vm, report, supervisor, console_out) -> None:
    scen = "corrupt 4/4 data"
    log(f"=== {scen} + livecd rescue ===")
    vm.start()
    con = _connect(cfg, vm, supervisor, console_out)
    try:
        session = sc.Session(vm, con, cfg, report)
        session.mount_payload()
        # with every member corrupt, reading back (survival) can hang or panic the
        # guest, and a clean shutdown may never return. so we only induce the
        # damage here, then hard-reset and recover from the livecd -- we never wait
        # on a read or scrub that may never complete.
        session.corrupt_random([f"{d}3" for d in session.members()], 10000)
        report.add(scen, "corrupt", PASS, "wrote random data to all 4 members")
    except Exception as exc:
        report.add(scen, "corrupt", WARN, f"corrupting members: {str(exc)[:120]}")
    finally:
        con.close()
        vm.destroy()  # hard reset; no clean shutdown attempted after 4/4 corruption

    log("booting the livecd to rescue the damaged array")
    rc = _live_run(cfg, vm, _RESCUE, "rescue.result", cfg.rescue_timeout)
    files = _read(vm, "rescue.files")
    ok = rc == "0" and bool(files)
    report.add(scen, "rescue", PASS if ok else WARN, f"exit={rc or 'none'} files={files[:120]}")
