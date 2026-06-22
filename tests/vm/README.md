# automated vm e2e harness

drives a real raiden install and the resilience scenarios in a libvirt/kvm vm,
grades each result, and writes a markdown report. modeled on raid-explorations'
`make recreate`, but automated with no human in the loop.

## how it boots (mirrors raid-explorations)

one transient, uniquely-named domain holds the raw member disks plus the live iso
as a cdrom, with a per-device boot order: disks first, cdrom last. blank disks
make the firmware fall through to the cdrom (installer); once raiden has installed
a bootloader the firmware boots the disks. no kernel extraction, no domain
recreation between install and boot.

## the two channels

- **live/install phase:** the stock live iso has no serial console, so this short
  phase is driven with `virsh send-key`: the harness waits briefly for the (fast)
  grub prompt, presses enter to boot the live entry, then types one command that
  runs raiden non-interactively, records a result on the virtiofs share, and
  powers off. completion is observed via the domain power state. the two coarse
  waits (`grub_seconds`, `live_boot_seconds`) are the only timing points and are
  tuned to the live iso (it boots quickly). raiden's output is tee'd, so it shows
  on the vm console (virt-viewer) and is also saved to the share (`live.log`);
  the harness itself logs each step it takes.
- **post-install phase:** raiden enables a serial console on the installed system
  (`serial_console`), so every later step -- the unlock prompt, login, the
  resilience scenarios, and their reboots -- is driven over the serial console
  with pexpect, waiting on real console state with no timers. the serial console
  works at stages ssh cannot reach (initramfs unlock, degraded boots, panics).

the harness logs what it is doing to stdout, and also shows the serial console
(post-install) and the live install output, with terminal control sequences
stripped. everything is saved to `console.log` in the run directory too. watch
the raw live console yourself with `virt-viewer <name>` or virt-manager.

the destructive 4/4 scenario recovers by booting the livecd cdrom-first and
running `raiden rescue` from the live environment.

## requirements

a host with kvm (`/dev/kvm`), libvirt (`virsh`), OVMF firmware
(`/usr/share/OVMF/OVMF_CODE_4M.fd`, `OVMF_VARS_4M.fd`), and a Debian live iso.
python deps (pexpect, pytest) are managed by `uv`. raw disk images and per-run
state live under `~/.local/share/libvirt/images/<name>/` (override with
`--image-dir`).

## run

```
cargo build --release
cd tests/vm
uv run python -m raiden_e2e.run --iso /abs/path/debian-live.iso \
    --stack dm-crypt~md~lvm~ext4 --out report.md
```

iso/binary paths resolve from the current directory; an absolute iso path is
safest. from the repo root you can also use `make test-vm ISO=/abs/path.iso`
(or `make analysis ISO=...` for the supervised variant), with optional
`SCENARIO=<name[,name]>` and `BOOT_RAID=1`.

by default /boot is an independent per-disk ext4 (synced by a hook); pass
`--boot-raid` to test the md raid1 /boot path instead.

to run only a subset of the post-install scenarios (faster iteration on one),
pass `--scenario` (repeatable or comma-separated); `--list-scenarios` prints the
selectable names. the install still runs first unless `--skip-install` is given.

```
uv run python -m raiden_e2e.run --list-scenarios
uv run python -m raiden_e2e.run --iso /abs/iso --scenario truncate_disks,corrupt_efiboot
make test-vm ISO=/abs/iso SCENARIO=corrupt_efiboot
```

`corrupt_efiboot` (boot/esp destruction) is excluded from the default run: bundled
after the other corruptions, a boot failure there would be confounded by
accumulated state rather than the boot damage. run it on its own clean install
with `make test-vm-boot ISO=/abs/iso` (equivalently `--scenario corrupt_efiboot`).

`--interactive` pauses for the operator on any unexpected serial-console state
and lets you choose when to retry, skip, or abort (watch/intervene with
`virt-viewer <name>` while you decide). `--keep` leaves the vm and images in
place. each run uses a distinct name and image dir, so runs can proceed in
parallel.

to iterate on the post-install scenarios without re-running the install, keep a
named run and reuse its disks:

```
uv run python -m raiden_e2e.run --iso /abs/iso --name dev --keep ...   # full run
uv run python -m raiden_e2e.run --iso /abs/iso --name dev --skip-install ...
```

if the grub prompt or live shell needs longer on your host, raise the timing in
config.py (`grub_seconds`, `live_boot_seconds`); the live iso boots fast, so the
defaults are small.

## what it grades

1. install (live, send-key) and first serial boot (unlock + login).
2. in-place scenarios, each recovered before the next: bitrot within redundancy,
   header damage, and whole-disk truncation -- graded for detect / survive /
   reboot / clean.
3. 4/4 corruption + `raiden rescue` from the livecd.
4. boot/esp destruction (`corrupt_efiboot`), run separately on a fresh install
   (`make test-vm-boot`) so its boot result is not confounded by the above.

## fast tests

the hermetic dry-run/validation/resume tests in `tests/` (`make test-e2e`) need
no vm. the harness logic here has unit tests too (`make test-vm-unit`).
