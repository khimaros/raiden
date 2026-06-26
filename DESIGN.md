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
3. `Manifest` (src/manifest.rs) -- the resolved truth written at install time to the
   manifest: the resolved `Config`. post-install operations read this instead of
   a hand-maintained config; devices are re-derived from it at run time (blkid,
   fstab), so no per-disk UUIDs are stored.

## config vs manifest

- `raiden.toml` is what you want at install time. only `install` consumes it.
- the manifest (`/boot/raiden/manifest.toml`, canonical, mirrored to
  `/etc/raiden/manifest.toml`) is what was built. `status`, `scrub`, `rescue`,
  `replace`, and `close` resolve from it, so they never depend on a config that
  matches install time. this removes the sharpest edge of the bash version, which
  required editing config.sh to match install. the install pipeline writes the
  manifest into the target during the finish phase (while /mnt is still mounted),
  so it survives into the installed system. /boot is the canonical load path
  because it is reachable from a livecd by mounting a member's /boot alone,
  without unlocking the root fs; load tries /boot first, then /etc. the same
  phase copies the running binary to `/usr/local/sbin/raiden` in the target, so
  those ops have something to run after reboot (the manifest alone is useless
  without the tool that reads it). the distributed binary is a static musl
  build, so it runs there with no shared libs.

precedence for install-time config, lowest to highest: defaults, file, env,
flags.

## init

`raiden init` (src/init.rs) writes a starter `raiden.toml` so onboarding on real
hardware is one command rather than hand-editing an example. it discovers whole
disks via `lsblk` and tags the removable/live medium (the disk backing `/` or a
live mountpoint, read from /proc/mounts) so the installer is never offered as a
target; detects efi vs bios from `/sys/firmware/efi`; and derives the partition
prefix from the kernel rule (a `p` separator when the disk name ends in a digit:
`nvme0n1p1` vs `sda1`). the prefix is derived per disk in `Layout::part`, not
stored in the config, so a single array can mix nvme and sd members. the crypt
block is selected per stack to match the
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
any survivor. every esp shares one vfat fs uuid (the first member's, stamped
onto the rest at format time, mirroring the /boot design), so the single
`/boot/efi` fstab entry (by uuid, `nofail`) resolves to any survivor if the
primary is lost. the first member's esp is the one mounted at `/boot/efi`; the
others are mirrors with no persistent mount point. on each update-grub the grub.d
hook (`90_copy_to_efi_mirrors`, a thin wrapper generated by
`stack::EFI_MIRROR_WRAPPER`) execs `raiden sync efi --yes`, which resyncs
every other member's esp from the live `/boot/efi`, mounting each one transiently
under `/run/raiden` for the rsync and unmounting it again -- so neither `/` nor
`/boot` carries a per-disk esp mount, and the mirrors stay cold (corruption
exposure limited to the one mounted esp). the wrapper swallows the exit code
(`|| true`) so a verify or mirror failure can never abort grub-mkconfig and block
grub.cfg regeneration. at install the grub.d hook no-ops (raiden is staged into
the target only in the finish phase, after the bootloader phase's update-grub),
so the finish phase runs `raiden sync efi --yes` explicitly. a lost primary is
recovered by `replace` (rebuilds the disk in place, re-stamping the shared esp
uuid so `/boot/efi` mounts again), which `sync efi` then re-populates onto every
mirror. firmware boot is unaffected by the shared uuid: `efibootmgr` targets a
specific disk+partition, not a uuid.

`replace` preserves the rebuilt disk's identifying uuids so the baked
fstab/crypttab entries stay valid: every rebuilt esp is recreated with the shared
vfat uuid (read from the `/boot/efi` fstab entry, falling back to any survivor's
blkid, applied via `mkfs.msdos -i`) so `/boot/efi` still mounts. the luks
header is re-stamped with its original uuid (read from `/etc/crypttab`, applied
via `cryptsetup luksUUID` after `luksFormat`). without the latter a reboot after a
replace would drop to the initramfs -- the replaced members would never unlock,
so the array could not assemble. these uuids are read from the running system, so
`replace` is run from the booted target.

`replace --disks=a --with=c` is a physical disk swap: `a` is detached
(best-effort -- it may be gone) and never wiped, `c` is wiped + provisioned +
re-added, and the manifest's `members` is mutated (`a`->`c`, paired by position;
the primary stays primary). the new disk adopts the old disk's esp/luks identity
(the shared esp uuid, the shared /boot uuid) so fstab stays valid. the
crypt mapper names change with the member names, so `/etc/crypttab` is
regenerated (the per-member crypt stacks rewrite it from the swapped layout;
md_integrity's crypttab references the md array uuid, unchanged by a member
swap). the old luks uuid is NOT preserved on a swap (the crypttab rewrite uses
the fresh uuid from the new `luksFormat`). without `--with`, `replace` is the
in-place rebuild above (backward compatible).

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

syncing is `raiden sync boot` (src/sync.rs), not a grub.d hook: grub-mkconfig
writes grub.cfg to a temp file and moves it into place only after the grub.d
scripts run, so a grub.d hook would mirror a stale grub.cfg. instead it runs
from the kernel `postinst.d`/`postrm.d` hooks (inline scripts named to sort
after `zz-update-grub`) that exec `/usr/local/sbin/raiden sync boot --yes`, and
once at install (`raiden sync boot --yes`, run in the finish-phase chroot). the install-time sync
runs AFTER `update-initramfs` builds the crypttab-aware initrd -- syncing earlier
(eg. in the bootloader phase, before crypttab exists) would ship a
setup-less initrd to the mirrors, so a survivor booted after the primary's
/boot is lost could not unlock the encrypted root. before syncing, `sync boot`
verifies the source /boot (read-only `e2fsck`, grub.cfg/kernel/initrd presence,
and an initrd that contains cryptsetup) and bails on any failure, so a broken
/boot is never propagated. verification is on by default for every caller;
`--force` disables it (used by no script). per-mirror sync is best-effort-continue:
a failed mirror is reported and counted, and the exit code is non-zero iff any
mirror failed. the boot `postinst.d` wrapper propagates that exit code (a degraded
boot mirror should block the kernel upgrade); the efi `grub.d` wrapper swallows
it (never block grub.cfg). `sync boot` rsyncs with `--one-file-system` to stay
within the `/boot` filesystem; `sync efi` does not (there is no nested mount
under /boot/efi). the two are split because /boot must run after zz-update-grub
and the esp resync runs during update-grub -- a single command cannot serve both
without reintroducing the stale-grub.cfg bug.

`boot.raid = true` selects the legacy path instead: one md raid1 array across all
p2 members, mounted by `/dev/md/boot`, scrubbed and rebuilt with mdadm. the mode
is carried as `BootMode` on `Layout`, so the pipeline/ops branch on a single
`layout.boot_raid()` and the raid path is unchanged.

## recovery (raiden recover at boot)

when a degraded boot drops to the initramfs rescue shell (the root cannot be
mounted -- eg. a multi-device btrfs/bcachefs root with a faulty member refuses to
mount without `-o degraded`), `raiden recover` brings it online so the boot can
continue. it generalizes the per-stack manual `(initramfs)` commands into one
command, structured as check/fix like doctor: it observes whether the root is
mounted at the target (default `/root`, the initramfs convention where `init`
expects `$rootmnt`), and if not runs the stack's recovery actions, each confirmed
via `prompt::confirm_or_yes` (`--yes` is the escape hatch). the postcondition is
the root actually mounted; per-step errors are tolerated (assemble/import steps
are best-effort, a mount that lost the race to a sibling member is harmless), so
the verdict is the mount, not any one command.

the crypt members are already open by the initramfs (cryptroot + decrypt_keyctl),
so the actions (`Stack::recover_actions`) pick up at the array/mount layer: md/lvm
and dm-integrity `mdadm --run` + `vgchange -ay` + mount `/dev/vg0/root` (md
auto-assembles; `--run` kicks a stalled degraded array); btrfs/bcachefs `mount -o
degraded` from a surviving member; zfs `zpool import -f` + `zfs mount` (rarely
needed -- zfs auto-imports degraded).

for the command to exist at the rescue shell, install bakes the static raiden
binary + the manifest into the initrd via a raiden initramfs hook (the one
raiden-added hook -- no stock hook pulls them in, unlike the stack tooling). the
hook `copy_exec`s the binary to `/sbin` and copies the manifest to `/etc/raiden`
inside the initrd, where `Manifest::load` finds it with neither `/boot` nor the
root mounted. install rebuilds the initrd once more after staging the binary +
manifest, then mirrors that recovery-bearing initrd to every `/boot`. the feature
is config-guarded by `install.initramfs_recovery` (default on). install and
doctor's `--fix` establish the bundle through the SAME shared steps
(`stack::raiden_recovery_hook_step` parameterized by root, `stack::
update_initramfs_u` parameterized by chroot), so the establish and the verify
cannot drift. doctor checks both halves when the feature is on: `recover hook` (the
hook is installed + executable, so a future rebuild keeps baking raiden in -- a
removed hook would silently drop it, like the mirror hooks) and `recover` (the
initrd currently carries raiden + the manifest).

## password and serial console

the password is read interactively (echo off) by default, or non-interactively
from `--password-file` / `RAIDEN_PASSWORD` for unattended runs and the vm
harness. `install.serial_console` writes grub serial settings and enables a
serial getty so the installed system's boot menu, kernel, initramfs unlock
prompt, and login all reach ttyS0 -- useful for headless servers and required by
the harness.

## doctor

`raiden doctor` is a bolt-on (hand-written checks, not declarative) that walks
the installed system and reports the state of each layer the manifest says
should be present, resolving config from the manifest like the other post-install
ops. each check carries the exact reasoning the others cannot: disk presence, boot
+ esp mounts (and a note when they sit on different disks -- expected and benign,
since both /boot and /boot/efi mount by a shared fs uuid), fstab, the shared-uuid
invariant (every member's /boot carries one ext4 uuid and every esp one vfat uuid,
matching the fstab entry -- a divergent member silently cannot serve the mount if
the primary is lost), crypttab, luks headers, raid/zfs/btrfs status, boot +
esp mirror presence AND drift (a present-but-never-synced mirror is a silent
failure -- the esp grub.d hook swallows exit codes) AND bootloader (each member
esp/boot is transient-mounted read-only and verified to independently carry its
bootloader -- shim+grub / grub.cfg+kernel+initrd -- since under the shared uuid the
mount can land on any member, and the drift check, which trusts the source, cannot
catch a mounted-but-broken member), grub install (on whichever
esp is actually mounted, since the shared uuid lets /boot/efi land on any
survivor), the initrd
(it carries every binary needed to unlock and mount/recover the root: the
decrypt_keyctl keyscript + keyctl, cryptsetup, and the stack's assemble/mount tools
-- the per-stack list from Stack::initramfs_binaries, the same source an install
postcondition can use), the boot-mirror kernel hooks (postinst.d/postrm.d) AND the esp-mirror
grub.d hook (both checked for presence AND the executable bit, since run-parts and
grub-mkconfig silently skip non-executable scripts), the per-disk efibootmgr
entries (each member should have exactly one boot entry loading shim from its own
esp, matched by the esp PARTUUID -- distinct from the fs uuid, so a uuid re-stamp
needs no efibootmgr change -- with duplicate/shim-bypassing-grub cruft flagged),
and the manifest. exit 0
iff every check is ok/fixed, non-zero if any fail. every check is best-effort, so
a dead disk fails its own check and the rest still run.

raiden owns the nvram boot entries: the install preseeds `grub2/update_nvram=false`
so grub's postinst does not also add entries on every upgrade (the source of
accumulated duplicate/shim-bypassing cruft), and registers one per-disk shim entry
itself via efibootmgr. `--fix` reconciles -- prune the stale/duplicate raiden
entries, register one clean shim entry per member -- leaving the removable
`EFI/BOOT/BOOTX64.EFI` fallback (the nvram-loss backstop) and non-member entries
untouched.

`--fix` repairs the auto-fixable checks in place: it installs the boot-mirror
kernel hooks and the esp-mirror grub.d hook (the static wrapper content from
`stack::BOOT_MIRROR_HOOK_CONTENT` / `EFI_MIRROR_WRAPPER`, 0755) when missing or
non-executable, re-runs `raiden sync boot`/`sync efi` to repair drifted mirrors,
and re-stamps any mirror whose fs uuid diverges from the shared one (the
legacy-host migration to shared esp/boot uuids -- `raiden doctor --fix` brings an
older install up to spec). the re-stamp is a reconcile: it re-observes live state
at apply time rather than trusting the check snapshot, requires the source mounted
(its uuid is the truth propagated), never touches the live source, and skips
already-shared mirrors; `/boot` is re-stamped in place (`tune2fs`), an esp is
reformatted with the shared volume id then repopulated by the following sync (its
content is reconstructible, as in `replace --esp`). each fix is confirmed
individually before it runs (the shared `prompt::confirm_or_yes`, so `--yes`
auto-accepts every fix the same way it does for `sync`); a declined fix is left in
place and noted. `--fix --dry-run` previews each repair instead -- the exact
commands and target devices (the `restamp_argv` form, no prompts, nothing
written) -- so the destructive re-stamp can be inspected before it runs. each applied fix is reported as a `fixed` status row; a fix that
errors leaves the original status and notes the error. drift and uuid divergence are warns (the system still boots from the
primary), never a fail. the check is split predicate/presentation -- a pure
`uuid_set_result` decides ok/warn + the fix from collected uuids, separate from
the I/O collection and the table -- the first step toward a check/fix split the
ops can reuse (see ROADMAP, idempotent install/replace/doctor). destructive checks
(fstab, crypttab, luks headers, array state, grub-install) are never auto-fixed --
they point at `replace`/`sync`/`grub-install` instead.

the mirror-drift checks and `raiden sync` share their transient-mount/compare
helpers (src/sync.rs: `mount_transient`, `unmount_transient`, `drift`, and the
`verify_boot`/`verify_efi` source checks), so doctor never re-implements the
mirror walk -- it asks `sync` for the source + mirror set, mounts each mirror
read-only, and runs `rsync --dry-run --itemize-changes` to list changed paths.

## module layout

```
src/
  main.rs       entry point, command dispatch, confirmation + password resolution
  cli.rs        clap definitions and global flags
  config.rs     TOML config structs, load, override merge, validate
  init.rs       `raiden init`: discover disks + generate a starter config
  layout.rs     derived device/mount layout (pure)
  manifest.rs    install manifest (full config + discovered uuids)
  step.rs       Step actions, the runner (dry-run + checkpointed execution)
  checkpoint.rs resume cursor + config fingerprint
  prompt.rs     interactive password (echo off) and confirmation
  pipeline.rs   install pipeline assembled as reusable phase builders
  ops.rs        rescue, mount, replace, remove, close, scrub, status (compose phases)
  recover.rs    `raiden recover`: bring a degraded root online from the initramfs (check/fix like doctor; runs Stack::recover_actions)
  efi.rs        shared EFI bootloader surface: the shim paths, the efibootmgr entry builder, the esp mkfs, and grub's debconf -- one canonical form used by install, replace, and doctor (so the establish/check/fix sites do not drift)
  sync.rs       `raiden sync boot`/`raiden sync efi`: independent /boot + esp mirror sync, with pre-sync verification; shared `mount_transient`/`unmount_transient`/`drift`/`initrd_has_cryptsetup` helpers
  doctor.rs     `raiden doctor`: installed-system health checks (read-only; `--fix` reaches parity with install/replace -- it repairs hooks (boot/esp mirror + the raiden recovery hook), mirror sync/drift, uuid re-stamp, efibootmgr, grub debconf, fstab, crypttab, grub-install, initrd, and luks-header backup -- by building and running the SAME establish steps install/replace use)
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
in place of `login:`, runs `raiden recover --yes` (baked into the initrd), and
`exit`s to resume the boot -- covering the initial boot, scenario reboots, and the
rescue flow alike. this exercises `raiden recover` end to end: it is required by
btrfs/bcachefs (a multi-device root refuses to mount once a member is faulty, after
the corruption/truncate scenarios, and needs `mount -o degraded`), and is a no-op
for md/zfs (which assemble degraded automatically). a boot that needs the
follow-through is graded WARN (it did not come up unattended); one that recover
cannot bring online is an unrecoverable failure.

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
