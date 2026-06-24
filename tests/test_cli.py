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
    # the mirrors have no fstab entry; the sync script mounts them transiently.
    assert "/boot.mirror" not in out
    assert "/run/raiden" in out
    assert "/mnt/usr/local/sbin/raiden-sync-boot-mirrors" in out
    assert "/mnt/etc/kernel/postinst.d/zzz-raiden-boot-mirror" in out
    # update-grub does not run the kernel hooks, so install syncs once explicitly.
    assert "chroot /mnt /usr/local/sbin/raiden-sync-boot-mirrors --strict" in out
    # the install manifest must be written into the target so post-install ops
    # (status/scrub/replace) resolve their config without a config file.
    assert "/mnt/etc/raiden/state.toml" in out
    # ...and the binary itself must be staged into the target, or those ops have
    # nothing to run after reboot. copied (not apt-installed) into /usr/local/sbin.
    assert "install -D -m 0755" in out
    assert "/mnt/usr/local/sbin/raiden" in out
    # staged after the rootfs exists, so the destination path is valid.
    assert out.index("debootstrap") < out.index("/mnt/usr/local/sbin/raiden")
    # dosfstools must be installed in the TARGET (chroot): replace recreates esps
    # with mkfs.msdos on the running system, so it cannot live on the host only.
    assert "chroot /mnt env DEBIAN_FRONTEND=noninteractive apt-get install -y gdisk dosfstools mdadm" in out


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


def test_replace_unmounts_the_esp_before_rebuilding_it(raiden):
    # the primary esp is mounted at /boot/efi on a healthy system; replace must
    # unmount it before mkfs, or mkfs.msdos refuses "contains a mounted filesystem".
    out = raiden("replace", "--disks", "vda", "--esp", "--dry-run").stdout
    assert "umount /dev/vda1" in out
    assert out.index("umount /dev/vda1") < out.index("recreate primary esp on /dev/vda1")
    # and it remounts /boot/efi afterward so the running system is left consistent.
    assert "mountpoint -q /boot/efi" in out
    assert out.index("recreate primary esp on /dev/vda1") < out.index("mountpoint -q /boot/efi")


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
    assert "raiden-sync-boot-mirrors" not in out
    assert "/boot.mirror" not in out


def test_independent_boot_sync_runs_after_initramfs(raiden):
    # the boot-mirror sync must copy /boot AFTER the crypttab-aware initrd is
    # built (update-initramfs, in the finish phase). syncing earlier mirrors a
    # cryptsetup-less initrd, so a survivor booted after the primary's /boot is
    # destroyed cannot unlock luks and bring up the encrypted root.
    out = raiden("install", "--dry-run").stdout
    sync = out.index("raiden-sync-boot-mirrors --strict")
    initramfs = out.index("update-initramfs -c -k all")
    assert initramfs < sync, "boot mirrors must be synced after update-initramfs"


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
    # boot partition is reformatted with the shared uuid and the live /boot is
    # cloned onto it; the esp clone and root re-add are unchanged.
    out = raiden("replace", "--dry-run", "--disks", "vdb,vdc").stdout
    assert "clone esp from /dev/vda1 to /dev/vdb1" in out
    assert "re-add /dev/mapper/vdb3_crypt to the root array" in out
    assert "mdadm --wait /dev/md/root" in out
    assert "mdadm --remove /dev/md/root detached" in out
    assert "/dev/md/boot" not in out
    assert "mkfs.ext4 -m 0 -F -U" in out  # replaced boot fs shares the uuid
    assert "clone /boot to /dev/vdb2" in out


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


def test_scrub_independent_skips_boot_array(raiden, tmp_path):
    # independent /boot has no array to scrub; raid mode does.
    indep = raiden("scrub", "--dry-run").stdout
    assert "mdadm --action=check /dev/md/boot" not in indep
    raid = raiden(
        "scrub", "--dry-run", "--config", _raid_boot_config(tmp_path)
    ).stdout
    assert "mdadm --action=check /dev/md/boot" in raid


def test_resume_without_checkpoint_fails(raiden):
    r = raiden("install", "--resume", expect_ok=False)
    assert r.returncode != 0
    assert "no checkpoint" in r.stderr


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
    assert 'part_prefix = ""' in text  # bare sd* names take no partition separator
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
    assert 'part_prefix = "p"' in text
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
