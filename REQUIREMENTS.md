# requirements

raiden is a standalone command line tool for provisioning and maintaining
full-disk-encrypted RAID systems on Debian GNU/Linux. it is a rust rewrite of
the bash-based `raid-explorations` toolkit. these requirements capture the
capabilities that must not regress, plus the additions the rewrite introduces.

it is never okay to regress on a "must not regress" requirement in a release.

## must not regress (ported from raid-explorations)

### provisioning

- R1. provision a bootable Debian system from a LiveCD onto a multi-disk array,
  non-interactive except for the encryption password (typed once, verified).
- R2. support four stacks, selected by identifier:
  - `dm-crypt~btrfs`
  - `dm-crypt~zfs`
  - `dm-crypt~md~lvm~ext4` (default, recommended)
  - `dm-integrity~md~dm-crypt~lvm~ext4`
- R3. full-disk encryption of the root data via dm-crypt, or dm-integrity below
  dm-crypt for the integrity stack.
- R4. configurable RAID level per stack, plus a separate metadata level for
  btrfs:
  - md: 0, 1, 5, 6, 10
  - zfs: raidz1, raidz2, raidz3
  - btrfs: raid0, raid1, raid1c2, raid1c3, raid1c4, raid5, raid6, raid10
- R5. EFI and BIOS boot. for EFI, every disk gets an independent, bootable ESP
  (grub-install --removable plus a per-disk firmware boot entry), so the firmware
  can boot from any survivor. every ESP shares one vfat fs UUID (like /boot), so
  /boot/efi (mounted by that UUID, nofail) resolves to any survivor's ESP; the
  non-primary ESPs have no persistent mount point and are resynced from the live
  /boot/efi by a grub.d hook on each update-grub via transient mounts under
  /run/raiden. `nofail` keeps a lost disk from blocking boot, and losing the first
  member no longer drops /boot/efi (a survivor's ESP carries the same UUID).
  `raiden replace` re-stamps the shared ESP UUID onto a rebuilt disk so its ESP
  rejoins the set.
- R6. /boot: by default each disk carries an independent ext4 /boot, all sharing
  one fs UUID, so every disk's grub finds its own local /boot and first-disk loss
  still boots (no array to assemble). the non-primary copies have no persistent
  mount point; they are resynced from the live /boot by a script run from the
  kernel postinst.d/postrm.d hooks (after update-grub) via transient mounts under
  /run/raiden. `boot.raid = true` instead puts /boot on an md raid1 array across
  all member disks.
- R7. per-disk partition layout: p1 ESP (efi) or bios-boot, p2 /boot (independent
  ext4, or an md member when boot.raid), p3 root (crypt or integrity).
- R8. crypttab and fstab reference devices by UUID so device reordering is safe
  (the live /boot and /boot/efi mount by UUID). the mirror targets -- the other
  disks' /boot and ESP copies -- are addressed by member device, since each set
  shares one UUID (so it cannot address a specific disk) and the sync must write
  each specific disk; they are mounted only transiently during the sync, never
  persistently.
- R9. back up luks headers to /boot for disaster recovery.
- R10. prompt for the encryption password once, verify it, never write it to
  disk or logs.
- R11. tunable layer options: dm-crypt cipher, key size, sector size, aead
  integrity (and optionally skipping its full-device wipe); dm-integrity
  algorithm; btrfs checksum algorithm; ext4 stride/stripe-width aligned to the
  real md geometry.
- R12. post-install operations:
  - `status`: array health, plus mapping md read errors back to affected file
    paths (md stacks) via raid-stripe geometry, dmsetup offsets, and debugfs.
  - `scrub`: start or check a scrub.
  - `rescue`: assemble, unlock, and mount the array from a LiveCD.
  - `replace`: rebuild named disks, preserving each disk's ESP and luks UUID so
    the baked fstab/crypttab entries stay valid.
  - `remove`: detach disks from the array.
  - `close`: unmount, stop arrays, lock crypt devices.
- R13. configurable Debian release, backports, extra packages, and nvme
  partition prefix.
- R14. guard destructive operations: warn before destroying disks and require
  explicit confirmation unless overridden.

## additions introduced by the rewrite

- N1. TOML config file with flag overrides. precedence, lowest to highest:
  built-in defaults, config file, environment, command line flags.
- N2. persist an install manifest (`/boot/raiden/manifest.toml`, canonical,
  mirrored to `/etc/raiden/manifest.toml`) recording the resolved stack, level,
  members, and the partition/luks/esp UUIDs chosen at install. /boot is canonical
  so a livecd can read it without unlocking the root fs. `status`, `scrub`,
  `rescue`, `replace`, and `close` resolve from the manifest, so post-install
  operations need no hand-maintained config.
- N3. `--dry-run` prints the exact commands that would run; `config validate`
  checks a config without touching disks.
- N4. type-safe stack dispatch and config validation replace bash convention and
  `eval` brace-glob expansion.
- N5. fine-grained resume: every operation checkpoints after each step, and
  `--resume` continues from the next step, never re-running a completed one.
- N6. non-interactive password via `--password-file` / `RAIDEN_PASSWORD`, for
  unattended installs and the test harness.
- N7. optional serial console on the installed system (`serial_console`): grub,
  kernel, the initramfs unlock prompt, and login all reach ttyS0.
- N8. an automated libvirt vm test harness that installs and runs the
  resilience/repair scenarios over the serial console with no human in the loop
  and no timers, grading each result into a report. it follows through a boot that
  drops to the initramfs rescue shell by running `raiden recover` (see N11) and
  resuming -- required by btrfs/bcachefs, whose multi-device root needs
  `mount -o degraded` when a member is faulty (md and zfs assemble degraded
  automatically).
- N9. a per-stack example config catalog (`examples/`) of complete, valid
  configs that double as the vm harness fixtures. each example pairs the stack
  with its correct crypt integrity (aead for the ext4 stack; plain aes-xts for
  zfs/btrfs/dm-integrity). the harness installs `examples/<stack>.toml` (or an
  explicit `--config`), overlaying only the test-specific keys.
- N10. additional stacks beyond the ported four: `dm-crypt~md~lvm~xfs` (the md/lvm
  stack with an xfs root instead of ext4) and the experimental
  `dm-crypt~bcachefs` (per-disk dm-crypt + multi-device bcachefs, redundancy by
  replica count). a stack may declare extra apt repositories it needs beyond
  Debian's (eg. an out-of-tree dkms module): bcachefs adds apt.bcachefs.org for
  its kernel module and tools.
- N11. `raiden recover`: bring a degraded root online from the initramfs rescue
  shell so the boot can continue, generalizing the per-stack manual
  `mount -o degraded` commands into one command. structured as check/fix (mount
  the root only if not already mounted; confirm each action unless `--yes`); crypt
  members are already open by the initramfs, so it assembles/imports and mounts
  (degraded/forced) at `$rootmnt`. raiden + the manifest are baked into the initrd
  (config-guarded `install.initramfs_recovery`, default on) via a raiden initramfs
  hook so the command exists at the rescue shell. doctor verifies the bake and
  `doctor --fix` installs the hook (the legacy-host migration). automatic recovery
  (a local-premount hook) is future work; the manual command is the first step.

## non-goals

- raiden orchestrates system binaries (sgdisk, mdadm, cryptsetup, integritysetup,
  debootstrap, grub, debugfs, zpool, btrfs). it does not reimplement them.
- no GUI. no daemon. raiden runs, does its work, and exits.
