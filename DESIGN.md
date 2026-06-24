# design

## model

raiden is an orchestrator. it computes a plan of system commands from a typed
config and executes them (or prints them under `--dry-run`). it does not
reimplement sgdisk, mdadm, cryptsetup, debootstrap, grub, debugfs, zpool, or
btrfs; it drives them.

the bash predecessor recomputed a large set of derived shell variables on every
script invocation and dispatched to per-stack scripts by filesystem convention.
raiden replaces that with three typed layers:

1. `Config` (src/config.rs) -- install-time input, deserialized from TOML and
   merged with environment and flag overrides.
2. `Layout` (src/layout.rs) -- pure functions deriving device paths, esp/boot
   members, mount points, and crypt names from a `Config`. this is the typed
   replacement for the old `options.sh`.
3. `State` (src/state.rs) -- the resolved truth written at install time to the
   manifest: the resolved `Config`. post-install operations read this instead of
   a hand-maintained config; devices are re-derived from it at run time (blkid,
   fstab), so no per-disk UUIDs are stored.

## config vs state

- `raiden.toml` is what you want at install time. only `install` consumes it.
- the manifest (`/etc/raiden/state.toml`, mirrored to /boot) is what was built.
  `status`, `scrub`, `rescue`, `replace`, and `close` resolve from it, so they
  never depend on a config that matches install time. this removes the sharpest
  edge of the bash version, which required editing config.sh to match install.
  the install pipeline writes the manifest into the target during the finish
  phase (while /mnt is still mounted), so it survives into the installed system.
  the same phase copies the running binary to `/usr/local/sbin/raiden` in the
  target, so those ops have something to run after reboot (the manifest alone is
  useless without the tool that reads it). the distributed binary is a static
  musl build, so it runs there with no shared libs.

precedence for install-time config, lowest to highest: defaults, file, env,
flags.

## init

`raiden init` (src/init.rs) writes a starter `raiden.toml` so onboarding on real
hardware is one command rather than hand-editing an example. it discovers whole
disks via `lsblk` and tags the removable/live medium (the disk backing `/` or a
live mountpoint, read from /proc/mounts) so the installer is never offered as a
target; detects efi vs bios from `/sys/firmware/efi`; and derives the partition
prefix from the kernel rule (a `p` separator when the disk name ends in a digit:
`nvme0n1p1` vs `sda1`). the crypt block is selected per stack to match the
examples/ catalog -- aead (aegis128) for the md/lvm ext4 and xfs stacks, plain
aes-xts where integrity is provided elsewhere (zfs/btrfs/bcachefs checksum their
own data; dm-integrity sits below md) -- so a generated config is correct out of
the box rather than inheriting the aead default everywhere. the assembled config
is run through the same `Config::validate` before writing, and the default raid
level is chosen to fit the member count so the result always validates. it is
interactive by default (each prompt pre-answerable by a flag) and falls back to
the detected defaults under `--non-interactive` or a non-tty stdin, which is how
the e2e suite drives it without real disks.

the same generator (`init::generate`, which resolves stack/level/members/release
from the env+flag overrides with `init` reading detected defaults) backs a
config-less `raiden install`: when no config file is present, install generates a
machine-appropriate config in memory and provisions from it, so one command takes
a bare live environment to an installed system. a config file, when present, stays
authoritative (only `install` consumes it). install then confirms the erase,
naming the member disks (R14), unless `--yes` -- which the harness and the
unattended one-liner pass. distribution is a fully static musl binary (`make
dist`, published as the github release asset that `install.sh` fetches), so the
live environment needs no rust toolchain.

two shell entrypoints sit in front of this: `install.sh` downloads the binary,
and `livecd.sh` is the guided driver (install screen, fetch raiden, run init under
a screen session that survives a disconnect). `livecd.sh` also exposes thin
`init`/`install`/`rescue` subcommands (binary located via `RAIDEN_BIN`); the vm
harness stages it and drives raiden through them, so the live-phase raiden
invocation is defined once and the tested flow is the live flow.

## stacks

a `Stack` (src/stack/mod.rs) is a trait, one implementation per supported
combination. it contributes the stack-specific packages and the per-phase
command steps (partition root, format root, finish, status, scrub). generic
steps shared by every stack -- gpt partitioning, /boot (see below), esp
creation -- live alongside the trait so they are written once. type-safe
dispatch replaces the old symlink-and-probe convention.

stacks are identified by the same `~` strings as raid-explorations for
continuity: `dm-crypt~btrfs`, `dm-crypt~zfs`, `dm-crypt~md~lvm~ext4`,
`dm-integrity~md~dm-crypt~lvm~ext4`. the rewrite adds two more: `dm-crypt~md~lvm~xfs`
(the ext4 stack parameterized by root filesystem -- they share all crypt/md/lvm/
replace/rescue logic and differ only in mkfs + the fstab line) and the
experimental `dm-crypt~bcachefs` (per-disk dm-crypt + multi-device bcachefs,
redundancy by replica count). bcachefs is out-of-tree: the `Stack::apt_repos` hook
adds apt.bcachefs.org (key + deb822 source) on the host and target, pinned below
debian so only the dkms module comes from it, and the module builds against the
running kernel like zfs. it is not currently installable on forky (the repo's
tools lag forky's libsodium), so it is plan-validated but not vm-tested.

each stack writes its own root fstab entry mounted at `/`. the ext4 stacks use a
static line by the stable lvm path (`/dev/vg0/root`); btrfs captures the live
mount options (`fstab_root_btrfs`) but rewrites the device as the filesystem uuid
(blkid) and the mountpoint as `/` -- the live mount is at `/mnt` during install,
so a verbatim capture would record `/mnt` and leave the booted system with no rw
`/` entry, so `systemd-remount-fs` keeps root read-only. zfs needs no fstab root
entry (the pool mounts it). all are uuid- or stable-path-addressed (R8).

## pipeline and steps

a `Step` (src/step.rs) is one action: run a command, write a file, or append to
a file. a run step records whether it enters the `/mnt` chroot, feeds the
encryption password on stdin, or is best-effort (failure tolerated). the password
is never printed or logged; dry-run shows a placeholder. file edits that need a
runtime value (eg. a uuid from blkid) are emitted as `sh -c`; static files are
native write steps.

the install pipeline (src/pipeline.rs) assembles phases as reusable builders --
apt, prepare, reset, partition, format, mount, strap, install, bootloader,
finish, close -- so operations (src/ops.rs: rescue, mount, replace, remove, close,
scrub, status) compose the subset they need. `--from`/`--only` restrict the
install to a phase; `--list-phases` prints them.

the `reset` phase makes install re-runnable: before partitioning it tears down
any prior stack on the members (the same best-effort `teardown_steps` that
`close` uses -- unmount, then lvm/md/crypt or the zpool) and `udevadm settle`s,
so wipefs does not hit "Device or resource busy" while a half-finished run still
holds the disks. the md-backed stacks also sweep `/sys/block/<dev>/holders` of
their array members (the crypt devices for md~lvm, the dm-integrity devices for
the integrity stack), so an array assembled under a non-canonical node (md127,
from a hand-create or a prior boot's auto-assembly) is stopped too -- not only
`/dev/md/root` by name. arrays built on disks outside the configured members are
left alone.

the runner (src/step.rs) prints the plan under `--dry-run` or executes it,
streaming command output. status (src/bad_files.rs) ports the md read-error to
file-path mapping (raid-stripe geometry + dmsetup offsets + debugfs); `status
--bad-files` narrows the output to just that affected-file listing.

benchmark (src/benchmark.rs) is the fsync-bound `sysbench fileio` workload (ported
from the harness's old guest script so it lives in one place). like status it does
not fit the plain run-and-stream model: `--dry-run` prints it as a plan, but a real
run captures each pass, parses the durable-write metrics, and prints a per-mode
summary (or `--format json`). the vm harness invokes `raiden benchmark` rather than
shipping a script -- the binary is staged into the target, so it is present.

## resume

every execution is checkpointed (src/checkpoint.rs): after each successful step,
the (phase, step) cursor is written to `/var/lib/raiden/checkpoint.toml` with the
operation name and a config fingerprint. a failed run leaves the last good cursor
on disk; `--resume` validates the operation and config still match, then skips
every already-applied step and continues from the next one. resume is
step-granular by design -- it never re-runs a completed (and possibly
destructive) step.

## esp mirroring

for efi, every disk carries its own bootable esp so the firmware can boot from
any survivor. the first member's esp is mounted at `/boot/efi` (by uuid,
`nofail`); the others are mirrors with no persistent mount point. on each
update-grub the grub.d hook (`90_copy_to_efi_mirrors`, generated by
`stack::efi_mirror_hook`) resyncs every other member's esp from the live
`/boot/efi`, mounting each one transiently under `/run/raiden` for the rsync and
unmounting it again -- so neither `/` nor `/boot` carries a per-disk esp mount,
and the mirrors stay cold (corruption exposure limited to the one mounted esp).
the scripts are baked with the member device list rather than reading fstab,
since the mirrors have no fstab entries. a lost primary is recovered by `replace`
(rebuilds the disk in place, preserving the esp uuid so `/boot/efi` mounts again),
which the hook then re-populates onto every mirror.

`replace` preserves the rebuilt disk's identifying uuids so the baked
fstab/crypttab entries stay valid: the primary member's esp is recreated with its
original vfat uuid (read from the `/boot/efi` fstab entry, applied via
`mkfs.msdos -i`) so `/boot/efi` still mounts; a non-primary esp is a mirror with
no fstab entry, so it gets a fresh uuid and is re-populated by the hook. the luks
header is re-stamped with its original uuid (read from `/etc/crypttab`, applied
via `cryptsetup luksUUID` after `luksFormat`). without the latter a reboot after a
replace would drop to the initramfs -- the replaced members would never unlock,
so the array could not assemble. these uuids are read from the running system, so
`replace` is run from the booted target.

## boot: independent (default) vs md array

by default `/boot` is NOT a raid array. each disk carries its own ext4 `/boot` on
p2, and every copy is given the SAME fs uuid at format time (`mkfs.ext4 -U`; the
first member's uuid, read back with blkid, is reused for the rest). grub-mkconfig
writes `search --fs-uuid <UUID>` into grub.cfg, and grub-install `--removable`
bakes a self-contained grub onto each disk, so each disk's grub finds its own
local `/boot` and boots with no array to assemble. this is what lets a system
survive losing the first disk -- the failure that a degraded md `/boot` could not
recover from (grub `no such device`). it also makes `replace` a plain mkfs+rsync
instead of an mdadm re-add.

because the live `/boot` is mounted by the shared uuid, it can land on any disk,
including one being replaced -- `mkfs` would then refuse ("device is mounted"). so
`replace` first remounts `/boot` from a survivor if it currently sits on a target
disk, and `udevadm settle`s after the crypt/dm teardown before reformatting (udev
frees the just-closed devices asynchronously, else mkfs/luksFormat hit "busy").

the live `/boot` mounts by the shared uuid (`nofail`); any surviving copy is
correct since they are identical. the non-primary copies have no persistent mount
point -- the sync mounts each one transiently under `/run/raiden` (with `rsync
--one-file-system`, so it never recurses into the esp mount under `/boot`). they
are addressed by DEVICE, not uuid, so the sync writes each physical disk rather
than whatever the shared uuid resolves to first.

syncing is a standalone script (`raiden-sync-boot-mirrors`, src/stack/mod.rs),
not a grub.d hook: grub-mkconfig writes grub.cfg to a temp file and moves it into
place only after the grub.d scripts run, so a grub.d hook would mirror a stale
grub.cfg. instead it runs from the kernel `postinst.d`/`postrm.d` hooks (named to
sort after `zz-update-grub`) and once, strictly, at install. the install-time sync
runs in the finish phase, AFTER `update-initramfs` builds the crypttab-aware
initrd -- syncing earlier (eg. in the bootloader phase, before crypttab exists)
would ship a cryptsetup-less initrd to the mirrors, so a survivor booted after the
primary's /boot is lost could not unlock the encrypted root. `--strict` makes a
present-but-unwritable mirror a hard error; the kernel hooks pass a kernel version
instead, staying best-effort so they never fail a package upgrade. it rsyncs with
`--one-file-system` to stay within the `/boot` filesystem.

`boot.raid = true` selects the legacy path instead: one md raid1 array across all
p2 members, mounted by `/dev/md/boot`, scrubbed and rebuilt with mdadm. the mode
is carried as `BootMode` on `Layout`, so the pipeline/ops branch on a single
`layout.boot_raid()` and the raid path is unchanged.

## password and serial console

the password is read interactively (echo off) by default, or non-interactively
from `--password-file` / `RAIDEN_PASSWORD` for unattended runs and the vm
harness. `install.serial_console` writes grub serial settings and enables a
serial getty so the installed system's boot menu, kernel, initramfs unlock
prompt, and login all reach ttyS0 -- useful for headless servers and required by
the harness.

## module layout

```
src/
  main.rs       entry point, command dispatch, confirmation + password resolution
  cli.rs        clap definitions and global flags
  config.rs     TOML config structs, load, override merge, validate
  init.rs       `raiden init`: discover disks + generate a starter config
  layout.rs     derived device/mount layout (pure)
  state.rs      install manifest (full config + discovered uuids)
  step.rs       Step actions, the runner (dry-run + checkpointed execution)
  checkpoint.rs resume cursor + config fingerprint
  prompt.rs     interactive password (echo off) and confirmation
  pipeline.rs   install pipeline assembled as reusable phase builders
  ops.rs        rescue, mount, replace, remove, close, scrub, status (compose phases)
  bad_files.rs  md read-error to file-path mapping (status / status --bad-files)
  stack/
    mod.rs      Stack trait, selection, shared steps, static hooks
    md_lvm_ext4.rs / btrfs.rs / zfs.rs / md_integrity.rs
```

## testing

`tests/` holds the fast, hermetic e2e suite (python/pytest): planning,
validation, and resume via `--dry-run`, runnable anywhere with `make test-e2e`.

`examples/` is a per-stack catalog of complete, valid configs (one per stack),
named like raid-explorations' explorations/ (`<stack>.<level>[.<variant>].toml`,
eg. `dm-crypt~zfs.raidz2.toml`). it serves two purposes: a ready-to-edit starting
point for users, and the fixtures the vm harness installs. each example pairs the
stack with its correct crypt settings -- aead + aegis128 for the recommended ext4
stack, plain aes-xts for zfs/btrfs/dm-integrity (where integrity is provided by
the filesystem or by dm-integrity below md). the harness loads the matching file
(or an explicit `--config`), parses it, and overlays only the test-specific keys
(serial console on, the vm's member disks, the `/boot` mode, the scenario
packages) before staging it -- so the thing under test is the same config a user
would write, and the rendered config is embedded at the top of the report. the
e2e suite asserts every example validates and plans, guarding the catalog.

`tests/vm/` holds the automated libvirt vm harness (python), modeled on
raid-explorations' `make recreate`: one transient, uniquely-named domain with raw
disks plus the live iso, booted via a per-device boot order (disks first, cdrom
fallback). the short live/install phase is driven with `virsh send-key` but still
observed -- the guest writes its result to the virtiofs share and powers off, and
the host reads it and watches domain state. the installed system enables a serial
console, so every post-install phase (unlock, login, the resilience scenarios,
their reboots) is driven over **serial** with pexpect, waiting on real console
state with no timers. unexpected serial state pauses for the operator in
`--interactive` mode; unattended runs grade it and continue. it produces a graded
markdown report (with a small reproduction table of the installed config at the
top). see tests/vm/README.md.

when a boot drops to the initramfs rescue shell (the root cannot be mounted), the
console driver follows through generically: it detects the `(initramfs)` prompt
in place of `login:`, runs the stack's configured recovery commands, and `exit`s
to resume the boot -- covering the initial boot, scenario reboots, and the rescue
flow alike. this is required by btrfs: a multi-device btrfs root refuses to mount
once a member is faulty (after the corruption/truncate scenarios) and needs a
manual `mount -o degraded` from a surviving member. md and zfs assemble degraded
arrays automatically and need no recovery (an empty command list). a drop with no
configured recovery is graded as an unrecoverable failure.

a reboot that only reached login because the follow-through intervened is graded
WARN, not PASS: it did not boot unattended (operator intervention was required).
this is stack-agnostic -- any scenario whose boot needs the recovery commands gets
the warning -- so btrfs's degraded reboots warn while md/zfs (which auto-assemble)
pass cleanly. the overall report stays INCOMPLETE until the run reaches the end of
its flow, so a report flushed by a killed, hung, or aborted run is never mistaken
for a clean pass; WARNs do not fail the run (only FAILs do).

reaching the login prompt over serial after a degraded boot needs care. the
serial getty prints its prompt only after a carriage return arrives on the line,
and over the console connection that survived the guest reboot a CR is ignored
(only a fresh connection re-asserts the carrier the getty waits on). so when no
prompt appears, `_reach_login` reconnects (`virsh console` afresh) and sends a
bare CR, polling until the login prompt -- or the `(initramfs)` shell -- appears,
bounded so a boot that reaches neither is graded rather than hung. the initramfs
follow-through itself is bounded too: a recovery command that brings the root
online can let the boot resume with no further `(initramfs)` prompt, so the wait
for that prompt times out and moves on instead of blocking. these are the only
timed polls in the otherwise event-driven serial flow, because a silent getty and
a resumed boot each leave nothing to wait on.

## dependencies

kept minimal: clap (cli), serde + toml (config/state), anyhow (errors), regex
(dmesg parsing for the status mapping). external system tools are invoked via
std::process, not wrapped in crates.
