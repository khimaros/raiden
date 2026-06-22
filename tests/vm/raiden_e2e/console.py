"""serial-console driver built on pexpect over `virsh console`.

the robust, human-free replacement for keystroke-injection: it waits for real
prompts (login, the cryptsetup unlock prompt, a shell prompt, a command's exit
sentinel) instead of sleeping. the one timed action is reaching the login prompt
after a degraded boot: the serial getty prints no prompt until a carriage return
arrives, and over the connection that survived the reboot a CR is ignored, so
_reach_login reconnects (fresh carrier) and sends a CR on a short poll until the
prompt appears. the serial console works at every stage (initramfs unlock,
degraded boots, even panics), which ssh cannot guarantee.

every wait goes through `_expect`. when it cannot find what it expects (a
timeout or hangup), it consults the optional supervisor: in interactive runs the
operator is shown the situation and chooses when to retry, skip, or abort; in
unattended runs the failure propagates and is graded."""

from __future__ import annotations

import re
import sys

SHELL_PROMPT = "RAIDEN_SHELL> "
# the busybox rescue shell prompt shown when the boot cannot mount the root.
INITRAMFS_PROMPT = r"\(initramfs\) "
# boot-time crypt passphrase prompts to answer. the keyscript stacks print
# "Caching passphrase for X" (decrypt_keyctl); a plain crypttab entry (the
# dm-integrity stack's single md_root_crypt) prints cryptsetup's default
# "Please unlock disk X" -- both must be recognized.
UNLOCK_PROMPTS = [
    r"[Pp]lease enter passphrase",
    r"[Ee]nter passphrase",
    r"[Pp]assphrase for",
    r"[Uu]nlock disk",
]
# seconds between carriage-return nudges while waiting for the serial getty: it
# prints its login prompt only after a CR arrives on the line (see _reach_login).
LOGIN_NUDGE_INTERVAL = 15
# bound the wait for the initramfs prompt to reappear after a recovery command:
# a recovery that brings the root online can let the boot resume with no further
# prompt, so this wait must not block forever (see _recover_in_initramfs).
RECOVERY_CMD_TIMEOUT = 30
_MARK = "__RAIDEN_RC__"
_RC_RE = re.escape(_MARK) + r"(-?\d+)" + re.escape(_MARK)

RETRY, SKIP, ABORT = "retry", "skip", "abort"


class SkipScenario(Exception):
    pass


class AbortRun(Exception):
    pass


class Console:
    def __init__(
        self,
        vm_name: str,
        logfile=None,
        timeout: int = 900,
        supervisor=None,
        initramfs_recovery=None,
    ):
        import pexpect  # imported lazily so the package loads without it

        self._pexpect = pexpect
        self.vm_name = vm_name
        self.supervisor = supervisor
        # shell commands that bring the root online when a boot drops to the
        # initramfs rescue shell (eg. a degraded btrfs root needs mount -o
        # degraded). empty -> a drop is treated as an unrecoverable failure.
        self.initramfs_recovery = list(initramfs_recovery or [])
        # how many times a boot dropped to the initramfs and was recovered by the
        # follow-through. reported as a WARN: a boot needing manual intervention
        # (the recovery commands) is a caveat, not a clean unattended boot.
        self.initramfs_recoveries = 0
        self._timeout = timeout
        self._logfile = logfile or sys.stdout
        self._spawn()

    def _spawn(self) -> None:
        self.child = self._pexpect.spawn(
            f"virsh console {self.vm_name} --force",
            encoding="utf-8",
            codec_errors="replace",
            timeout=self._timeout,
        )
        self.child.logfile_read = self._logfile
        self.child.sendline("")  # past the "Escape character is ^]" banner

    def reconnect(self) -> None:
        """drop and re-open the serial console. across a guest reboot the live
        connection cannot reliably coax the serial getty into printing its login
        prompt (a CR on the stale connection is ignored); a fresh `virsh console`
        re-asserts carrier so a CR surfaces the prompt again."""
        try:
            self.child.sendcontrol("]")
            self.child.close()
        except Exception:
            pass
        self._spawn()

    def _expect(self, patterns, timeout=None) -> int:
        """wait for one of `patterns`; on timeout/eof defer to the supervisor."""
        while True:
            try:
                return self.child.expect(patterns, timeout=timeout)
            except (self._pexpect.TIMEOUT, self._pexpect.EOF) as exc:
                if not self.supervisor:
                    raise
                action = self.supervisor.on_stuck(self, patterns, exc)
                if action == RETRY:
                    continue
                if action == SKIP:
                    raise SkipScenario(f"operator skipped while waiting for {patterns!r}")
                raise AbortRun("operator aborted the run")

    def recent(self, n: int = 2000) -> str:
        return (self.child.before or "")[-n:]

    def unlock(self, password: str, timeout: int = 600) -> None:
        """answer the boot-time cryptsetup passphrase prompt."""
        self._expect(UNLOCK_PROMPTS, timeout=timeout)
        self.child.sendline(password)

    def login(self, user: str, password: str | None, timeout: int = 600) -> None:
        self.child.sendline("")
        self._reach_login(timeout)  # boot + initramfs + surface a login: prompt
        self._submit_credentials(user, password, timeout)
        self._init_shell()

    def _submit_credentials(self, user: str, password: str | None, timeout: int) -> None:
        """drive login: -> Password: -> shell, responding to whatever prompt
        actually appears rather than assuming a fixed order. surfacing the serial
        getty leaves a stray CR that agetty reads as an empty username, so a fixed
        user-then-password sequence desyncs (the password lands as the next
        username and login times out 60s at a time). reacting to each prompt --
        username on login:, password on Password:, done on a shell prompt --
        converges in seconds through that artifact and any 'Login incorrect'."""
        self.child.sendline(user)
        for _ in range(max(2, timeout // LOGIN_NUDGE_INTERVAL)):
            try:
                idx = self.child.expect(
                    [r"[#$] $", r"[Pp]assword: *$", r"login: *$"], timeout=LOGIN_NUDGE_INTERVAL
                )
            except self._pexpect.TIMEOUT:
                self.child.sendline(user)  # re-offer the username; also nudges the getty
                continue
            except self._pexpect.EOF as exc:
                raise AbortRun("serial console closed during login") from exc
            if idx == 0:
                return
            self.child.sendline((password or "") if idx == 1 else user)
        raise AbortRun("could not complete login over serial")

    def _reach_login(self, timeout: int = 600, max_recoveries: int = 3) -> None:
        """wait for the login prompt, coaxing the serial getty and following
        through any drop to the initramfs rescue shell.

        a degraded boot detours through the initramfs follow-through (a btrfs root
        needs mount -o degraded), after which the serial getty comes up but will
        not print its login prompt over the now-stale console connection. so on
        each poll with no prompt, reconnect (a fresh `virsh console` re-asserts
        carrier) and send a bare CR (what ENTER sends): a fresh connection plus a
        CR reliably surfaces the prompt where a CR alone does not. bounded so a
        boot that reaches neither prompt is graded, not hung. covers the initial
        boot, scenario reboots, and the rescue flow."""
        recoveries = 0
        for _ in range(max(1, timeout // LOGIN_NUDGE_INTERVAL)):
            try:
                idx = self.child.expect(
                    [r"login:\s*$", INITRAMFS_PROMPT], timeout=LOGIN_NUDGE_INTERVAL
                )
            except self._pexpect.TIMEOUT:
                self.reconnect()  # fresh carrier; a CR on the stale link is ignored
                self.child.send("\r")
                continue
            except self._pexpect.EOF as exc:
                raise AbortRun("serial console closed while waiting for login") from exc
            if idx == 0:
                return
            self._recover_in_initramfs()  # resumes the boot; getty handled below
            recoveries += 1
            if recoveries > max_recoveries:
                raise AbortRun("boot kept dropping to the initramfs rescue shell")
        raise AbortRun("login prompt never appeared after coaxing the serial getty")

    def _recover_in_initramfs(self) -> None:
        if not self.initramfs_recovery:
            raise AbortRun(
                "boot dropped to the initramfs rescue shell and no recovery is configured"
            )
        self.initramfs_recoveries += 1
        for cmd in self.initramfs_recovery:
            self.child.sendline(cmd)
            # the prompt usually reappears, but a command that brings the root
            # online can let the boot resume with no further prompt -- don't hang
            # waiting for one. a bounded wait keeps us in sync when it returns and
            # moves on when it does not.
            try:
                self.child.expect(INITRAMFS_PROMPT, timeout=RECOVERY_CMD_TIMEOUT)
            except (self._pexpect.TIMEOUT, self._pexpect.EOF):
                break
        self.child.sendline("exit")  # resume the boot if still in the shell

    def become_root(self, password: str | None, timeout: int = 120) -> None:
        self.child.sendline("sudo -i")
        if self._expect([r"[Pp]assword:", r"[#$] $"], timeout=timeout) == 0:
            self.child.sendline(password or "")
            self._expect(r"[#$] $", timeout=timeout)
        self._init_shell()

    def _init_shell(self) -> None:
        self.child.sendline(f"export PS1='{SHELL_PROMPT}'")
        self._expect(re.escape(SHELL_PROMPT))
        self.child.sendline("stty -echo 2>/dev/null; export TERM=dumb")
        self._expect(re.escape(SHELL_PROMPT))

    def run(self, cmd: str, timeout: int = 1800, check: bool = False) -> tuple[int, str]:
        """run a command, returning (exit_code, output)."""
        self.child.sendline(f"{cmd}; echo {_MARK}$?{_MARK}")
        self._expect(_RC_RE, timeout=timeout)
        rc = int(self.child.match.group(1))
        out = self.child.before
        self._expect(re.escape(SHELL_PROMPT))
        if check and rc != 0:
            raise RuntimeError(f"command failed (rc={rc}): {cmd}\n{out}")
        return rc, out

    def reboot(self) -> None:
        self.child.sendline("reboot")

    def close(self) -> None:
        try:
            self.child.sendcontrol("]")
            self.child.close()
        except Exception:
            pass
