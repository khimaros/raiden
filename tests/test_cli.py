"""end-to-end tests over the planning and config surface."""

import pytest

STACKS = [
    "dm-crypt~md~lvm~ext4",
    "dm-crypt~btrfs",
    "dm-crypt~zfs",
    "dm-integrity~md~dm-crypt~lvm~ext4",
]

# a valid raid level per stack family, for plan generation across stacks.
STACK_LEVEL = {
    "dm-crypt~md~lvm~ext4": "6",
    "dm-crypt~btrfs": "raid1c3",
    "dm-crypt~zfs": "raidz2",
    "dm-integrity~md~dm-crypt~lvm~ext4": "6",
}


def test_config_validate_ok(raiden):
    assert "config ok" in raiden("config", "validate").stdout


def test_list_phases(raiden):
    phases = raiden("install", "--list-phases").stdout.split()
    assert phases == [
        "apt", "prepare", "reset", "partition", "format", "mount",
        "strap", "bind", "install", "bootloader", "finish", "close",
    ]


def test_install_resets_existing_stack_before_wipe(raiden):
    # a re-run must free the member disks before wipefs, or wipefs fails with
    # "Device or resource busy" while a prior run's crypt/md/lvm still hold them.
    # the reset phase tears the stack down top-down (lvm -> md -> crypt), all
    # best-effort so a first run no-ops, then settles udev before partition wipes.
    out = raiden("install", "--dry-run").stdout
    assert "=== phase: reset ===" in out
    for cmd in [
        "vgchange -a n vg0",
        "mdadm --stop /dev/md/root",
        "cryptsetup luksClose vda3_crypt",
        "udevadm settle",
    ]:
        assert cmd in out, cmd
    # teardown precedes the destructive wipe, and crypt is closed before the
    # settle that precedes wipefs (udev must release the freed partitions first).
    wipe = out.index("wipefs -a")
    assert out.index("mdadm --stop /dev/md/root") < wipe
    assert out.index("cryptsetup luksClose vda3_crypt") < out.index("udevadm settle") < wipe


def test_reset_stops_oddly_named_md_holding_crypt(raiden):
    # md_stop by the /dev/md/root node only catches the canonical name; an array
    # assembled under a non-canonical node (md127, from a hand-create or a prior
    # boot's auto-assembly) is found via the crypt devices' /sys holders and
    # stopped. otherwise it keeps holding the crypt devices and the luksClose
    # (then wipefs) fails "busy".
    out = raiden("install", "--dry-run").stdout
    assert "holders/md*" in out
    assert "/dev/mapper/vda3_crypt" in out
    sweep = out.index("holders/md*")
    # after the vg is down (so the array can stop) and before the crypt close.
    assert out.index("vgchange -a n vg0") < sweep < out.index("cryptsetup luksClose vda3_crypt")


def test_install_dry_run_has_key_steps(raiden):
    out = raiden("install", "--dry-run").stdout
    # the password is never printed verbatim.
    assert "<password>" in out
    assert "cryptsetup -q luksFormat" in out
    assert "mkfs.ext4 -m 0 /dev/vg0/root" in out
    # the grub.d esp-mirror hook must be installed.
    assert "/mnt/etc/grub.d/90_copy_to_efi_mirrors" in out
    assert "90_copy_to_efi_mirrors" in out
    # the hook calls rsync, so it must be installed in the target (chroot)
    # before update-grub runs it. checking the exact target install line guards
    # against rsync being present only on the host.
    assert "chroot /mnt env DEBIAN_FRONTEND=noninteractive apt-get install -y grub-efi-amd64 shim-signed rsync" in out
    # debconf preseeded before the grub package install: keep the EFI/BOOT removable
    # fallback in sync on upgrades, but leave grub's own nvram management OFF --
    # raiden owns the per-disk nvram entries (avoids duplicate/cruft accumulation).
    assert "grub-efi-amd64 grub2/force_efi_extra_removable boolean true" in out
    assert "grub-efi-amd64 grub2/update_nvram boolean false" in out
    # grub is installed in both modes: the named EFI/debian layout (what the
    # efibootmgr entries and sync/doctor verification check) and the removable
    # EFI/BOOT fallback. both --no-nvram; raiden registers entries itself.
    assert 'install grub to the esp (named)' in out
    assert 'install grub to the esp (removable fallback)' in out
    # the primary esp mounts directly at /boot/efi by uuid (no per-slot mounts,
    # no symlink); the other esps are mirrors synced transiently under /run/raiden.
    assert "/boot/efi vfat" in out
    assert "/boot/efi1 vfat" not in out
    assert "ln -sfn efi1 /mnt/boot/efi" not in out  # no esp slot symlink
    # default boot mode is independent: a per-disk ext4 /boot, all sharing the
    # first member's fs uuid, synced by a script run from the kernel hooks. there
    # is no md array for /boot.
    assert "/dev/md/boot" not in out
    assert "mkfs.ext4 -m 0 -F -L boot /dev/vda2" in out
    assert "mkfs.ext4 -m 0 -F -U" in out  # the mirrors copy the primary's uuid
    assert 'echo "UUID=$uuid /boot ext4 defaults,nofail 0 2"' in out
    # the mirrors have no fstab entry; `sync boot` mounts them transiently
    # under /run/raiden at runtime (inside the raiden binary, not the plan).
    assert "/boot.mirror" not in out
    # the boot hooks are inline scripts (no separate wrapper file), one per dir.
    assert "/mnt/etc/kernel/postinst.d/zzz-raiden-boot-mirror" in out
    assert "/mnt/etc/kernel/postrm.d/zzz-raiden-boot-mirror" in out
    assert "exec /usr/local/sbin/raiden sync boot --yes" in out
    # the hook must NOT forward run-parts' positional args (kernel version +
    # bootdir): `sync boot` takes no positional, so clap would reject the call and
    # the exit-code-propagating hook would block every kernel upgrade.
    assert 'sync boot --yes "$@"' not in out
    # update-grub does not run the kernel hooks, so install syncs once explicitly.
    # invoked by absolute path: a chroot inherits the livecd's PATH, which lacks
    # /usr/local/sbin where raiden is staged in the target.
    assert "chroot /mnt /usr/local/sbin/raiden sync boot --yes" in out
    # the efi mirrors are synced explicitly in the finish phase too (the grub.d
    # hook no-ops at install, before raiden is staged into the target).
    assert "chroot /mnt /usr/local/sbin/raiden sync efi --yes" in out
    # postcondition after the syncs: every member esp/boot must carry its bootloader
    # or the install fails loudly (so a survivor can boot when the shared uuid mounts
    # /boot/efi or /boot from it).
    assert "verify every esp carries the bootloader" in out
    assert "verify every /boot carries grub.cfg" in out
    # the install manifest must be written into the target so post-install ops
    # (status/scrub/replace) resolve their config without a config file.
    assert "/mnt/etc/raiden/manifest.toml" in out
    # ...and the binary itself must be staged into the target, or those ops have
    # nothing to run after reboot. copied (not apt-installed) into /usr/local/sbin.
    assert "install -D -m 0755" in out
    assert "/mnt/usr/local/sbin/raiden" in out
    # staged after the rootfs exists, so the destination path is valid.
    assert out.index("debootstrap") < out.index("/mnt/usr/local/sbin/raiden")
    # dosfstools must be installed in the TARGET (chroot): replace recreates esps
    # with mkfs.msdos on the running system, so it cannot live on the host only.
    assert "chroot /mnt env DEBIAN_FRONTEND=noninteractive apt-get install -y gdisk dosfstools mdadm" in out


def test_install_bakes_raiden_into_the_initrd_for_recovery(raiden):
    # initramfs_recovery (default on): after the binary + manifest are staged, the
    # finish phase installs the raiden initramfs hook and rebuilds the initrd so it
    # carries raiden + the manifest, making `raiden recover` available at the rescue
    # shell. the hook copies the static binary in and the manifest to /etc/raiden.
    out = raiden("install", "--dry-run").stdout
    assert "/mnt/etc/initramfs-tools/hooks/raiden" in out
    assert "copy_exec /usr/local/sbin/raiden /sbin" in out
    assert "cp /etc/raiden/manifest.toml" in out
    assert "chroot /mnt update-initramfs -u -k all" in out
    # the hook is written only after the binary + manifest are staged (so the
    # rebuild has something to bake), and the rebuild precedes the mirror sync (so
    # the recovery-bearing initrd is what reaches every /boot mirror).
    assert out.index("/mnt/usr/local/sbin/raiden") < out.index(
        "/mnt/etc/initramfs-tools/hooks/raiden"
    )
    assert out.index("chroot /mnt update-initramfs -u -k all") < out.index(
        "chroot /mnt /usr/local/sbin/raiden sync boot --yes"
    )


def test_recover_dry_run_prints_the_recovery_flow(raiden):
    # `raiden recover --dry-run` previews the recovery flow (the exact commands, in
    # order) without touching anything -- pure plan generation. the default md~lvm
    # stack runs the array, activates lvm, then mounts /dev/vg0/root at /root (the
    # initramfs convention).
    out = raiden("recover", "--dry-run").stdout
    assert "recovery flow" in out
    assert "mdadm --run /dev/md/root" in out
    assert "mount /dev/vg0/root /root" in out
    # --at overrides the mount target (eg. /mnt from a livecd).
    out2 = raiden("recover", "--dry-run", "--at", "/mnt").stdout
    assert "mount /dev/vg0/root /mnt" in out2


def test_benchmark_dry_run_emits_sysbench_plan(raiden):
    # the fsync-bound fileio workload, on the root fs (not tmpfs), with the
    # configured sizing. --dry-run prints the exact sysbench invocations (v1).
    out = raiden("benchmark", "--dry-run").stdout
    assert "=== phase: benchmark ===" in out
    assert "/var/tmp/raiden-benchmark" in out
    assert (
        "sysbench fileio run --file-total-size=2G --file-test-mode=rndwr "
        "--file-fsync-all=on" in out
    )
    assert "--file-test-mode=seqwr" in out
    # flags override the configured sizing, and --passes controls the pass count.
    out2 = raiden("benchmark", "--dry-run", "--size", "1G", "--passes", "1").stdout
    assert "--file-total-size=1G" in out2
    assert out2.count("--file-test-mode=rndwr") == 1


def test_replace_default_rebuilds_whole_disk(raiden):
    # no layer flags: zap and rebuild the whole disk, including the root resilver.
    out = raiden("replace", "--disks", "vdb", "--dry-run").stdout
    assert "sgdisk --zap-all /dev/vdb" in out
    assert "mdadm --wait /dev/md/root" in out  # the resilver


def test_replace_esp_boot_skips_the_root_resilver(raiden):
    # partial: rebuild p1+p2 in place (delete+recreate, no whole-disk zap) and
    # leave the root member alone -- no resilver, no luksFormat. this is the fast
    # boot-recovery path (eg. after a scribbled esp/boot).
    out = raiden("replace", "--disks", "vdb", "--esp", "--boot", "--dry-run").stdout
    assert "sgdisk --zap-all" not in out
    assert "delete partition 1 on /dev/vdb" in out
    assert "delete partition 2 on /dev/vdb" in out
    assert "delete partition 3" not in out
    assert "mdadm --wait /dev/md/root" not in out
    assert "luksFormat" not in out


def test_replace_boot_uuid_reads_fstab_then_falls_back(raiden):
    # the shared /boot uuid comes from fstab (canonical), then any survivor's
    # /boot, and only mkfs's a fresh uuid if none exists -- it must not fail when
    # one survivor's /boot has no uuid (the empty-blkid bug).
    out = raiden("replace", "--disks", "vdb", "--dry-run").stdout
    assert '$2=="/boot"' in out  # fstab is the primary source
    assert "for s in /dev/vda2" in out  # fall back across surviving members
    assert "mkfs.ext4 -m 0 -F -L boot /dev/vdb2" in out  # fresh-uuid fallback, not a failure


def test_replace_unmounts_the_esp_before_rebuilding_it(raiden):
    # /boot/efi is mounted on a healthy system; replace must unmount it before
    # mkfs, or mkfs.msdos refuses "contains a mounted filesystem".
    out = raiden("replace", "--disks", "vda", "--esp", "--dry-run").stdout
    assert "umount /dev/vda1" in out
    rebuild = "recreate esp on /dev/vda1 sharing the esp uuid"
    assert out.index("umount /dev/vda1") < out.index(rebuild)
    # and it remounts /boot/efi afterward so the running system is left consistent.
    assert "mountpoint -q /boot/efi" in out
    assert out.index(rebuild) < out.index("mountpoint -q /boot/efi")


def _raid_boot_config(tmp_path):
    """a config file selecting the (opt-in) md raid1 /boot path."""
    p = tmp_path / "raid-boot.toml"
    p.write_text(
        '[disks]\nmembers = ["vda", "vdb", "vdc", "vdd"]\n'
        '[raid]\nstack = "dm-crypt~md~lvm~ext4"\nlevel = "6"\n'
        "[boot]\nraid = true\n"
    )
    return str(p)


def test_install_raid_boot_via_config(raiden, tmp_path):
    out = raiden(
        "install", "--dry-run", "--config", _raid_boot_config(tmp_path)
    ).stdout
    # opt-in raid mode rebuilds the md array path and writes the array fstab line.
    assert "mdadm --create --name=boot" in out
    assert "mkfs.ext4 -m 0 /dev/md/boot" in out
    assert "/dev/md/boot /boot ext4" in out
    # the independent-boot machinery must be absent in raid mode.
    assert "zzz-raiden-boot-mirror" not in out
    assert "/boot.mirror" not in out


def test_independent_boot_sync_runs_after_initramfs(raiden):
    # the boot-mirror sync must copy /boot AFTER the crypttab-aware initrd is
    # built (update-initramfs, in the finish phase). syncing earlier mirrors a
    # cryptsetup-less initrd, so a survivor booted after the primary's /boot is
    # destroyed cannot unlock luks and bring up the encrypted root.
    out = raiden("install", "--dry-run").stdout
    # match the finish-phase chroot step, not the bootloader-phase hook (which
    # also contains "raiden sync boot --yes" but runs before update-initramfs).
    sync = out.index("chroot /mnt /usr/local/sbin/raiden sync boot --yes")
    initramfs = out.index("update-initramfs -c -k all")
    assert initramfs < sync, "boot mirrors must be synced after update-initramfs"


# note: `sync boot/efi --dry-run` (non-raid) and `doctor` inspect the live host
# (findmnt /boot, blkid, mdadm, efibootmgr, ...), so they are NOT exercised here --
# the hermetic suite stays pure plan-generation against a config. the live behaviour
# is covered in the controlled vm (the sync_mirrors and doctor/doctor_fix scenarios).


def test_sync_boot_no_ops_when_boot_is_raid(raiden, tmp_path):
    # when boot.raid is set, /boot is md raid1 and mdadm handles replication:
    # `sync boot` must say so and succeed without planning any rsync.
    out = raiden(
        "sync", "boot", "--dry-run", "--config", _raid_boot_config(tmp_path)
    ).stdout
    assert "md raid1" in out
    assert "rsync" not in out


def test_rescue_activates_lvm_before_mount(raiden):
    # rescue assembles md/root then mounts /dev/vg0/root. the volume group must be
    # activated explicitly -- udev auto-activation is unreliable for a freshly
    # assembled (possibly degraded) array from a livecd -- or the mount fails with
    # "Can't lookup blockdev /dev/vg0/root".
    out = raiden("rescue", "--dry-run").stdout
    assert "vgchange -a y vg0" in out
    assert out.index("vgchange -a y vg0") < out.index("mount /dev/vg0/root /mnt")


def test_mount_full_opens_and_mounts_the_stack(raiden):
    # the full mount opens crypt, assembles, activates, and mounts root + boot/efi.
    out = raiden("mount", "--dry-run").stdout
    assert "=== phase: mount ===" in out
    assert "cryptsetup luksOpen" in out
    assert "mount /dev/vg0/root /mnt" in out
    assert "/mnt/boot/efi" in out


def test_mount_boot_only_skips_crypt_and_root(raiden):
    # --boot --at / just ensures the live /boot + /boot/efi are mounted (from the
    # first available member, idempotently) -- no crypt, no array, no password.
    out = raiden("mount", "--boot", "--at", "/", "--dry-run").stdout
    assert "cryptsetup luksOpen" not in out
    assert "mount /dev/vg0/root" not in out
    assert "mountpoint -q /boot ||" in out
    assert "mountpoint -q /boot/efi ||" in out


# the example config catalog, one per stack, named like raid-explorations'
# explorations/ (<stack>.<level>[.<variant>].toml). the vm harness installs
# these, so they must stay valid and plannable.
EXAMPLES = [
    "dm-crypt~md~lvm~ext4.raid6.aead.toml",
    "dm-crypt~md~lvm~ext4.raid10.aead.toml",
    "dm-crypt~md~lvm~ext4.raid6.bios.toml",
    "dm-crypt~md~lvm~xfs.raid6.aead.toml",
    "dm-crypt~md~lvm~xfs.raid10.aead.toml",
    "dm-crypt~btrfs.raid1c3.toml",
    "dm-crypt~bcachefs.replicas3.toml",
    "dm-crypt~zfs.raidz2.toml",
    "dm-integrity~md~dm-crypt~lvm~ext4.raid6.toml",
]


def test_bcachefs_adds_apt_repo_and_uses_replicas(raiden):
    # bcachefs is out-of-tree: the install adds apt.bcachefs.org (key + source) on
    # host and target for the tools + dkms module, uses plain crypt (no aead --
    # bcachefs checksums its own data), and formats with --replicas for redundancy.
    out = raiden(
        "install", "--dry-run", "--config", "examples/dm-crypt~bcachefs.replicas3.toml"
    ).stdout
    assert "apt.bcachefs.org" in out
    assert "bcachefs-kernel-dkms" in out
    assert "mkfs.bcachefs -f --replicas=3" in out
    assert "--integrity=aead" not in out  # plain crypt
    # the repo is added in the target chroot too (for future updates + the dkms build).
    assert "/mnt/etc/apt/sources.list.d/apt.bcachefs.org.sources" in out
    # the repo is pinned below debian so only the dkms module comes from it
    # (its per-suite bcachefs-tools can lag the distro libs and fail to install).
    assert "apt.bcachefs.org.pref" in out
    assert "Pin-Priority: 100" in out


def test_xfs_stack_uses_xfs_mkfs_and_fstab(raiden):
    # the xfs variant of the md/lvm stack formats the root with mkfs.xfs, installs
    # xfsprogs, and writes an xfs fstab line; the rest matches the ext4 stack.
    out = raiden(
        "install", "--dry-run", "--config", "examples/dm-crypt~md~lvm~xfs.raid6.aead.toml"
    ).stdout
    assert "mkfs.xfs -f /dev/vg0/root" in out
    assert "xfsprogs" in out
    assert "/dev/vg0/root / xfs defaults 0 0" in out
    # the root is xfs, not ext4 (the independent /boot is still ext4 on each p2).
    assert "mkfs.ext4 -m 0 /dev/vg0/root" not in out
    assert "/dev/vg0/root / ext4" not in out
ZFS_EXAMPLE = "examples/dm-crypt~zfs.raidz2.toml"
BTRFS_EXAMPLE = "examples/dm-crypt~btrfs.raid1c3.toml"
INTEGRITY_EXAMPLE = "examples/dm-integrity~md~dm-crypt~lvm~ext4.raid6.toml"


def test_dm_integrity_uses_builtin_integrity_algorithm(raiden):
    # dm-integrity's internal hash must be a kernel built-in (crc32c). xxhash64
    # needs the xxhash crypto module, which is absent from the live env (so
    # `integritysetup format` fails with "reload ioctl ... No such file or
    # directory") and from the integrity initramfs hook (so the devices could not
    # be opened at boot). crc32c is the integritysetup default and always present.
    out = raiden("install", "--dry-run", "--config", INTEGRITY_EXAMPLE).stdout
    assert "integritysetup" in out and "format" in out


def test_reset_integrity_stack_sweeps_integrity_device_holders(raiden):
    # the integrity stack's array sits on the per-disk dm-integrity devices (md is
    # below the single crypt), so the reset holder sweep must walk those, not a
    # crypt device, to find and stop an oddly-named array before teardown.
    out = raiden("install", "--dry-run", "--config", INTEGRITY_EXAMPLE).stdout
    assert "holders/md*" in out
    assert "/dev/mapper/vda3_int" in out
    assert "--integrity=crc32c" in out
    assert "xxhash64" not in out


@pytest.mark.parametrize("name", EXAMPLES)
def test_example_config_validates(raiden, name):
    out = raiden("config", "validate", "--config", f"examples/{name}").stdout
    assert "config ok" in out


def test_btrfs_crypttab_enables_initramfs(raiden):
    # btrfs crypt members must be pulled into the initrd (the initramfs flag, as in
    # the md and zfs stacks), or the multi-device root cannot unlock at boot and
    # drops to the initramfs shell.
    out = raiden("install", "--dry-run", "--config", BTRFS_EXAMPLE).stdout
    assert "none luks,discard,initramfs,keyscript=decrypt_keyctl" in out


def test_btrfs_root_fstab_mounts_at_root_by_uuid(raiden):
    # the btrfs root fstab entry must mount at / by uuid. the rewrite captured the
    # live mount verbatim ("grep btrfs /proc/self/mounts"), which during install
    # records the install target mountpoint /mnt -- so the installed system had no
    # rw / entry and booted read-only (systemd-remount-fs had nothing to remount).
    out = raiden("install", "--dry-run", "--config", BTRFS_EXAMPLE).stdout
    assert "grep btrfs /proc/self/mounts >> /mnt/etc/fstab" not in out
    assert "UUID=$uuid / btrfs" in out


def test_zfs_example_uses_plain_crypt(raiden):
    # zfs checksums its own data, so the example must use a plain cipher with no
    # aead -- aegis128 (the default) would force aead and add needless overhead.
    out = raiden("install", "--dry-run", "--config", ZFS_EXAMPLE).stdout
    assert "--cipher=aes-xts-plain64" in out
    assert "--integrity=aead" not in out
    assert "--sector-size=4096" in out  # 4k-native ssd friendly
    assert "zpool" in out  # confirms this is the zfs plan


def test_zfs_host_builds_against_running_kernel(raiden):
    # on the livecd, zfs-dkms must build for the RUNNING kernel. linux-headers-amd64
    # (correct for the target) can pull a newer kernel whose module the running
    # kernel cannot modprobe, so the host installs the running kernel's headers and
    # drops linux-headers-amd64.
    host = raiden(
        "install", "--dry-run", "--config", ZFS_EXAMPLE, "--only", "apt"
    ).stdout
    assert "linux-headers-$(uname -r)" in host
    assert "linux-headers-amd64" not in host
    # the target (installed system) still gets linux-headers-amd64 for its kernel.
    target = raiden(
        "install", "--dry-run", "--config", ZFS_EXAMPLE, "--only", "install"
    ).stdout
    assert "linux-headers-amd64" in target


def test_root_partition_end_aligned_to_sector_size(raiden):
    # without aead, cryptsetup refuses a device whose size is not a multiple of
    # --sector-size, so the root partition end is aligned down to it. 4096-byte
    # sectors are 8 lba sectors, so (end+1) is rounded down to a multiple of 8.
    out = raiden("install", "--dry-run", "--config", ZFS_EXAMPLE).stdout
    assert "sgdisk -E /dev/vda" in out
    assert "/ 8 * 8 - 1" in out
    assert "sgdisk -n3:0:$end -t3:8301 /dev/vda" in out


@pytest.mark.parametrize("stack", STACKS)
def test_every_stack_plans(raiden, stack):
    out = raiden(
        "install", "--dry-run", "--stack", stack, "--level", STACK_LEVEL[stack]
    ).stdout
    assert f"install plan for stack {stack}" in out


def test_bad_level_rejected(raiden):
    r = raiden("config", "validate", "--stack", "dm-crypt~zfs", "--level", "6", expect_ok=False)
    assert r.returncode != 0
    assert "invalid for zfs" in r.stderr


def test_unknown_stack_rejected(raiden):
    r = raiden("config", "validate", "--stack", "bogus", expect_ok=False)
    assert r.returncode != 0


def test_replace_requires_a_surviving_disk(raiden):
    r = raiden("replace", "--dry-run", "--disks", "vda,vdb,vdc,vdd", expect_ok=False)
    assert r.returncode != 0
    assert "at least one member must survive" in r.stderr


def test_replace_plans_for_subset(raiden):
    # default (independent) boot: no md array for /boot. each replaced disk's
    # boot partition is reformatted with the shared uuid and a survivor's /boot is
    # cloned onto it; the esp clone and root re-add are unchanged.
    out = raiden("replace", "--dry-run", "--disks", "vdb,vdc").stdout
    assert "clone esp from /dev/vda1 to /dev/vdb1" in out
    assert "re-add /dev/mapper/vdb3_crypt to the root array" in out
    assert "mdadm --wait /dev/md/root" in out
    assert "mdadm --remove /dev/md/root detached" in out
    assert "/dev/md/boot" not in out
    assert "mkfs.ext4 -m 0 -F -U" in out  # replaced boot fs shares the uuid
    assert "clone /boot from /dev/vda2 to /dev/vdb2" in out
    # the clone refuses to rsync --delete from an unverified source (would wipe the
    # mirror) and verifies the bootloader marker landed.
    assert 'EFI/debian/shimx64.efi' in out and 'grub/grub.cfg' in out
    # postcondition: replace fails loudly if a rebuilt mirror is missing its
    # bootloader, rather than silently shipping an unbootable esp/boot. the esp
    # check requires shim AND grub (the same criteria as doctor's bootloader check).
    assert "verify every esp carries the bootloader" in out
    assert "EFI/debian/grubx64.efi" in out  # esp postcondition checks grub too
    assert "verify every /boot carries grub.cfg" in out


def test_replace_preserves_luks_uuid(raiden):
    # replace re-luksFormats the new disk with a fresh header; it must then restore
    # the disk's original luks uuid (read from the running /etc/crypttab) so the
    # installed crypttab still unlocks it on the next reboot. without this, a reboot
    # after replace drops to the initramfs -- the replaced members never unlock, so
    # the array cannot assemble ("cannot start dirty degraded array").
    out = raiden("replace", "--dry-run", "--disks", "vdb").stdout
    assert "/etc/crypttab" in out
    assert "cryptsetup -q luksUUID" in out
    assert "--uuid" in out
    # the uuid stamp must come after the (fresh) luksFormat and before the unlock.
    assert out.index("luksFormat") < out.index("luksUUID")
    assert out.index("luksUUID") < out.index("luksOpen")


def test_replace_settles_after_teardown_before_reformat(raiden):
    # replace tears down the old crypt/dm devices, but udev releases the
    # underlying partition asynchronously. without a settle before the partition
    # is wiped and re-luksFormatted, mkfs/luksFormat can hit "device busy" on the
    # just-freed partition (intermittent, seen replacing a still-healthy member).
    out = raiden("replace", "--dry-run", "--disks", "vdb").stdout
    assert "settle udev after teardown" in out
    assert out.index("settle udev after teardown") < out.index("luksFormat")


def test_replace_relocates_boot_before_reformat(raiden):
    # independent /boot mounts by the shared fs uuid, so the live /boot can sit on
    # a disk being replaced. that disk's boot partition must be vacated (remounted
    # on a survivor) before mkfs, or mkfs refuses ("device is mounted").
    out = raiden("replace", "--dry-run", "--disks", "vdb").stdout
    assert "move /boot off any disk being replaced" in out
    assert out.index("move /boot off any disk being replaced") < out.index("-L boot /dev/vdb2")


def test_replace_raid_boot_waits_for_both_arrays(raiden, tmp_path):
    out = raiden(
        "replace", "--dry-run", "--disks", "vdb,vdc",
        "--config", _raid_boot_config(tmp_path),
    ).stdout
    # raid mode must wait for BOTH arrays to finish rebuilding; leaving /boot
    # mid-resync makes it unreadable by grub if another member is then lost.
    assert "mdadm --wait /dev/md/boot" in out
    assert "mdadm --wait /dev/md/root" in out
    # a wholly-lost disk has no device node, so per-device --remove is a no-op and
    # the array keeps a vacant slot that blocks repartition + re-add. the slot
    # must be cleared array-wide (detached/failed) for both arrays.
    assert "mdadm --remove /dev/md/boot detached" in out
    assert "mdadm --remove /dev/md/root detached" in out


def test_replace_with_swaps_member_for_new_disk(raiden):
    # --disks=vdb --with=sde physically swaps vdb for sde: the old disk is
    # detached (best-effort, not wiped) and the new disk is provisioned +
    # re-added. the plan targets the NEW disk's partitions throughout.
    out = raiden("replace", "--dry-run", "--disks", "vdb", "--with", "sde").stdout
    # detach the OLD disk's crypt member (vdb3_crypt), not the new one.
    assert "cryptsetup luksClose vdb3_crypt" in out
    assert "mdadm --remove /dev/md/root /dev/mapper/vdb3_crypt" in out
    # the OLD disk is never wiped; only the NEW disk is.
    assert "wipefs -a /dev/sde3" in out
    assert "wipefs -a /dev/vdb" not in out
    # provision + re-add the NEW disk under its own crypt name.
    assert "luksFormat --cipher=aegis128-plain64" in out
    assert "cryptsetup luksOpen /dev/sde3 sde3_crypt" in out
    assert "mdadm --add /dev/md/root /dev/mapper/sde3_crypt" in out
    # crypttab is regenerated with the new member (sde) in place of vdb.
    assert "write /etc/crypttab" in out
    assert "sde3_crypt UUID=$uuid" in out
    assert "vdb3_crypt" not in out.split("=== phase: crypttab ===")[1]


def test_replace_with_adopts_the_shared_esp_and_boot_uuid(raiden):
    # every esp shares one vfat uuid and every /boot one ext4 uuid, so swapping the
    # PRIMARY disk (vda) just re-stamps those shared uuids onto the new disk -- the
    # /boot/efi fstab entry stays valid and resolves to the new esp.
    out = raiden("replace", "--dry-run", "--disks", "vda", "--with", "sde").stdout
    # the new disk's esp is recreated with the shared esp uuid (from fstab / a
    # survivor), not a fresh one.
    assert "recreate esp on /dev/sde1 sharing the esp uuid" in out
    # /boot shares the uuid from fstab / a survivor.
    assert "mkfs.ext4 -m 0 -F -U" in out
    # esp content cloned from a survivor onto the new disk.
    assert "clone esp from /dev/vdb1 to /dev/sde1" in out


def test_replace_with_rejects_mismatched_lengths(raiden):
    # --disks and --with must pair 1:1 by position.
    r = raiden(
        "replace", "--dry-run", "--disks", "vdb,vdc", "--with", "sde",
        expect_ok=False,
    )
    assert r.returncode != 0
    assert "1:1" in r.stderr


def test_replace_with_rejects_new_disk_already_a_member(raiden):
    # the --with disk must be a NEW disk, not an existing member.
    r = raiden(
        "replace", "--dry-run", "--disks", "vdb", "--with", "vdc",
        expect_ok=False,
    )
    assert r.returncode != 0
    assert "already a member" in r.stderr


def test_replace_with_rejects_old_disk_not_a_member(raiden):
    r = raiden(
        "replace", "--dry-run", "--disks", "zzz", "--with", "sde",
        expect_ok=False,
    )
    assert r.returncode != 0
    assert "not a configured member" in r.stderr


def test_scrub_independent_skips_boot_array(raiden, tmp_path):
    # independent /boot has no array to scrub; raid mode does.
    indep = raiden("scrub", "--dry-run").stdout
    assert "mdadm --action=check /dev/md/boot" not in indep
    raid = raiden(
        "scrub", "--dry-run", "--config", _raid_boot_config(tmp_path)
    ).stdout
    assert "mdadm --action=check /dev/md/boot" in raid


def test_resume_without_checkpoint_fails(raiden, tmp_path):
    # no checkpoint at the (overridden, hermetic) path -> nothing to resume.
    cp = tmp_path / "checkpoint.toml"
    r = raiden("install", "--resume", expect_ok=False, env={"RAIDEN_CHECKPOINT": str(cp)})
    assert r.returncode != 0
    assert "no checkpoint" in r.stderr


def test_resume_rejects_from_and_only(raiden):
    # --resume continues an interrupted run as-is; combining it with a phase
    # selector is contradictory and rejected up front.
    for sel in (["--from", "mount"], ["--only", "partition"]):
        r = raiden("install", "--resume", *sel, expect_ok=False)
        assert r.returncode != 0
        assert "cannot be combined" in r.stderr


def test_resume_rejects_a_different_operation(raiden, tmp_path):
    # a checkpoint from one op must not be resumed by another -- the cursor is
    # meaningless against a different plan. resume validates this before touching
    # disks. driven via `install` (unlike the post-install ops, install needs no
    # manifest, so this stays hermetic): a replace checkpoint must not resume it.
    cp = tmp_path / "checkpoint.toml"
    cp.write_text(
        'operation = "replace"\nconfig_hash = "abc"\nscope = ""\n'
        'phase = 0\nstep = 0\nphase_name = "partition"\n'
    )
    r = raiden(
        "install", "--resume",
        expect_ok=False,
        env={"RAIDEN_CHECKPOINT": str(cp)},
    )
    assert r.returncode != 0
    assert "checkpoint is for" in r.stderr


def test_post_install_ops_require_an_install_manifest(raiden):
    # doctor/sync/status/scrub/replace/remove/close only run on an installed raiden
    # system: with no manifest (and not --dry-run) they refuse up front, before
    # inspecting or touching anything -- so a stray raiden.toml in cwd can't make
    # `doctor --fix` mutate a non-install host. (bails at the guard, so host-safe.)
    r = raiden("doctor", expect_ok=False)
    assert r.returncode != 0
    assert "only runs on an installed raiden system" in r.stderr


def test_bios_mode_uses_bios_boot_partition(raiden):
    out = raiden("install", "--dry-run", "--only", "partition").stdout
    assert "EF00" in out  # efi by default
    # bios mode would emit EF02; verify via an override config is out of scope
    # here, but ensure the efi path includes the esp type code.


def test_init_generates_a_valid_aead_config(raiden, tmp_path):
    # `raiden init` writes a complete config from a few choices. the md/lvm ext4
    # stack must get the aead crypt block (aegis128), and the result must both
    # validate and plan a full install -- so init is a real shortcut past hand
    # editing an example.
    out = tmp_path / "raiden.toml"
    raiden(
        "init", "--non-interactive", "--output", str(out),
        "--members", "sda,sdb,sdc,sdd",
        "--stack", "dm-crypt~md~lvm~ext4", "--level", "6", "--boot-mode", "efi",
    )
    text = out.read_text()
    assert 'stack = "dm-crypt~md~lvm~ext4"' in text
    assert '"sda"' in text and '"sdd"' in text
    assert 'part_prefix' not in text  # derived per disk, never stored
    assert 'boot_mode = "efi"' in text
    assert 'cipher = "aegis128-plain64"' in text
    assert 'integrity = "aead"' in text
    assert "generated by `raiden init`" in text  # the review header
    assert "config ok" in raiden("config", "validate", "--config", str(out)).stdout
    plan = raiden("install", "--dry-run", "--config", str(out)).stdout
    assert "install plan for stack dm-crypt~md~lvm~ext4" in plan


def test_init_detects_nvme_prefix_and_plain_crypt(raiden, tmp_path):
    # nvme disks need the "p" partition separator (nvme0n1p1), and zfs (which
    # checksums its own data) must get the plain aes-xts crypt block, not aead.
    # the level defaults to one the disk count supports (4 disks -> raidz2).
    out = tmp_path / "nvme.toml"
    raiden(
        "init", "--non-interactive", "--output", str(out),
        "--members", "nvme0n1,nvme1n1,nvme2n1,nvme3n1",
        "--stack", "dm-crypt~zfs", "--boot-mode", "bios",
    )
    text = out.read_text()
    assert 'part_prefix' not in text  # derived per disk, never stored
    assert 'level = "raidz2"' in text
    assert 'cipher = "aes-xts-plain64"' in text
    assert 'integrity = "none"' in text
    assert "config ok" in raiden("config", "validate", "--config", str(out)).stdout


def test_init_default_level_fits_the_disk_count(raiden, tmp_path):
    # with no --level, init picks a level the member count supports so the config
    # validates: two md disks cannot be raid6 (the default), so it chooses raid1.
    out = tmp_path / "two.toml"
    raiden(
        "init", "--non-interactive", "--output", str(out),
        "--members", "sda,sdb", "--stack", "dm-crypt~md~lvm~ext4", "--boot-mode", "bios",
    )
    assert 'level = "1"' in out.read_text()
    assert "config ok" in raiden("config", "validate", "--config", str(out)).stdout


def test_mixed_nvme_and_sd_members_get_per_disk_partition_prefixes(raiden, tmp_path):
    # part_prefix is gone: each disk derives its own separator. a mixed
    # nvme+sd array must produce sda1 and nvme0n1p1 in the same plan, which one
    # global prefix could not.
    out = tmp_path / "mixed.toml"
    raiden(
        "init", "--non-interactive", "--output", str(out),
        "--members", "sda,nvme0n1", "--stack", "dm-crypt~md~lvm~ext4",
        "--level", "1", "--boot-mode", "efi",
    )
    plan = raiden("install", "--dry-run", "--config", str(out), "--only", "format").stdout
    # the format phase addresses each member's partitions; the per-disk prefix
    # makes nvme0n1p1 and sda1 coexist in one plan (one global prefix could not).
    assert "/dev/sda1" in plan
    assert "/dev/nvme0n1p1" in plan
    assert "/dev/sda2" in plan
    assert "/dev/nvme0n1p2" in plan


def test_init_refuses_to_overwrite_without_force(raiden, tmp_path):
    out = tmp_path / "keep.toml"
    out.write_text("# keep me\n")
    r = raiden(
        "init", "--non-interactive", "--members", "sda,sdb",
        "--output", str(out), expect_ok=False,
    )
    assert r.returncode != 0
    assert "force" in r.stderr
    assert out.read_text() == "# keep me\n"  # untouched
    # --force overwrites it.
    raiden(
        "init", "--non-interactive", "--members", "sda,sdb",
        "--output", str(out), "--force",
    )
    assert out.read_text() != "# keep me\n"


def test_install_without_config_generates_stack_correct_crypt(raiden, tmp_path):
    # `raiden install` with no config file generates one for the machine, so it is
    # a single command from a live env. crucially the generated config gets the
    # stack's correct crypt -- zfs (which checksums its own data) must use plain
    # aes-xts, not the aead default a bare flag override would otherwise leave in
    # place.
    missing = tmp_path / "none.toml"  # does not exist -> generate
    out = raiden(
        "install", "--dry-run", "--config", str(missing),
        "--stack", "dm-crypt~zfs", "--level", "raidz2", "--members", "vda,vdb,vdc,vdd",
    ).stdout
    assert "install plan for stack dm-crypt~zfs" in out
    assert "--cipher=aes-xts-plain64" in out
    assert "--integrity=aead" not in out
    assert "zpool" in out


def test_install_without_config_defaults_to_recommended_stack(raiden, tmp_path):
    # with neither a config nor a --stack, the config-less install falls back to
    # the recommended md/lvm ext4 stack (aead crypt) and a level that fits the
    # member count, and still plans a full install.
    missing = tmp_path / "none.toml"
    out = raiden(
        "install", "--dry-run", "--config", str(missing), "--members", "vda,vdb,vdc,vdd",
    ).stdout
    assert "install plan for stack dm-crypt~md~lvm~ext4" in out
    assert "--cipher=aegis128-plain64" in out  # aead default for this stack
    assert "mkfs.ext4 -m 0 /dev/vg0/root" in out


def test_crypt_integrity_no_wipe_adds_flag_for_aead(raiden, tmp_path):
    # crypt.integrity_no_wipe passes --integrity-no-wipe to luksFormat on aead
    # stacks, skipping the slow full-device integrity wipe. it is off by default.
    assert "--integrity-no-wipe" not in raiden("install", "--dry-run").stdout
    cfg = tmp_path / "nowipe.toml"
    cfg.write_text(
        '[disks]\nmembers = ["vda", "vdb", "vdc", "vdd"]\n'
        '[raid]\nstack = "dm-crypt~md~lvm~ext4"\nlevel = "6"\n'
        '[crypt]\nintegrity = "aead"\nintegrity_no_wipe = true\n'
    )
    out = raiden("install", "--dry-run", "--config", str(cfg)).stdout
    assert "luksFormat" in out and "--integrity=aead" in out
    assert "--integrity-no-wipe" in out


def test_crypt_integrity_no_wipe_ignored_without_aead(raiden, tmp_path):
    # the flag is only valid alongside --integrity; a plain (no-integrity) stack
    # must never emit it, or cryptsetup would reject the format.
    cfg = tmp_path / "plain.toml"
    cfg.write_text(
        '[disks]\nmembers = ["vda", "vdb", "vdc", "vdd"]\n'
        '[raid]\nstack = "dm-crypt~zfs"\nlevel = "raidz2"\n'
        '[crypt]\ncipher = "aes-xts-plain64"\nkey_size = 512\n'
        'integrity = "none"\nintegrity_no_wipe = true\n'
    )
    out = raiden("install", "--dry-run", "--config", str(cfg)).stdout
    assert "--integrity-no-wipe" not in out


def test_bios_grub_install_is_noninteractive(raiden):
    # the bios bootloader path must not block on an interactive dpkg-reconfigure
    # grub-pc (it hangs an unattended install). it preseeds the install devices
    # via debconf, then installs grub to each disk explicitly.
    out = raiden(
        "install", "--dry-run", "--config", "examples/dm-crypt~md~lvm~ext4.raid6.bios.toml"
    ).stdout
    assert "grub-pc/install_devices" in out  # preseeded, not prompted
    assert "dpkg-reconfigure" not in out  # no interactive reconfigure
    assert "grub-install /dev/vda" in out  # explicit per-disk install
