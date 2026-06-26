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

[x] harness: opt-out then opt-in benchmark control. originally a --skip-benchmark
    flag (make SKIP_BENCH=1) dropped the costly sysbench pass; later inverted so
    the benchmark is off by default (see below). the benchmark is ~26min on btrfs
    and orthogonal to correctness, so validation/troubleshooting runs skip it

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

[x] crypt.integrity_no_wipe (default false): pass --integrity-no-wipe to luksFormat
    for aead stacks, skipping the slow full-device integrity wipe. off by default;
    documented caveat that it leaves tags uninitialized (reads of unwritten sectors
    fail) and conflicts with the md array's initial resync.

[ ] expand test coverage (see ANALYSIS.md "coverage gaps"):
    - dm-integrity~md~dm-crypt~lvm~ext4 (the untested 4th stack; dm-integrity
      below md vs aead above it)
    - ext4 without aead (plain aes-xts) as a baseline to isolate the aead write tax
    - BOOT_RAID=1 (md raid1 /boot) and corrupt_efiboot (make test-vm-boot): the
      independent-/boot first-disk-loss path is unverified in the current set
    - bios boot (all runs used efi); more raid levels (md raid1/5; btrfs raid1/10
      and raid5/6 with a write-hole note; zfs raidz1/raidz3)

[x] idempotent install: a re-run tears down any existing stack on the members
    before partitioning. a new "reset" phase (after prepare, before partition)
    runs the stack's own best-effort teardown (unmount, then lvm/md/crypt or the
    zpool) and settles udev, so wipefs no longer fails "Device or resource busy"
    when a prior run's crypt/md/lvm still holds the disks. shares close_phase's
    teardown steps. md-backed stacks also sweep the array members' /sys holders,
    so an oddly-named (md127) array on the configured disks is stopped too, not
    only /dev/md/root by name.

[x] retry the encryption password prompt on mismatch/empty instead of aborting:
    re-asks until the entry is non-empty and (on install) matches its
    confirmation, so a typo no longer kills a long install. the root password is
    set by the system `passwd` in the chroot, which already re-prompts on mismatch.

[x] install the raiden binary into the target during finish, so post-install ops
    (status/scrub/replace/remove/close/rescue) work after reboot -- previously
    only the manifest was staged, leaving the tool that reads it absent. copies
    the running (static musl) binary to /usr/local/sbin/raiden; a dynamically
    linked dev build copied there may not run, but real installs use the musl one.

[x] `raiden benchmark`: port the harness's guest/benchmark.sh fsync-bound sysbench
    fileio workload into the binary. a `[benchmark]` config section (size, passes,
    rndwr/seqwr events) with flag overrides; `--dry-run` prints the exact sysbench
    plan (v1); a real run captures + parses each pass and prints a per-mode summary
    (avg total + avg p95), or `--format json` (v2). the vm harness scenario calls
    `raiden benchmark` instead of shipping the script, which is deleted.

[x] rename the badblocks module to bad_files and expose `status --bad-files`,
    which narrows status to just the unrecoverable-read-error file listing (md
    stacks). the full `status` still runs it after the array detail as before.

[x] option a boot/esp layout: drop the per-slot esp mounts (/boot/efiN), the
    /boot/efi symlink, and the /boot.mirrorN fstab entries. the primary member's
    esp mounts directly at /boot/efi (by uuid); every other esp and /boot copy is
    a mirror with no persistent mount point, resynced via transient mounts under
    /run/raiden by member-driven hooks (baked device list, not fstab). replace
    preserves only the primary esp uuid; mirrors get fresh uuids, re-populated by
    the hook. declutters / and /boot; recovery is via replace (rebuild in place).
    R5/R6/R8 reworded to match without regressing first-disk-loss survivability.

[x] replace --esp/--boot/--root (default all): rebuild only the named per-disk
    layers. a partial replace recreates just those partitions in place (no
    whole-disk zap) and leaves the others untouched -- so --esp --boot recovers a
    scribbled boot region without touching the root member (no resilver).
[x] `raiden mount [--boot] [--at DIR]`: ensure the stack is open + mounted
    (idempotent, guarded). full form opens crypt/assembles/activates/mounts under
    /mnt; --boot just mounts /boot + /boot/efi from the first available member at
    DIR (default /mnt; --at / for the running system). the boot-mount logic is
    shared (pipeline::boot_mount_steps) by install's bind, rescue, and mount, so
    all three tolerate a missing primary esp.

[x] migrate-boot-layout.sh: in-place migration of an existing old-layout install
    (per-slot /boot/efiN + symlink + /boot.mirrorN fstab) to option a. rewrites
    fstab to the single /boot/efi-by-uuid entry, remounts the primary esp there,
    installs the new member-driven mirror hooks, and resyncs. dry-run by default
    (APPLY=1 to apply); backs up fstab; idempotent.

[x] benchmark scratch-dir lifecycle: a Workdir RAII guard removes
    /var/tmp/raiden-benchmark on normal return, error (`?`), and panic (a failed
    pass used to leak the 2g working set); `new` clears a stale dir from a prior
    killed run. a signal-hook thread (new dep, leaner than ctrlc) removes it and
    exits 130 on SIGINT/SIGTERM, which would otherwise skip the guard's drop.
    validated live: workdir removed on SIGINT, exit 130. progress + child chatter
    go to stderr so `--format json` stdout stays clean.

[x] bug: replace of a healthy disk failed at "recreate primary esp" with
    "mkfs.msdos: /dev/.. contains a mounted filesystem" -- the primary esp is
    mounted at /boot/efi on a running system, and the best-effort wipefs silently
    skipped it before the (hard-fail) mkfs. fixed: replace now unmounts each
    rebuilt disk's esp before wipefs/mkfs, and remounts /boot + /boot/efi from the
    first available member afterward (pipeline::boot_mount_steps) so the running
    system is left consistent. the corrupt_efiboot vm passed only because there
    the esp is destroyed (unmounted) before replace.

[x] verify the encryption password (prompt twice, retry on mismatch) for replace,
    not just install: both luks-FORMAT a disk with it, so a typo silently makes a
    member with a mismatched password that fails to unlock at boot. open-only ops
    (rescue, mount) still prompt once (a wrong password just fails to unlock).
[x] vm scenario replace_primary: replace the PRIMARY disk's --esp --boot on a
    healthy system (its esp mounted at /boot/efi) -- the only coverage for the
    mounted-esp unmount fix (corrupt_efiboot destroys the esp first; the others
    replace non-primary unmounted-mirror disks). selectable, excluded from the
    default bundle.

[x] bug: `--resume` with a different destructive scope (eg. replace --disks/
    --parts changed from the original run) silently reused the checkpoint cursor
    against a different plan and skipped the wrong steps (left a crypt unopened,
    then mdadm --add "No such file"). fixed: the checkpoint now records the op's
    scope (disks + layer flags), and resume refuses if it changed -- "run without
    --resume to start fresh". unit-tested.

[x] resume e2e coverage: RAIDEN_CHECKPOINT overrides the checkpoint path so the
    tests are hermetic. cli e2e now covers no-checkpoint, --resume + --from/--only
    rejection, and resuming a checkpoint from a different operation (a real
    non-dry-run invocation that bails at validation before touching disks). the
    scope/config-hash/skip-steps logic is unit-tested (resume_refuses_a_changed_
    scope, config_hash_changes_with_config, resume_skips_already_applied_steps).

[x] bug: replace's /boot reformat hardcoded blkid on the first survivor; if that
    disk's /boot had no uuid (eg. it was rebuilt earlier without /boot), mkfs -U ""
    failed "could not parse UUID". fixed: read the shared uuid from fstab (the
    /boot entry, canonical), fall back across all survivors, and mkfs a fresh uuid
    only if every /boot copy is gone -- never fail on an empty uuid.

[x] bug: the boot-mirror kernel hook exec'd `raiden sync boot --yes "$@"`, but
    run-parts invokes postinst.d/postrm.d with the kernel version + bootdir as
    positional args, which `sync boot` rejects (clap exit 2). since the hook
    propagates its exit code, every post-install kernel/initramfs upgrade would
    fail. fixed: the hook no longer forwards "$@" (sync always mirrors the whole
    /boot); the pointless "$@" is also dropped from the esp grub.d wrapper. guarded
    by a unit test (boot_mirror_hook_does_not_forward_runparts_args) and an e2e
    assertion in the install plan.

[x] bug: the vm harness's Console.run() returned the raw serial output, so
    debian's bash shell-integration osc markers leaked into parsed values -- the
    sync_mirrors scenario read /boot's source device wrapped in escapes, never
    matched a member, and corrupted the live /boot instead of a mirror (a false
    FAIL; raiden correctly refused to mirror a broken source). fixed: run() strips
    terminal control sequences before returning, which also de-junks the graded
    report details. regression-tested in test_log.py.

[ ] run the vm harness against all four stacks on a kvm host and commit
    a result-YYYY-MM.md per stack (needs a libvirt host)
[ ] rescue boot: instrument the efi boot manager via send-key to select the
    cdrom, as an alternative to the cdrom-first domain re-create
[ ] remote unlock (dropbear-initramfs) so a degraded boot can be unlocked
    without console access
[ ] write cryptsetup keys to TPM
[ ] add integrity to /boot

[x] manifest rename: state.rs -> manifest.rs, State -> Manifest,
    state.toml -> manifest.toml. atomic write (tmp+rename). /boot becomes the
    canonical load path (reachable from a livecd without unlocking root), /etc
    the mirror; load tries /boot first. clean break from state.toml with a
    helpful rename hint on load failure. no behavioral change beyond the path
    flip.

[x] drop part_prefix: remove disks.part_prefix from config and Layout. derive
    the partition separator per disk via the kernel rule ("p" when the disk name
    ends in a digit -- nvme0n1p1 vs sda1), so mixed nvme+sd arrays work (one
    global prefix was broken for them). init stops emitting part_prefix and the
    mixed-member warning; examples and tests drop it.

[x] sync-boot subcommand: replace the standalone shell script with
    `raiden sync boot`. the kernel postinst.d/postrm.d hooks (and the install
    finish phase) call it via a thin wrapper. no-ops when boot.raid is set (mdadm
    handles replication). pre-sync validation (fsck, grub.cfg/kernel/initrd
    presence, initrd contains cryptsetup) on the source /boot; interactive
    confirmation unless --yes; per-mirror rsync; per-mirror best-effort-continue
    with a non-zero exit if any failed. items 4 (validation) and 5 (confirmation)
    land here.

[x] sync efi + unified sync: migrate the esp grub.d hook's rsync logic into
    rust as `raiden sync efi` (paired with `raiden sync boot`, sharing the
    transient mount->rsync->unmount helper). source verification is ON by
    default for both (boot: e2fsck/grub.cfg/kernel/initrd+cryptsetup; efi:
    shimx64/grubx64 present), disabled with --force (used by no script). on a
    verify failure, report and exit non-zero without syncing. install finish
    phase runs `raiden sync efi --yes` (raiden is staged after the bootloader
    phase's update-grub, so the grub.d hook no-ops at install). runtime grub.d
    wrapper swallows the exit code so a verify/mirror failure never blocks
    grub.cfg regeneration; the postinst.d boot wrapper propagates its exit code.

[x] drop `--force` from the install-time `sync efi` step. resolved by installing
    grub in both modes (named `EFI/debian/` + removable `EFI/BOOT/`), so the
    primary esp has `shimx64.efi` from first boot and verification passes without
    `--force`. the debconf keys `grub2/force_efi_extra_removable=true` and
    `grub2/update_nvram=true` keep both layouts + nvram in sync on future
    grub/kernel upgrades.

[ ] `raiden mirror <src> <dst>`: block-level (dd) copy of one device onto
    another, out of band -- a rescue primitive for fully synchronizing a disk's
    /boot (or esp) onto a replacement when the structured `sync` flow cannot
    (a member won't mount, or cloning a healthy partition onto a freshly-wiped
    disk before re-adding it). dd preserves the shared /boot fs uuid natively,
    so the target's grub finds its local copy with no uuid gymnastics. requires
    dst >= src; wipes the dst signature first; prompts unless --yes; refuses a
    mounted src/dst unless --force (used by no script). no manifest/layout --
    raw device paths, genuinely complementary to the structured sync commands.

[x] `raiden doctor`: comprehensive health checks (bolt-on, hand-written). a
    per-check table with status (ok/warn/fail) and detail: disk presence, boot
    + esp mounts, fstab, crypttab, luks headers, raid/zfs/btrfs status, boot +
    esp mirrors (presence + drift), grub install, initrd + cryptsetup, kernel +
    esp hooks (presence + executable bit), manifest. resolves from the manifest
    like the other post-install ops. exit 0 iff all pass, non-zero if any fail.
    read-only by default; `--fix` installs missing/unexecutable mirror hooks and
    re-syncs drifted mirrors (safe, idempotent). no checkpoint.

[x] `raiden doctor --fix`: repair the auto-fixable checks in place. installs the
    boot-mirror kernel hooks (postinst.d/postrm.d) and the esp-mirror grub.d hook
    when missing or non-executable (the static wrapper content from
    stack::BOOT_MIRROR_HOOK_CONTENT / EFI_MIRROR_WRAPPER, 0755), and re-runs
    `raiden sync boot`/`sync efi` to repair drifted mirrors. each fix is reported
    as a `fixed` status row; a fix that errors leaves the original status and
    notes the error. drift is a warn (the system still boots from any copy), not
    a fail. destructive checks (fstab, crypttab, luks, array state, grub-install)
    are never auto-fixed -- they point at `replace`/`sync`/`grub-install`.

[x] doctor: boot/esp drift checks. mirror presence checks only verified the
    device node existed; a present-but-never-synced mirror (eg. a prior sync
    failed silently under the esp grub.d hook's swallowed exit code) reported
    ok. now each mirror is transient-mounted read-only and compared via `rsync
    --dry-run --itemize-changes` against the live source; any changed path is a
    warn, fixable by `--fix`. shares the transient mount/unmount helper with
    `raiden sync` (DRY: mount_transient/unmount_transient/drift).

[x] doctor: note /boot and /boot/efi mounted from different disks. this is
    expected and benign (/boot mounts by the shared fs uuid so any survivor can
    serve it; /boot/efi mounts by the primary esp's unique uuid). surfaced so
    an operator is not surprised -- no action needed.

[~] keep PARTUUID in sync across mirrors: DECLINED. nothing in raiden keys off
    the gpt partition GUID (fstab uses the fs uuid, crypttab the luks uuid, grub
    the fs uuid), so forcing it equal adds cost (sgdisk -u on a mounted
    partition, duplicate GUIDs confusing lsblk/blkid, more replace/sync
    complexity) for zero boot-path gain. filesystem uuids are shared across
    mirrors for both /boot and the esp (load-bearing for fstab + grub search and
    the /boot/efi fstab entry respectively). the esp fs-uuid sharing was
    originally declined here and later reversed -- see the "esp shared fs uuid"
    entry above. PARTUUID (the gpt partition GUID) remains distinct: no consumer
    uses it, and sharing it adds the same resolution cost for no gain.

[x] vm harness: doctor + sync_mirrors scenarios. `doctor` runs `raiden doctor`
    on the healthy installed system and fails on any non-passing check. `sync
    mirrors` dry-runs + real-syncs `sync boot`/`sync efi`, then corrupts a
    non-primary mirror's /boot and confirms a re-sync restores it (skipped under
    boot.raid). both are in the default scenario bundle so every regression pass
    covers the new surfaces; selectable via --scenario.

[x] replace --with: physical disk swap. `raiden replace --disks=a,b --with=c,d`
    (paired by position) swaps the named members for new physical disks,
    mutating the manifest's members list (a->c, b->d; primary stays primary).
    the new disks take the old disks' esp uuid (primary, so the /boot/efi fstab
    entry stays valid) and the shared /boot uuid; crypttab is regenerated for the
    new crypt names (so the fresh luks uuid needs no stamping). --with is
    optional: without it, replace stays the in-place rebuild (backward
    compatible). the old disks are detached best-effort (may be gone) and not
    wiped; the new disks are wiped + provisioned.
```

## esp shared fs uuid

[x] share the esp fs uuid across mirrors (as /boot already does). today every
    esp gets a fresh vfat uuid and only the primary's is load-bearing (the
    single /boot/efi fstab entry). mirroring the /boot design -- one shared esp
    fs uuid stamped on every member at format time and preserved across replace
    -- lets /boot/efi mount from any survivor (the fstab entry degrades to a
    mirror if the primary is lost, instead of going dead until replace runs).
    the concerns raised when this was previously declined (blkid/lsblk
    ambiguity, two same-uuid filesystems mounted at once) already apply to /boot
    and are tolerated there because nothing resolves /boot by uuid at runtime
    except fstab + grub search, both happy with "any device having this uuid";
    the esp sync hook already addresses mirrors by device path, so the same
    holds. firmware boot is unaffected (efibootmgr targets disk+partition, not
    uuid). drops the primary-only esp uuid-preservation special case in replace.
    REQUIREMENTS R5/R8 and the replace e2e tests (test_replace_unmounts...,
    test_replace_with_adopts_the_shared_esp_and_boot_uuid) updated to match.
```

[x] doctor: uuid-sharing check + --fix re-stamp. verify every member's /boot
    shares one ext4 uuid and (efi) every esp one vfat uuid, matching the fstab
    entry -- a member with a divergent uuid silently cannot serve the mount if the
    primary is lost (a loss of redundancy the content-drift checks miss). warn (the
    system still boots from the primary). `doctor --fix` re-stamps the divergent
    mirrors to the shared uuid and re-syncs, which doubles as the one-shot
    migration for a legacy host installed before the shared esp uuid. the re-stamp
    is a reconcile (re-observes live state at apply time, requires the source
    mounted, never touches the source, skips already-shared); /boot in place via
    tune2fs, esp reformatted with the shared volume id then repopulated by the
    sync. also refined the grub check to verify the bootloader on whichever esp is
    actually mounted (shared uuid means /boot/efi can land on any survivor), not an
    assumed primary. the check is split predicate/presentation (pure uuid_set_result
    decides ok/warn+fix; I/O collection + table are separate) -- the first concrete
    step toward the check/fix split below. unit-tested (uuid_divergences,
    uuid_set_result) + e2e (doctor check-name list).

[x] doctor --fix: confirm each fix individually (was: apply all unprompted). each
    fix prints what it will do (check name + detail + action) and asks; --yes
    auto-accepts, a declined fix is left in place and noted. DRY'd the prompt/--yes
    rule into prompt::confirm_or_yes, shared by sync and doctor (and any future
    caller) so "--yes assumes yes, otherwise ask" lives in one place.

[x] doctor --fix --dry-run: preview each repair instead of applying it -- the exact
    commands + target devices (which mirror gets reformatted by the re-stamp), no
    prompts, nothing written. the look-before-you-leap path before running --fix on
    hardware. re-stamp argv factored (restamp_argv) so preview and apply share it.
    the vm doctor_fix scenario asserts the re-stamp preview + that the esp is
    unchanged.

[x] doctor --fix --dry-run prints the fix FLOW, not the checks table: each fixable
    check followed by the exact commands it would run, in order (mkfs.msdos -i per
    divergent mirror, then the re-sync), honoring --yes in the wording. the table
    was the wrong view for a preview -- you want to see exactly what executes
    before running the destructive re-stamp on hardware. restamp target-resolution
    factored (restamp_targets) so the preview and the apply agree exactly.

[x] doctor: efibootmgr check + fix; raiden owns the nvram. new `efibootmgr` check
    (efi) verifies each member disk has exactly one boot entry that loads shim from
    its own esp (matched by the esp PARTUUID, NOT the fs uuid -- the two are
    distinct, so a uuid re-stamp needs no efibootmgr change), flagging members with
    none and duplicate/shim-bypassing-grub cruft. `--fix` reconciles: prune the
    stale/duplicate raiden entries (those referencing a member esp + EFI/debian) and
    register one clean shim entry per member, leaving the removable EFI/BOOT fallback
    and non-member entries alone. install now preseeds grub2/update_nvram=false (was
    true) so grub's postinst stops adding its own entries on every upgrade -- the
    source of the accumulated cruft; raiden manages the named entries exclusively.
    pure parser unit-tested; vm doctor scenario enumerates the new check.

[x] doctor: drop the "-- expected (shared uuids); no action" editorializing from the
    mount-consistency line; just report which disk each of /boot, /boot/efi is on.

[x] post-install ops require an install manifest. doctor/sync/status/scrub/replace/
    remove/close now refuse (up front, before inspecting or mutating anything) when
    there is no manifest and it is not a --dry-run -- so a stray raiden.toml in cwd
    can never make eg. `doctor --fix` write hooks to a non-install host. install,
    config, init, devices keep the config fallback (pre-install); rescue/mount are
    exempt (livecd recovery via --config); --dry-run previews are exempt.

[x] tests: retire the host-invoking hermetic tests. `doctor` and `sync ... --dry-run`
    (non-raid) inspect the live host (findmnt/blkid/mdadm/efibootmgr), so they no
    longer run in the hermetic suite -- which is now pure plan-generation against a
    config, safe to run on any host. the live behaviour is covered in the controlled
    vm (sync_mirrors, doctor, doctor_fix scenarios; the doctor scenario enumerates
    every check name). a new hermetic test confirms the manifest guard bails up
    front (so it inspects nothing).

[x] consolidate the EFI bootloader surface (was duplicated across install/replace/
    doctor). new src/efi.rs holds the one canonical form of each: SHIM_LOADER /
    SHIM_FILE / GRUB_FILE paths (were ~6 spellings across ops/pipeline/doctor/sync),
    register_argv (the `efibootmgr -c -g -d ... -l shim` builder, was built 3x in
    pipeline/ops/doctor), and GRUB_DEBCONF + grub_debconf_selections (the grub-efi
    preseed keys, the single source for the install step AND the new doctor check).
    folded the duplicate initrd lsinitramfs|cryptsetup probe into one
    sync::initrd_has_cryptsetup (was in both sync + doctor), and dropped doctor's
    duplicate strs(). generated output unchanged (planning tests + 50 unit tests
    green). a first concrete step of the check/establish convergence: install and
    doctor now register/stamp/preseed through the same primitives.

[x] doctor: debconf check + fix (raiden owns grub's debconf). verifies grub-efi's
    force_efi_extra_removable=true / update_nvram=false on the installed system (a
    dpkg-reconfigure or an older restore silently flips them back, dropping the
    EFI/BOOT fallback or re-accumulating nvram cruft); `--fix` re-sets them via the
    same efi::grub_debconf_selections the install preseeds. pure parser unit-tested;
    vm doctor scenario enumerates it.

[x] consolidation pass 2: the esp mkfs.msdos is now one builder (efi::mkfs_esp_argv)
    shared by install's format, replace's rebuild (both shell sites), and doctor's
    re-stamp (the argv site) -- the flags lived in 5 places. and the install/replace
    bootloader postcondition (verify_marker_step) now checks shim AND grub on each
    esp, the same criteria as doctor's esp-bootloader check (was shim-only, a silent
    divergence). output unchanged where tested; 50 unit + 67 hermetic green.

## doctor --fix parity with install/replace/sync

[x] full parity: doctor --fix can now repair every invariant install/replace
    establish. Step::execute is the lever -- a fix builds the SAME establish step
    install/replace do (parameterized for the running system) and runs it. added:
    - fstab fix (append the missing /boot or /boot/efi UUID= entry, idempotent).
    - crypttab fix (stack.crypttab_regen -- the exact builder replace uses).
    - grub-install fix (efi::grub_install_steps(false) -- the same two grub-installs
      install runs in the chroot, extracted to crate::efi and shared).
    - initrd fix (update-initramfs -u).
    - luks header backup: a new check (each member's header is backed up to
      /boot/luks) + a fix (stack::backup_luks_headers, now parameterized by target so
      install uses /mnt/boot and the fix /boot). cryptsetup refuses to overwrite, so
      the fix clears the dir first.
    install/replace plan output unchanged (the extractions are pure refactors); 50
    unit + 67 hermetic green; vm doctor scenario enumerates the new luks-backup check.

[ ] remaining establish/check duplications to collapse (lower value; the fixes work,
    but two of them re-implement the establish form rather than sharing it):
    - fstab: the fix appends entries via its own shell; install writes them via
      boot_fstab_step/esp_fstab_steps. lift the line format (mount point, fs, opts)
      to shared constants so the install write and the doctor ensure agree.
    - /boot uuid stamp: install/replace mkfs.ext4 -U vs doctor tune2fs -U (two
      mechanisms; reconcile or share the "set this uuid" intent).
    - verify mechanism: the install/replace postcondition is a shell `test -e` step
      while doctor's bootloader check is rust verify_efi/verify_boot_files -- same
      criteria now, but two implementations.

[x] bug: replace's boot-region clone could wipe a rebuilt mirror. the esp clone
    `mount -o ro $src; mount $dst; rsync --delete $src/ $dst/` was unguarded -- a
    failed/empty source mount let `rsync --delete` run against an empty source and
    delete everything on the destination, leaving a rebuilt esp with no shim/grub.
    under shared esp uuid /boot/efi then mounts from that empty mirror and the
    named bootloader path (and `sync efi` verify) breaks. caught by the vm run
    (sync_mirrors: shimx64.efi missing on /dev/vdb1 after replace cycles). fixed:
    one guarded clone_partition helper, shared by esp + /boot, runs rsync --delete
    only after the source mounted AND carries its bootloader marker (shimx64.efi /
    grub.cfg), then verifies the marker landed on the destination -- no empty-source
    wipe, fails loudly instead of shipping a broken copy. (block-copy/dd was
    considered and rejected for the routine path: worse for the large ext4 /boot,
    couples partition sizes, risks an inconsistent image off a mounted source; it
    stays the separate `raiden mirror` rescue primitive for an unmountable fs.)
    follow-up: a per-member "every esp/boot carries its bootloader" postcondition
    on install/replace (and the matching doctor check), so an incomplete mirror
    can never be shipped silently even if a future clone path regresses.

[x] per-member bootloader postcondition + doctor check (the follow-up above). new
    doctor checks `esp bootloader` / `boot bootloader` transient-mount EVERY member
    read-only and verify it independently carries its bootloader (shim+grub /
    grub.cfg+kernel+initrd), not just the live mount -- the drift check trusts the
    source and so cannot catch a mounted-but-broken member. reuses
    sync::verify_efi / a new no-fsck sync::verify_boot_files; warn, fixable by a
    re-sync. install + replace now append a `verify_bootloaders` postcondition step
    (ops::verify_bootloaders) that fails loudly if any member esp/boot lacks its
    marker -- catching an incomplete mirror at creation time, the exact failure the
    vm caught. planning-tested (install/replace --dry-run) + the vm doctor scenario
    enumerates every check name in the controlled env.

[x] vm harness: doctor_fix scenario covering the doctor/fix flows added in (and
    since) the staged changes. exercises `doctor --fix` end to end on the running
    system: a removed boot-mirror kernel hook is reinstalled; a mirror esp whose fs
    uuid is skewed off the shared one is detected (warn), declined (no --yes leaves
    it skewed -- proves the per-fix prompt gates the destructive re-stamp), then
    re-stamped to the shared uuid with --yes (the legacy-host migration), with a
    post-fix doctor confirming the divergence is gone. added to INPLACE +
    DEFAULT_INPLACE (cheap, runs every pass). [ ] still to add: a doctor check +
    scenario for the per-member "every esp carries shim" postcondition above.

## long-term: idempotent install / replace / doctor

[ ] evolve install, replace, and doctor toward idempotent, convergent behavior:
    re-running an op (or resuming it) should observe the current state and do only
    what is missing, leaving an already-correct system untouched. two candidate
    shapes, not mutually exclusive:
    - state tracking: the manifest records desired state and ops record observed
      state (uuids, the member->partition map, which layers are live); an op diffs
      desired vs observed and acts on the delta. the manifest + checkpoint cursor
      are the seed of this.
    - check-build-check (reconcile): each step is a (precondition check -> build if
      needed -> postcondition check) triple, and doctor's checks ARE the predicate
      library. an op becomes "for each step: if the check passes, skip; else build,
      then re-check". doctor --fix is already a one-shot version of this loop.
    cheap places that already point this way -- lean into them rather than fight:
    - keep observed-state probes pure and reusable (eg. doctor's uuid_divergences,
      and sync::verify_boot/verify_efi already shared by sync + doctor) rather than
      doctor-only, so install/replace can call the same predicate as a guard and as
      a postcondition.
    - prefer idempotent step primitives + skip-if-satisfied guards (mkfs only when
      the uuid is wrong, mdadm --add only when not already a member, rsync already
      converges) so a re-run is a no-op wherever nothing changed.
    - the manifest is the natural home for observed identifiers; recording the
      resolved uuid + member map at install would let replace converge without
      re-probing every device.
    not scheduled; captured so new code (checks, steps, manifest fields) is shaped
    to make this a refactor, not a rewrite.

[x] doctor: initrd binaries check. verify the initrd carries everything needed to
    unlock and mount (or recover) the root at boot: the decrypt_keyctl keyscript +
    its keyctl, cryptsetup, and the stack's assemble/mount tools (mdadm/lvm, btrfs,
    zpool/zfs, bcachefs, integritysetup). the per-stack list is one source of truth
    (Stack::initramfs_binaries -- a common dm-crypt base plus the stack's tools),
    reusable as an install postcondition. fix is update-initramfs -u (the stock
    initramfs hooks pull the tools in from the installed packages -- raiden adds NO
    hooks of its own). lsinitramfs probe shared with sync (sync::initrd_listing);
    basename-matched so keyctl vs decrypt_keyctl are distinct. unit-tested.

## initramfs recovery (raiden recover at boot)

[x] manual `raiden recover` + bake raiden (static musl, ~3.4MB) + the manifest into
    the initrd, so a degraded boot can continue from the rescue shell -- generalizing
    the per-stack manual `(initramfs)` commands (eg. btrfs `mount -o degraded`) the vm
    harness ran (N8) into one command. config-guarded `install.initramfs_recovery`
    (default on). implementation:
    - a new raiden initramfs hook (the one raiden-added hook, required because no
      stock hook pulls in raiden or the manifest) copies the binary to /sbin and the
      manifest to /etc/raiden inside the initrd, where Manifest::load finds it with
      neither /boot nor the root mounted. install rebuilds the initrd once more after
      staging the binary + manifest, then mirrors it to every /boot.
    - structured as check/fix like doctor: recover observes whether the root is
      mounted at the target (default /root, the initramfs convention), and if not
      runs the stack's recovery actions -- each confirmed via prompt::confirm_or_yes
      (--yes escape hatch). crypt members are already open (cryptroot +
      decrypt_keyctl), so the actions pick up at the array/mount layer.
    - per stack (Stack::recover_actions): md/lvm + dm-integrity `mdadm --run` +
      `vgchange -ay` + mount /dev/vg0/root (md auto-assembles; --run kicks a stalled
      degraded array); btrfs/bcachefs `mount -o degraded` from a surviving member;
      zfs `zpool import -f` + `zfs mount` (rarely needed -- zfs auto-imports).
    - doctor checks both halves when initramfs_recovery is on: `recover hook` (the
      hook is installed + executable, so a FUTURE rebuild keeps baking raiden in --
      a removed hook silently drops it on the next update-initramfs, like the mirror
      hooks) and `recover` (the initrd currently carries raiden + the manifest). the
      fix INSTALLS the hook (no stock hook bakes raiden, so a plain rebuild cannot
      add it on a legacy install) and rebuilds.
    - install AND the doctor fix establish the bundle through the SAME shared steps
      (stack::raiden_recovery_hook_step parameterized by root, stack::
      update_initramfs_u parameterized by chroot), so the establish and the verify
      cannot drift -- the convergence pattern the other doctor --fix repairs follow.
    - the harness runs `raiden recover --yes` for every stack's initramfs follow-
      through (was per-stack `mount -o degraded`), so the degraded btrfs/bcachefs
      reboots exercise recover end to end.

[x] vm harness: benchmark off by default; opt in with BENCH=1 / --benchmark
    -- the sysbench fileio pass is ~26min and orthogonal to correctness, so a
    plain run is resilience-only and perf reports opt in. inverts the old
    --skip-benchmark flag.

[x] vm harness: throttle send-key (live/install phase). `virsh send-key` fired
    one chord per character with no --holdtime and no inter-key gap, so on a busy
    guest modifier+key chords raced -- shift stuck/dropped and characters garbled
    (observed: the rescue command was mistyped, the guest never ran it, the run
    hung until manual intervention). fix: --holdtime per chord (HOLDTIME_MS) + a
    delay between chords (KEY_INTERVAL_S) in sendkeys.py. validated by a clean
    unattended dm-crypt~btrfs run: install + rescue live phases typed without
    garble, rescue PASS (exit=0), no manual help.

[x] recover: drop the redundant `mountpoint -q` tail from the btrfs/bcachefs
    degraded-mount steps. `mountpoint` is absent in the initramfs, so the step
    logged a spurious "sh: mountpoint: not found / ! command failed" even though the
    mount succeeded; recover already judges success by the is_mounted postcondition
    (/proc/mounts), so the tail was dead weight. observed in the clean btrfs vm run.

[ ] automatic recovery: a local-premount initramfs hook that runs `raiden recover`
    to self-heal before the init panics (the manual mode above is the operator-run
    first step). safety stance on degraded auto-mount: ro first, log loudly, maybe
    only after a retry -- btrfs/bcachefs refuse degraded by design to avoid
    divergence, so an unattended degraded rw mount is the risk to guard.
