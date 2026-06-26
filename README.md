# raiden

raiden provisions and maintains full-disk-encrypted RAID systems on Debian
GNU/Linux. it installs a bootable, encrypted, redundant root filesystem from a
LiveCD, then stays on the system as the tool for status, scrub, rescue, and disk
replacement.

it is a rust rewrite of [raid-explorations](../raid-explorations), driven by a
TOML config with command line overrides and a persisted install manifest.

> WARNING: provisioning destroys all data on the configured disks. only run it
> against disks you intend to wipe.

## stacks

| identifier                          | encryption          | array    | filesystem |
| ----------------------------------- | ------------------- | -------- | ---------- |
| dm-crypt~md~lvm~ext4 (recommended)  | dm-crypt            | md       | ext4 (lvm) |
| dm-crypt~md~lvm~xfs                  | dm-crypt            | md       | xfs (lvm)  |
| dm-crypt~btrfs                      | dm-crypt            | btrfs    | btrfs      |
| dm-crypt~zfs                        | dm-crypt            | zfs      | zfs        |
| dm-integrity~md~dm-crypt~lvm~ext4   | dm-integrity+crypt  | md       | ext4 (lvm) |
| dm-crypt~bcachefs (experimental)    | dm-crypt            | bcachefs | bcachefs   |

> NOTE: with `dm-crypt~btrfs` (and `dm-crypt~bcachefs`), a degraded array (a
> failed/missing member) does not boot unattended -- the filesystem refuses a
> multi-device mount with a missing device. recover by running `mount -o degraded
> <member> /root` at the initramfs prompt to bring the system up, then `raiden
> replace` the disk. the md and zfs stacks assemble degraded arrays automatically.

> NOTE: `dm-crypt~bcachefs` is experimental and uses an out-of-tree dkms module
> from apt.bcachefs.org. it is not currently installable on Debian forky (testing):
> the repo's bcachefs-tools depend on libsodium23, which forky has superseded with
> libsodium26, and forky does not package bcachefs-tools itself. revisit when the
> repo catches up to forky's libraries.

## install on real hardware

boot a Debian live image (eg. `debian-live-*-amd64-standard.iso`); it autologins
as a sudo-capable user. then run, in one line:

```
wget https://raw.githubusercontent.com/khimaros/raiden/master/livecd.sh
bash livecd.sh
```

that is the whole flow. [livecd.sh](livecd.sh) elevates itself with `sudo`,
installs `screen`, downloads a prebuilt static `raiden` (no toolchain needed), and
opens a `screen` session that runs `raiden init` then `raiden install`:

- `raiden init` asks for the stack, member disks, raid level, and boot mode
  (efi/bios) -- detecting sensible defaults -- and writes `raiden.toml`.
- `raiden install` prints the disks it will ERASE for you to confirm, asks for the
  encryption password (typed twice, re-prompting on a mismatch), then provisions.

running inside `screen` means a dropped ssh/console connection does not abort the
install: reconnect and `screen -r raiden` to reattach. when it finishes, `reboot`
into the new system.

needs a wired/DHCP network (and `wget`, which the standard live image ships). to
stop after `init` and review or edit `raiden.toml` before installing yourself, run
`RAIDEN_REVIEW=1 sh /tmp/livecd.sh` instead.

### lower level: just the binary

to drive raiden yourself (no screen, no apt), install only the static binary:

```
wget -qO- https://raw.githubusercontent.com/khimaros/raiden/master/install.sh | sh
raiden install
```

`raiden install` with no config file discovers the machine's disks, generates a
stack-correct config, confirms the disks it will ERASE, then provisions. for a
fully unattended run, pre-answer every choice and pass the password
non-interactively:

```
RAIDEN_PASSWORD=... raiden install --yes \
  --stack dm-crypt~zfs --level raidz2 --members sda,sdb,sdc,sdd
```

or run `raiden init` first to write a `raiden.toml` you can edit, then `raiden
install --config raiden.toml`. the same `livecd.sh install`/`rescue` subcommands
are what the vm test harness uses, so the live flow and the tested flow stay in
sync.

## usage

```
raiden [global flags] <command>
```

provisioning:

```
raiden init                     # generate a raiden.toml for this machine
raiden init --non-interactive   # accept the detected defaults, no prompts
raiden install                  # run the full pipeline
raiden install --dry-run        # print every command without running it
raiden install --from mount     # resume from a phase
raiden install --only partition # run a single phase
raiden install --list-phases
raiden replace --disks vdb,vdc       # rebuild whole disks (in place)
raiden replace --disks vdb --esp --boot  # rebuild just the boot region (no resilver)
raiden replace --disks vdb --with sde    # physical swap: vdb out, sde in
```

operations (post-install or from a livecd):

```
raiden status                   # array health + read-errors mapped to files
raiden status --bad-files       # only the files hit by unrecoverable read errors
raiden scrub --wait
raiden rescue                   # assemble + unlock + mount (livecd -> chroot)
raiden recover                  # bring a degraded root online from the initramfs
raiden mount                    # ensure the stack is open + mounted (idempotent)
raiden mount --boot --at /      # just (re)mount /boot + /boot/efi on a live system
raiden close                    # unmount, stop arrays, lock crypt
raiden remove --disks vdb
raiden sync boot                # resync the independent /boot mirrors from the live primary
raiden sync efi                 # resync the esp mirrors from the live /boot/efi (efi mode)
raiden benchmark                 # fsync-bound fileio benchmark on the array
```

`raiden replace` rebuilds whole disks by default; naming layers (`--esp`, `--boot`,
`--root`) rebuilds only those -- `--esp --boot` recovers a scribbled boot region
without touching the root member, skipping the slow resilver. `raiden replace
--disks=a --with=c` physically swaps disk `a` for a new disk `c` (paired by
position): the old disk is detached best-effort (it may be gone) and never wiped,
the new disk is wiped + provisioned + re-added, and the manifest's member list is
mutated (`a`->`c`). the new disk adopts the old disk's esp/luks identity (the
shared esp uuid, the shared /boot uuid) so fstab stays valid; crypttab is
regenerated for the new crypt names. `--with` is optional -- without it, replace
is the in-place rebuild. `raiden mount` brings
the stack up from the first available member (the primary ESP mounts at `/boot/efi`;
mirrors are synced transiently), so a lost-primary system stays serviceable until a
`replace`.

`raiden recover` brings a degraded root online from the initramfs rescue shell so
the boot can continue, generalizing the per-stack manual `mount -o degraded`
commands into one command (btrfs/bcachefs refuse a multi-device mount with a faulty
member; md/zfs assemble degraded on their own, so it is a no-op there). raiden and
the manifest are baked into the initrd at install (`install.initramfs_recovery`,
default on), so the command is available at the rescue shell; run `raiden recover`
there, then `exit` to resume booting. it is check/fix like `doctor`: it mounts the
root only if it is not already mounted, and confirms each action unless `--yes`.

`raiden sync boot` and `raiden sync efi` resync the independent `/boot` and esp
mirrors from the live primary (the default, non-raid `/boot`). both verify the
source first (boot: read-only fsck, grub.cfg/kernel/initrd presence, initrd
contains cryptsetup; efi: shimx64/grubx64 present) and bail without syncing on a
verify failure, so a broken source is never propagated; `--force` skips verify
(used by no script). they prompt unless `--yes`. `sync boot` is a no-op when
`boot.raid` is set (mdadm handles replication). the kernel `postinst.d`/`postrm.d`
hooks call `sync boot` automatically after each kernel update, and the `grub.d`
hook calls `sync efi` on each `update-grub`; you only run them by hand to force a
resync. the two are separate because /boot must sync after `zz-update-grub`
(needs the final grub.cfg), while the esp syncs during `update-grub`.

`raiden benchmark` runs the durable-write (`sysbench fileio`, `--file-fsync-all`)
workload on the root fs and prints a per-mode summary (`--format json` for tooling);
`--dry-run` prints the exact sysbench commands. tune it with the `[benchmark]`
config keys (`size`, `passes`, `rndwr_events`, `seqwr_events`) or the matching
flags.

`raiden doctor` walks the installed system and reports the health of each layer
the manifest says should be present: disk presence, /boot + /boot/efi mounts
(and a note when they sit on different disks -- expected, since both mount by
a shared fs uuid),
fstab + crypttab, luks headers, array status (md/zfs/btrfs), boot + esp mirror
presence AND drift (a present-but-never-synced mirror is a silent failure),
grub install, initrd (that it carries the boot/recovery binaries -- decrypt_keyctl
+ keyctl, cryptsetup, and the stack's assemble/mount tools; and, when
`install.initramfs_recovery` is on, that raiden + the manifest are baked in for
`raiden recover`), the boot-mirror kernel
hooks AND the esp-mirror grub.d hook (both checked for presence AND the
executable bit, since run-parts and grub-mkconfig silently skip non-executable
scripts), and the manifest itself. it runs every check best-effort (a dead disk
fails its own check but the rest still run), and exits non-zero if any check
fails. `--verbose` shows full detail lines.

`raiden doctor --fix` repairs the auto-fixable checks in place: it installs the
boot-mirror kernel hooks (`postinst.d`/`postrm.d`/`zzz-raiden-boot-mirror`), the
esp-mirror grub.d hook (`90_copy_to_efi_mirrors`), and the raiden recovery hook
(`/etc/initramfs-tools/hooks/raiden`, then rebuilds the initrd so it carries raiden
+ the manifest for `raiden recover` -- a plain rebuild cannot add them when the hook
is absent, eg. on a legacy install) when missing or
non-executable, re-runs `raiden sync boot`/`sync efi` to repair drifted mirrors,
and re-stamps any mirror whose fs uuid diverges from the shared one so it can
serve `/boot` or `/boot/efi` if the primary is lost. the re-stamp re-observes
live state (it propagates the mounted source's uuid, never touches the source,
and skips already-shared mirrors); `/boot` is changed in place (`tune2fs`), an
esp is reformatted with the shared volume id then repopulated by the sync. this
doubles as the one-shot migration for a legacy host installed before the shared
esp uuid: `raiden doctor --fix` brings it up to spec. each fix is confirmed
individually before it runs (`--yes` auto-accepts them all, for unattended use);
a declined fix is left in place and noted. `raiden doctor --fix --dry-run` prints
the fix flow instead of the checks table -- each fixable check followed by the
exact commands it would run, in order (the `mkfs.msdos -i ...` per mirror, then the
re-sync) -- and changes nothing. a look-before-you-leap for the destructive
re-stamp, before running it on hardware. each applied fix is reported as a
`fixed` status row. drift and uuid divergence are warns (the system still boots
from the primary); destructive checks (fstab, crypttab, luks, array state,
grub-install) are never auto-fixed.

introspection:

```
raiden config show              # resolved config + derived layout
raiden config validate          # check config without touching disks
raiden devices                  # candidate disks and array members
raiden doctor                   # installed-system health checks
raiden doctor --fix             # repair missing hooks + drifted mirrors
```

global flags include `--config <path>`, `--dry-run`, `--yes`, `--resume`,
`-v/--verbose`, and per-field overrides such as `--stack`, `--level`,
`--members`, `--release`. interrupted operations resume from their last
checkpoint with `--resume`.

## configuration

install-time input is a TOML file (default `./raiden.toml`). the quickest way to
get a correct starting config on real hardware is `raiden init`: it discovers the
machine's disks (excluding the removable/live medium it booted from), detects the
firmware mode (efi/bios) and the partition prefix (nvme vs sd), picks the stack's
correct crypt settings, and writes a `raiden.toml` to review before installing. it
is interactive by default; `--stack`, `--level`, `--members`, and `--boot-mode`
pre-answer any step, and `--non-interactive` takes the detected defaults. see
[raiden.toml](raiden.toml) for an annotated example, and [examples/](examples)
for a complete, ready-to-edit config per stack (these double as the vm harness
fixtures). precedence, lowest to highest: built-in defaults, config file,
environment, command line flags.

each stack pairs the crypt layer with the right integrity choice: the
recommended ext4 stack uses an aead cipher (aegis128), while the zfs, btrfs, and
dm-integrity stacks use a plain cipher (aes-xts) because integrity is provided
elsewhere (zfs/btrfs checksum their own data; dm-integrity sits below md). the
examples encode this so a stack is configured correctly out of the box.

on the aead stacks, luksFormat wipes the whole disk to initialize integrity tags,
which is slow on large disks. `crypt.integrity_no_wipe = true` skips it for a fast
format, at the cost of leaving tags uninitialized (reads of unwritten sectors fail
until written, and it conflicts with the md array's initial resync) -- see the
caveat in [raiden.toml](raiden.toml).

by default `/boot` is not a raid array: each disk carries its own ext4 `/boot`
(all sharing one fs uuid), so every disk's grub boots from its local copy and the
system survives losing any disk -- including the first -- with no array to
assemble. the copies are kept in sync automatically on kernel changes. set
`[boot] raid = true` to use the legacy md raid1 `/boot` instead (`boot.level`
applies only in that mode).

after install, the resolved truth (stack, level, members, and the chosen
partition/luks/esp UUIDs) is written to `/boot/raiden/manifest.toml` (canonical)
and mirrored to `/etc/raiden/manifest.toml`. /boot is canonical so a livecd can
read it by mounting a member's /boot without unlocking the root fs. post-install
operations read this manifest, so they do not need a config that matches install
time. install also copies the `raiden` binary itself to
`/usr/local/sbin/raiden` in the target, so `status`/`scrub`/`replace`/`remove`/
`close` are available on the booted system without re-fetching it.

## unattended use and resume

`raiden install --yes --password-file <path>` (or the `RAIDEN_PASSWORD` env var)
runs without prompting. any interrupted operation can be continued with
`raiden <op> --resume`: raiden checkpoints after every step, so resume skips
everything already applied and continues from the next step, never re-running a
completed one.

a fresh `raiden install` (without `--resume`) is also re-runnable: it tears down
any stack a previous attempt left on the member disks -- unmounting, then
stopping the array and locking the crypt devices (or destroying the zpool) --
before it repartitions, so a retry does not fail at wipefs with "Device or
resource busy". this includes an array assembled under a non-canonical node (eg.
`md127`) as long as it sits on the configured member disks -- raiden finds it
through the members' `/sys` holders. an array on disks outside the member list is
left alone.

set `serial_console = true` (under `[install]`) to enable a serial console on the
installed system -- handy for headless servers and used by the automated test
harness.

`install.initramfs_recovery` (default on) bakes the raiden binary + the manifest
into the initrd so `raiden recover` is available at the rescue shell to bring a
degraded root online; set it to `false` to keep the initrd minimal (the
per-stack manual `mount -o degraded` is then the fallback).

## testing

```
make test-e2e        # fast: planning, validation, resume (no vm)
make test-vm-unit    # vm harness logic (no vm)
make test-vm ISO=/path/to/debian-live.iso              # libvirt vm run (resilience only)
make test-vm ISO=... STACK=dm-crypt~zfs                # a specific stack's example
make test-vm ISO=... CONFIG=examples/dm-crypt~btrfs.raid1c3.toml   # a specific config
make test-vm ISO=... BENCH=1                            # add the sysbench benchmark (off by default)
```

the vm harness drives a real install and the corruption/repair scenarios in a
libvirt/kvm vm over the serial console, fully automated, and writes a graded
report (with its full run log saved alongside). the fsync-bound sysbench
benchmark is off by default (it is ~26min and orthogonal to correctness); add it
with `BENCH=1` for a performance report. it installs the matching
`examples/` config for the stack (or `CONFIG=<path>`), overlaying only the
test-specific keys (serial console, member disks, /boot mode). graded reports and
logs land in [tests/vm/reports/](tests/vm/reports/); see
[tests/vm/README.md](tests/vm/README.md). [ANALYSIS.md](ANALYSIS.md) compares the
stacks (performance + resilience) from those reports and gives recommendations.

## building

```
make
```

see [CONTRIBUTING.md](CONTRIBUTING.md) for the toolchain and test workflow.
