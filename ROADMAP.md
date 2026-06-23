# roadmap

```
[x] scaffold the raiden project (docs + config/dry-run skeleton)
[x] implement command execution (run for real, with confirmation guard)
[x] dm-crypt~md~lvm~ext4 stack: full phase command emission
[x] dm-crypt~btrfs stack: full phase command emission
[x] dm-crypt~zfs stack: full phase command emission
[x] dm-integrity~md~dm-crypt~lvm~ext4 stack: full phase command emission
[x] persist + load the install manifest; ops resolve from it
[x] status: port the md read-error to file-path mapping (badblocks)
[x] scrub for each stack
[x] rescue (assemble + unlock + mount from livecd)
[x] replace named disks, preserving esp + luks uuids
[x] remove + close
[x] efi esp mirroring + grub.d resync hook + luks header backup
[x] fine-grained checkpoint + resume from the last good step
[x] non-interactive password (--password-file / RAIDEN_PASSWORD)
[x] optional serial console on the installed system (serial_console)
[x] fast e2e tests under tests/ (planning, validation, resume)
[x] automated libvirt vm harness: serial-driven, no timers, graded report
[x] install-correctness fixes surfaced by real vm runs: non-interactive
    mdadm/passwd, strap-before-bind phase order, rsync in target for the
    esp-mirror grub.d hook
[x] esp layout: stable per-slot mounts (/boot/efiN) + /boot/efi symlink to the
    active primary, so a dead primary fails over by re-pointing the link
[x] harness: show the serial console in the run output (control chars stripped)
[x] badblocks: recognize uncorrectable errors ("(sector S on dev)") and map
    files only for those, not for corrected (repaired) errors
[x] harness: after corrupt 4/4 data, skip the in-guest survival/scrub (can hang
    or panic) and go straight to hard-reset + livecd rescue
[x] persist the manifest into the installed target (finish phase, /mnt mounted)
    so post-install status/scrub/replace resolve it; drop the dead per-disk uuid
    fields. fixes "disks.members is empty" and the cascading rescue failure
[x] raiden --verbose: echo the exact command per step; default on in the harness
[x] harness: rewrite the report after every check so ctrl-c leaves a partial one
[x] replace: wait for the /boot array rebuild too (not just root), so /boot is
    fully redundant before returning
[x] install dosfstools (mkfs.msdos) in the TARGET, not just the host: replace
    recreates a disk's esp on the running system. its absence made every replace
    fail in the partition phase, leaving arrays degraded and esps unbootable --
    the real cause of the corrupt_efiboot grub "no such device" cascade
[x] harness: replace()/scrub() check raiden's exit code and fail loudly, so a
    broken op can no longer read as PASS ("replaced and scrubbed")
[x] replace: clear vanished array members (mdadm --remove failed/detached) and
    settle udev before re-add, so a re-added member is not "busy"
[x] harness: disks use cache=none so the truncate-disk scenario actually wipes
    the guest's view -- writeback was resurrecting the zeroed pages, leaving stale
    md superblocks that auto-assembled and made replace's re-add fail "busy"

[x] independent (non-md) /boot, now the default (boot.raid=false): each disk
    carries its own ext4 /boot sharing one fs uuid, synced by a standalone script
    run from the kernel postinst.d/postrm.d hooks (after update-grub). each disk's
    grub boots from its local /boot with no array assembly, so first-disk loss
    still boots and replace is a plain mkfs+rsync. md raid1 /boot stays available
    via boot.raid=true. fixes the "no such device" boot after first-disk loss and
    the replace "busy" on the boot member
[x] harness: run a subset of the post-install scenarios (--scenario name[,name],
    --list-scenarios; Makefile SCENARIO=) for faster iteration on one scenario;
    --boot-raid selects the md /boot path (Makefile BOOT_RAID=1)
[x] harness: keep corrupt_efiboot out of the default bundled run (its boot
    failures would be confounded by the prior scenarios' accumulated state) and
    give it its own clean-install test (make test-vm-boot / --scenario
    corrupt_efiboot)
[x] independent /boot: run the install-time mirror sync in the finish phase, after
    update-initramfs builds the crypttab-aware initrd (not in bootloader, before
    crypttab exists). syncing too early shipped a cryptsetup-less initrd to the
    mirrors, so a survivor booted after the first disk's /boot was lost dropped to
    the initramfs shell (no luks unlock -> no md/root -> vg0-root missing)
[x] replace: udevadm settle after the crypt/dm teardown, before wiping and
    re-luksFormatting the partitions. udev frees the just-closed devices
    asynchronously, so without it wipefs/mkfs/luksFormat intermittently hit
    "device busy" on the freed partition -- exposed by replacing a member whose
    root layer was still healthy (eg. losing only its esp+boot, not p3)
[x] replace (independent /boot): remount /boot from a survivor when the live /boot
    sits on a disk being replaced. /boot mounts by the shared uuid so it can land
    on any disk; mkfs refuses a mounted device ("/dev/vdb2 is mounted; will not
    make a filesystem here"). fixes corrupt_headers replace failing on the disk
    that happened to back /boot
[x] rescue (md~lvm~ext4): activate the vg (vgchange -a y vg0) in map() after
    assembling md/root, before mounting /dev/vg0/root. udev auto-activation is
    unreliable for a freshly assembled (possibly degraded) array from a livecd, so
    the mount failed "Can't lookup blockdev". the dm-integrity stack already did
    this; md~lvm~ext4 had not

[x] examples/: a per-stack repository of complete, valid raiden configs
    (md-lvm-ext4, btrfs, zfs, md-integrity) that double as the vm harness
    fixtures. the harness loads examples/<stack>.toml and overlays only the
    test-specific keys (serial console on, member disks, /boot mode, sysbench);
    a --config flag (Makefile CONFIG=) points at any config. fixes the harness
    silently using the default aead crypt for stacks that want plain crypt
    (zfs/btrfs/md-integrity checksum or integrity-check elsewhere)

[x] align the root partition end to the crypt sector size. without aead,
    cryptsetup refuses a device whose size is not a sector-size multiple, and a
    partition run to the last usable sector is only 512-aligned -- so plain crypt
    (zfs/btrfs) with sector_size=4096 failed at luksFormat ("device size is not
    aligned"). raiden now rounds the partition end down to the sector size via
    sgdisk -E, keeping 4096 (4k-native ssd friendly) instead of dropping to 512

[x] harness: write the graded report to a timestamped file under tests/vm/
    reports/ by default (stack[-tag]-YYYYmmdd-HHMMSS.md) so every run is kept as
    history; OUT= / --out overrides, --tag labels (eg. boot)

[x] benchmark: restore the meaningful sysbench sizing AND run on the array, not
    tmpfs. raid-explorations regressed events 5000(rndwr)/20000(seqwr) -> 2000 and
    capped the file at 500M; raiden inherited the short version AND ran it under
    mktemp -d (/tmp), which recent systemd mounts as tmpfs (ram) -- so it
    benchmarked ram, not the raid stack, and 2g could not fit. now runs in
    /var/tmp (on the root fs) with the larger events + 2g working set, keeping the
    fsync-bound methodology, so the 95th percentile reflects durable array writes

[x] zfs on the livecd: build zfs-dkms against the RUNNING kernel, not the target
    kernel. installing linux-headers-amd64 on the host pulls the latest kernel
    headers, so dkms built the module for a kernel the livecd was not running and
    "modprobe zfs" then failed at the format phase. the host now installs
    linux-headers-$(uname -r); the target still gets linux-headers-amd64. (note:
    a livecd whose running kernel is so old its headers were dropped from the repo
    still cannot build zfs -- use a current live iso)

[x] replace must preserve each disk's LUKS uuid (R12), not just its esp uuid. a
    regression in the rewrite: raid-explorations stamped the original uuid back
    (common/dm-crypt.sh: luksUUID --uuid from /etc/crypttab in replace mode), but
    the rust port only carried the esp-uuid preservation. replace re-luksFormatted
    with a fresh uuid, so the installed /etc/crypttab still referenced the original
    and the first reboot AFTER a replace dropped to the initramfs (replaced members
    never unlock -> "cannot start dirty degraded array"). hidden because the
    in-place scenarios reboot BEFORE replacing and the livecd rescue opens crypt by
    device path (bypassing crypttab); only a post-replace normal boot hit it. fix:
    crypt_preserve_uuid stamps the disk's uuid from the running /etc/crypttab after
    luksFormat (no-op at install, load-bearing on replace) for every dm-crypt stack

[x] harness polish: name the example configs like raid-explorations'
    explorations/ (<stack>.<level>[.<variant>].toml, no config. prefix); keep the
    ~ in report filenames (<stack>[-tag]-<ts>.md); embed a small reproduction
    table (release, boot mode, /boot, members, crypt cipher/key/sector/integrity)
    at the top of each report

[x] use a current forky live iso for the harness, not a stable (trixie) one. the
    trixie detour was a workaround for the stale forky weekly (7.0.10, headers
    dropped); it caused a stable-live/forky-target split (zfs 2.3.2 vs 2.4.3,
    feature-flag skew). the forky weekly rebuilt (7.0.12), and the host-headers
    fix makes a current forky iso build zfs fine, so we stay on forky throughout

[x] btrfs crypttab: add the initramfs flag (luks,discard,initramfs,...) it was
    missing vs the md and zfs stacks, so the multi-device crypt root can unlock
    all its members at boot

[x] DRY: the per-disk dm-crypt stacks (md~lvm~ext4, btrfs, zfs) had byte-identical
    partition_root bodies; factor into super::crypt_partition_root

[x] harness: general-purpose initramfs follow-through. when a boot drops to the
    busybox rescue shell (root cannot mount), the console runs per-stack recovery
    commands and exits to resume -- covers the initial boot, scenario reboots, and
    the rescue flow. btrfs needs it: a multi-device btrfs root with a faulty member
    refuses to mount and requires a manual `mount -o degraded` (documented as a
    btrfs boot requirement)

[x] bug: dm-crypt~btrfs boots read-only. the root fstab line was captured with
    `grep btrfs /proc/self/mounts` (no chroot), recording the install mountpoint
    /mnt instead of /, so the installed system had no rw / entry and stayed
    read-only (systemd-remount-fs had nothing to remount). fixed with
    super::fstab_root_btrfs: writes the root line by uuid mounted at / (preserving
    the live mount options), matching the uuid-by-blkid pattern the other stacks
    use (R8)

[x] add a raid10 example (dm-crypt~md~lvm~ext4.raid10.aead.toml), mirroring
    raid-explorations' raid10 config, so the md stack is exercised at more than
    one raid level

[x] bug: harness hung after a degraded btrfs boot (two layers). (1) the serial
    getty prints its login prompt only after a carriage return, and over the
    console connection that survives the reboot a CR is ignored -- only a fresh
    connection re-asserts carrier. (2) the initramfs follow-through waited
    unboundedly for the (initramfs) prompt to reappear, but a recovery that mounts
    the root lets the boot resume with no further prompt. fix: _reach_login
    reconnects (fresh virsh console) and sends a bare CR, polling until the
    prompt; the recovery's per-command wait is bounded and tolerant. both bounded
    so a boot that reaches neither prompt is graded, not hung. validated e2e: the
    corrupt_headers degraded boot now logs in and replaces (all PASS)

[x] harness report: a report flushed before the run finishes (killed, hung, or
    aborted) read "OK" if nothing had failed yet, hiding that the run never
    completed. add a completed flag the runner sets only at the end of the flow;
    until then the summary is INCOMPLETE, never a silent OK

[x] harness: --skip-benchmark flag (make SKIP_BENCH=1) drops the costly sysbench
    pass from a run, keeping the resilience scenarios + rescue. the benchmark is
    ~26min on btrfs and orthogonal to correctness, so validation/troubleshooting
    runs should skip it

[x] report: WARN (not PASS) a reboot that only reached login because the harness
    intervened at the initramfs (ran the recovery commands, eg. a manual
    mount -o degraded) -- it did not boot unattended. stack-agnostic: any scenario
    needing boot intervention gets the warning. md/zfs assemble degraded
    automatically (clean PASS); btrfs needs it (WARN)

[x] harness: persist the full run log next to each report (report.md ->
    report.log) -- the workdir console.log is removed with the vm on cleanup, so
    the sibling .log is the durable record. add TAG= (fold a label into the report
    filename, eg. raid6 vs raid10) and make resolved_level read the config file so
    a --config run (raid10) is labelled with its actual level, not the default

[x] vm validation pass (forky iso), full runs with benchmark, one report+log each
    (all OK): dm-crypt~btrfs (raid1c3), dm-crypt~md~lvm~ext4 (raid6 + raid10),
    dm-crypt~zfs (raidz2). btrfs's degraded reboots are WARN (manual mount -o
    degraded needed); md/zfs auto-assemble degraded (clean PASS)

[x] ANALYSIS.md: compare the four runs (performance + resilience) and give
    per-use-case recommendations. headline: zfs raidz2 ~2-3x faster on fsync'd
    writes; btrfs alone needs manual intervention to boot degraded

[x] bug: dm-integrity install failed at `integritysetup format` ("reload ioctl
    ... No such file or directory"). the internal hash was xxhash64, which needs
    the xxhash crypto module -- absent from the live env and the integrity
    initramfs hook -- so the dm-integrity table load is rejected. fixed: default
    + example use crc32c (kernel built-in, the integritysetup default; what
    raid-explorations used). found by the should-pass dm-integrity vm run

[x] bug: bios install hung on an interactive `dpkg-reconfigure grub-pc` debconf
    dialog (inherited from raid-explorations; only bites an unattended install).
    fixed: preseed grub-pc/install_devices via debconf-set-selections (so it
    configures non-interactively), then the explicit per-disk grub-install does
    the authoritative install. found by the should-pass bios vm run

[x] should-pass coverage runs (all OK, full reports + logs): dm-integrity (raid6;
    found+fixed crc32c + unlock-prompt bugs), md raid1 /boot (BOOT_RAID), bios
    (found+fixed interactive grub-pc), corrupt_efiboot (first-disk-loss boot)

[x] new stack dm-crypt~md~lvm~xfs (parameterized MdLvm{fs}; ext4 + xfs share all
    crypt/md/lvm/replace/rescue logic). validated raid6 + raid10, both OK. xfs
    write perf ~= ext4 here (the aead crypt dominates the fsync benchmark)

[~] new stack dm-crypt~bcachefs (per-disk dm-crypt + multi-device bcachefs;
    redundancy by --replicas). implemented + plan-tested (config validate/dry-run
    pass): adds a Stack::apt_repos hook that installs apt.bcachefs.org (key +
    deb822 source, pinned below debian so only the out-of-tree dkms module comes
    from it), dkms + running-kernel headers like zfs, degraded-boot WARN +
    initramfs recovery like btrfs. BLOCKED at install on forky: the repo's
    bcachefs-tools (all suites) depend on libsodium23, but forky ships libsodium26
    and does not package bcachefs-tools natively, so the tools are uninstallable.
    upstream/library-transition skew, not a raiden defect; revisit when the repo
    rebuilds against forky's libs (or test on a release where they match). the
    stack code + example stay for that point. see ANALYSIS.md

[x] raiden init: a config generator to simplify onboarding on real hardware.
    discovers candidate whole disks (lsblk), excludes the removable/live medium,
    detects efi vs bios (/sys/firmware/efi) and the partition prefix (kernel rule:
    "p" when the disk name ends in a digit -- nvme0n1p1 vs sda1), and picks the
    stack-correct crypt block (aead aegis128 for md/lvm ext4+xfs; plain aes-xts
    where integrity lives in the fs or dm-integrity), mirroring examples/. emits a
    valid raiden.toml the user reviews before install. interactive by default;
    --stack/--level/--members/--boot-mode pre-answer any step and --non-interactive
    takes the detected defaults. refuses to clobber an existing file without --force

[x] single-command install: fuse the init generator into install. with no config
    file, `raiden install` generates a machine-appropriate, stack-correct config in
    memory (shared init::generate, honoring env+flag overrides) and provisions from
    it, so one command takes a bare live env to an installed system. a config file,
    when present, stays authoritative. also closes an R14 gap: install now confirms
    the erase and names the member disks before the destructive run (skipped by
    --yes, which the harness and the unattended one-liner pass). fixes the flag-only
    install silently keeping the aead crypt default for non-ext4 stacks

[x] distribution: `make dist` builds a fully static musl binary, and install.sh
    fetches it from the github release so a bare Debian live needs no rust
    toolchain -- `wget ...install.sh | sh` then `raiden install`. (publishing the
    release asset is the maintainer's job; never mutates version control)

[x] two live-environment entrypoints: install.sh (download the static binary) and
    livecd.sh (guided driver). the live flow is one line -- download then run --
    after which livecd.sh self-elevates with sudo (live images autologin a
    sudo-capable user), apt-installs screen, fetches raiden, and opens a screen
    session that runs init then install by default (RAIDEN_REVIEW=1 stops after
    init to review the config; install still confirms the erase + password).
    livecd.sh exposes init/install/rescue subcommands (RAIDEN_BIN override); the vm
    harness stages it and drives raiden through them (runner._INSTALL/_RESCUE), so
    the live flow and the tested flow share one definition of the raiden invocation
    -- no duplicated command strings. a vm-unit test pins the command contract;
    full path validated by a real make test-vm run

[x] release workflow (.github/workflows/release.yml): on a pushed `v*` tag, build
    the static binary via `make dist` (toolchain from mise.toml), gate on cargo
    test, and attach raiden-x86_64-linux-musl + its .sha256 to the github release
    (created for the tag, or the one published from the UI). preserves any
    release notes the maintainer wrote; tagging stays the maintainer's action

[x] bug: `raiden init` disk discovery presented members in lsblk's discovery order,
    not sorted, so eg. nvme2n1 appeared before nvme1n1 in the suggested members
    list. parse_disks now sorts discovered disks by name (lexical).

[ ] expand test coverage (see ANALYSIS.md "coverage gaps"):
    - dm-integrity~md~dm-crypt~lvm~ext4 (the untested 4th stack; dm-integrity
      below md vs aead above it)
    - ext4 without aead (plain aes-xts) as a baseline to isolate the aead write tax
    - BOOT_RAID=1 (md raid1 /boot) and corrupt_efiboot (make test-vm-boot): the
      independent-/boot first-disk-loss path is unverified in the current set
    - bios boot (all runs used efi); more raid levels (md raid1/5; btrfs raid1/10
      and raid5/6 with a write-hole note; zfs raidz1/raidz3)

[ ] run the vm harness against all four stacks on a kvm host and commit
    a result-YYYY-MM.md per stack (needs a libvirt host)
[ ] rescue boot: instrument the efi boot manager via send-key to select the
    cdrom, as an alternative to the cdrom-first domain re-create
[ ] remote unlock (dropbear-initramfs) so a degraded boot can be unlocked
    without console access
[ ] write cryptsetup keys to TPM
[ ] add integrity to /boot
```
