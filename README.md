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
  encryption password, then provisions.

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
raiden replace --disks vdb,vdc  # rebuild specific disks
```

operations (post-install or from a livecd):

```
raiden status                   # array health + read-errors mapped to files
raiden scrub --wait
raiden rescue                   # assemble + unlock + mount
raiden close                    # unmount, stop arrays, lock crypt
raiden remove --disks vdb
```

introspection:

```
raiden config show              # resolved config + derived layout
raiden config validate          # check config without touching disks
raiden devices                  # candidate disks and array members
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

by default `/boot` is not a raid array: each disk carries its own ext4 `/boot`
(all sharing one fs uuid), so every disk's grub boots from its local copy and the
system survives losing any disk -- including the first -- with no array to
assemble. the copies are kept in sync automatically on kernel changes. set
`[boot] raid = true` to use the legacy md raid1 `/boot` instead (`boot.level`
applies only in that mode).

after install, the resolved truth (stack, level, members, and the chosen
partition/luks/esp UUIDs) is written to `/etc/raiden/state.toml` and mirrored to
/boot. post-install operations read this manifest, so they do not need a config
that matches install time.

## unattended use and resume

`raiden install --yes --password-file <path>` (or the `RAIDEN_PASSWORD` env var)
runs without prompting. any interrupted operation can be continued with
`raiden <op> --resume`: raiden checkpoints after every step, so resume skips
everything already applied and continues from the next step, never re-running a
completed one.

set `serial_console = true` (under `[install]`) to enable a serial console on the
installed system -- handy for headless servers and used by the automated test
harness.

## testing

```
make test-e2e        # fast: planning, validation, resume (no vm)
make test-vm-unit    # vm harness logic (no vm)
make test-vm ISO=/path/to/debian-live.iso              # full libvirt vm run
make test-vm ISO=... STACK=dm-crypt~zfs                # a specific stack's example
make test-vm ISO=... CONFIG=examples/dm-crypt~btrfs.raid1c3.toml   # a specific config
```

the vm harness drives a real install and the corruption/repair scenarios in a
libvirt/kvm vm over the serial console, fully automated, and writes a graded
report (with its full run log saved alongside). it installs the matching
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
