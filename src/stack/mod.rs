// stack trait, selection, and the steps shared by every stack. type-safe
// dispatch replaces the predecessor's symlink-and-probe convention; the shared
// helpers are the typed equivalent of the old common/ shell libraries.

mod bcachefs;
mod btrfs;
mod md_integrity;
mod md_lvm;
mod zfs;

use anyhow::Result;

use crate::config::{
    Config, STACK_BCACHEFS, STACK_BTRFS, STACK_MD_INTEGRITY, STACK_MD_LVM_EXT4, STACK_MD_LVM_XFS,
    STACK_ZFS,
};
use crate::layout::{Layout, ROOT_MD_DEVICE, ROOT_MD_NAME};
use crate::step::Step;

// gpt addresses disks in 512-byte logical block addresses regardless of the
// crypt sector size, so a sector size in bytes is this many lba sectors.
const LBA_BYTES: u32 = 512;

/// a recovery action: a labeled, ordered group of steps that brings one layer of
/// a degraded root online from the initramfs. `raiden recover` checks (is the root
/// mounted?), confirms each action (unless --yes), then runs its steps. defined
/// here with the trait it belongs to.
pub struct RecoverAction {
    pub label: String,
    pub steps: Vec<Step>,
}

impl RecoverAction {
    pub fn new(label: impl Into<String>, steps: Vec<Step>) -> Self {
        Self {
            label: label.into(),
            steps,
        }
    }
}

pub trait Stack {
    fn id(&self) -> &str;
    /// debian packages the stack needs in the target system.
    fn packages(&self) -> Vec<String>;

    // install pipeline
    /// create the root partition and bring up its encryption/integrity layer.
    fn partition_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step>;
    /// assemble the array and create the root filesystem.
    fn format_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step>;
    /// mount the root filesystem at /mnt (and any post-mount setup).
    fn mount_root(&self, cfg: &Config, layout: &Layout) -> Vec<Step>;
    /// crypttab, fstab, initramfs, and luks header backup inside the target.
    fn finish(&self, cfg: &Config, layout: &Layout) -> Vec<Step>;

    // operations
    /// unlock and assemble the array, for rescue from a livecd.
    fn map(&self, cfg: &Config, layout: &Layout) -> Vec<Step>;
    /// native health report commands (md detail, zpool status, btrfs show).
    fn status(&self, cfg: &Config, layout: &Layout) -> Vec<Step>;
    /// scrub the array.
    fn scrub(&self, cfg: &Config, layout: &Layout) -> Vec<Step>;
    /// re-add the named replacement disks to the array.
    fn replace(&self, cfg: &Config, layout: &Layout, disks: &[String]) -> Vec<Step>;
    /// detach the named disks from the array and tear down their mappings.
    fn remove(&self, cfg: &Config, layout: &Layout, disks: &[String]) -> Vec<Step>;
    /// unmount and tear down all of this stack's mappings.
    fn close(&self, cfg: &Config, layout: &Layout) -> Vec<Step>;

    /// the ordered recovery actions that bring this stack's already-unlocked root
    /// online from the initramfs and mount it (degraded/forced) at `at`. crypt
    /// members are already open by the initramfs (cryptroot + decrypt_keyctl), so
    /// these pick up at the array/mount layer. used by `raiden recover` -- each is
    /// confirmed, then run, with the postcondition being root mounted at `at`.
    fn recover_actions(&self, cfg: &Config, layout: &Layout, at: &str) -> Vec<RecoverAction>;

    /// regenerate /etc/crypttab on the running system for the given layout, when a
    /// `replace --with` swap changed the member (and thus crypt) names. None when
    /// the stack's crypttab does not depend on member names (eg. md_integrity,
    /// whose crypttab references the md array uuid). default: None.
    fn crypttab_regen(&self, _layout: &Layout) -> Option<Step> {
        None
    }

    /// binaries the initrd must carry to unlock and mount (or recover) this stack's
    /// root at boot. the common base (the decrypt_keyctl keyscript + its keyctl, and
    /// cryptsetup) plus the stack's assemble/mount tools. the single source for
    /// doctor's initrd check (and reusable as an install postcondition); the stock
    /// initramfs hooks pull them in -- raiden adds no hooks of its own.
    fn initramfs_binaries(&self) -> Vec<&'static str> {
        crypt_initramfs_binaries()
    }

    /// apt pin file content for backports, when the stack needs specific pins.
    fn backports_pins(&self, _release: &str) -> Option<String> {
        None
    }

    /// apt steps to run on the LIVE host before its stack packages, eg. kernel
    /// headers matching the running (livecd) kernel for dkms. default: none.
    fn host_prereqs(&self) -> Vec<Step> {
        Vec::new()
    }

    /// packages to install on the LIVE host. defaults to packages(); stacks whose
    /// build deps track the running (not the target) kernel override this.
    fn host_packages(&self) -> Vec<String> {
        self.packages()
    }

    /// extra apt repositories (key + sources) beyond Debian's, eg. an out-of-tree
    /// dkms module's. `root` is "" for the live host or "/mnt" for the target
    /// chroot, so the same repo is added to both. default: none.
    fn apt_repos(&self, _cfg: &Config, _root: &str) -> Vec<Step> {
        Vec::new()
    }
}

pub fn select(id: &str) -> Result<Box<dyn Stack>> {
    Ok(match id {
        STACK_MD_LVM_EXT4 => Box::new(md_lvm::MdLvm::ext4()),
        STACK_MD_LVM_XFS => Box::new(md_lvm::MdLvm::xfs()),
        STACK_BTRFS => Box::new(btrfs::BtrfsStack),
        STACK_BCACHEFS => Box::new(bcachefs::BcachefsStack),
        STACK_ZFS => Box::new(zfs::ZfsStack),
        STACK_MD_INTEGRITY => Box::new(md_integrity::MdIntegrity),
        other => anyhow::bail!("unknown stack {other:?}"),
    })
}

// the grub.d hook wrapper that resyncs every mirror esp from the live primary on
// update-grub. a thin shim: the sync logic lives in `raiden sync efi`
// (src/sync.rs), invoked here with --yes so the unattended update-grub skips the
// interactive confirmation. the exit code is swallowed (|| true) so a verify or
// mirror failure can never abort grub-mkconfig and block grub.cfg regeneration --
// a broken esp mirror is not fixable from a hook, and wedging every grub upgrade
// would be worse than a stale mirror (run `grub-install` then `raiden sync efi`
// by hand to repair). the primary member's esp is mounted at /boot/efi; the
// others have no persistent mount point and are mounted transiently under
// /run/raiden by `raiden sync efi`.
pub const EFI_MIRROR_WRAPPER: &str =
    "#!/bin/sh\nexec 1>&2\n/usr/local/sbin/raiden sync efi --yes || true\n";

// where the esp mirror grub.d hook lives in the target. grub.d scripts run during
// update-grub and emit grub config on stdout, so the wrapper redirects to stderr.
pub const EFI_MIRROR_HOOK_PATH: &str = "/mnt/etc/grub.d/90_copy_to_efi_mirrors";

// the basename of the esp mirror grub.d hook, shared by doctor (installed path)
// and the install pipeline (target path).
pub const EFI_MIRROR_HOOK_NAME: &str = "90_copy_to_efi_mirrors";

// the inline script installed into the kernel postinst.d/postrm.d hooks. a thin
// shim: the actual sync logic lives in `raiden sync boot` (src/sync.rs), invoked
// here with --yes so the hooks (which run unattended after a kernel package
// upgrade) skip the interactive confirmation. the script propagates the exit
// code, so a mirror failure surfaces and blocks the kernel upgrade -- a
// degraded boot mirror should not silently let an upgrade complete. source
// verification is on by default (no --force); the install finish phase runs
// `raiden sync boot --yes` directly.
//
// unlike the esp hook this is NOT a grub.d script: grub-mkconfig writes
// grub.cfg to a temp file and moves it into place only after the grub.d scripts
// run, so a grub.d hook would mirror a stale grub.cfg. instead this runs from the
// kernel postinst.d/postrm.d hooks (which fire after zz-update-grub). every /boot
// shares one fs uuid (so each disk's grub finds its local copy); the copies have
// no persistent mount point and are mounted transiently under /run/raiden by
// device (the shared uuid cannot address a specific disk). --one-file-system keeps
// rsync from descending into the /boot/efi esp mount.
//
// run-parts invokes the hook with the kernel version + bootdir as positional
// args; they are NOT forwarded to `raiden sync boot` (which takes no positional
// and always mirrors the whole /boot) -- forwarding them would make clap reject
// the call and, since this hook propagates its exit code, block kernel upgrades.
pub const BOOT_MIRROR_HOOK_CONTENT: &str =
    "#!/bin/sh\nexec /usr/local/sbin/raiden sync boot --yes\n";

// must sort AFTER zz-update-grub so grub.cfg is final before we mirror it. a
// "zzz-" prefix beats "zz-update-grub" in every locale (the third letter z > u),
// unlike "zz_..." which a punctuation-ignoring collation could order before it.
pub const BOOT_MIRROR_HOOK_NAME: &str = "zzz-raiden-boot-mirror";

// initramfs hook that bakes the raiden binary and the install manifest into the
// initrd, so `raiden recover` can bring a degraded root online from the rescue
// shell. config-guarded by install.initramfs_recovery (default on). unlike the
// stack tooling (cryptsetup/mdadm/...), which the stock initramfs hooks pull in,
// nothing pulls raiden or the manifest in, so this hook is required for recover.
// it copies the manifest to /etc/raiden inside the initrd, where Manifest::load
// finds it with neither /boot nor the root mounted. runs in the chroot at
// update-initramfs time, so /usr/local/sbin/raiden and /etc/raiden are the target's.
pub const INITRAMFS_HOOK_RAIDEN: &str = r#"#!/bin/sh
PREREQ=""
prereqs() { echo "$PREREQ"; }
case $1 in prereqs) prereqs; exit 0;; esac
. /usr/share/initramfs-tools/hook-functions
copy_exec /usr/local/sbin/raiden /sbin
mkdir -p "${DESTDIR}/etc/raiden"
cp /etc/raiden/manifest.toml "${DESTDIR}/etc/raiden/manifest.toml"
"#;

// where the raiden recovery initramfs hook lives, relative to the system root.
// the single canonical path shared by the install step, doctor's hook check, and
// doctor's fix, so the establish and the verify cannot drift.
pub const RAIDEN_RECOVERY_HOOK: &str = "/etc/initramfs-tools/hooks/raiden";

// initramfs hook that force-loads dm_integrity, needed by dm-crypt aead.
pub const INITRAMFS_HOOK_AEAD: &str = r#"#!/bin/sh
PREREQ=""
prereqs()
{
    echo "$PREREQ"
}

case $1 in
    prereqs)
        prereqs
        exit 0
        ;;
esac

. /usr/share/initramfs-tools/hook-functions

# Begin real processing below this line

force_load dm_integrity
"#;

// initramfs hook for the dm-integrity stack: integrity tooling and udev rules
// must be present in the initrd so the array can be opened at boot.
pub const INITRAMFS_HOOK_INTEGRITY: &str = r#"#!/bin/sh
PREREQ=""
prereqs()
{
    echo "$PREREQ"
}

case $1 in
    prereqs)
        prereqs
        exit 0
        ;;
esac

. /usr/share/initramfs-tools/hook-functions

# Begin real processing below this line

force_load dm_integrity
copy_exec /sbin/integritysetup /sbin
copy_file text /etc/udev/rules.d/99-integrity.rules
"#;

fn pkgs(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// create the root (third) partition on each member disk, aligning its end down
/// to the crypt sector size. without aead, cryptsetup refuses a device whose size
/// is not a multiple of --sector-size, and a partition run to the last usable
/// sector is only 512-aligned. sgdisk aligns the start to 2048 sectors (a
/// multiple of any sector size up to 1MiB), so aligning the end is sufficient.
pub fn create_root_partitions(cfg: &Config, layout: &Layout) -> Vec<Step> {
    let spb = (cfg.crypt.sector_size / LBA_BYTES).max(1);
    layout
        .members
        .iter()
        .map(|d| {
            let dev = format!("/dev/{d}");
            // sgdisk -E is the last usable sector; round (end+1) down to a whole
            // number of sector-size blocks. spb==1 (512) reduces to the disk end.
            Step::sh(
                format!(
                    "create root partition on {d} (end aligned to {} bytes)",
                    cfg.crypt.sector_size
                ),
                format!(
                    "end=$(sgdisk -E {dev}); end=$(( (end + 1) / {spb} * {spb} - 1 )); \
                     sgdisk -n3:0:$end -t3:8301 {dev}"
                ),
            )
        })
        .collect()
}

fn cryptsetup_format_argv(cfg: &Config, dev: &str) -> Vec<String> {
    let c = &cfg.crypt;
    let mut a = vec![
        "cryptsetup".to_string(),
        "-q".to_string(),
        "luksFormat".to_string(),
        format!("--cipher={}", c.cipher),
        format!("--key-size={}", c.key_size),
        format!("--sector-size={}", c.sector_size),
    ];
    if c.integrity == "aead" {
        a.push("--integrity=aead".to_string());
        // skip the full-device integrity wipe (slow on large disks); tags are then
        // uninitialized until written. only valid alongside --integrity.
        if c.integrity_no_wipe {
            a.push("--integrity-no-wipe".to_string());
        }
    }
    a.extend(c.extra_args.iter().cloned());
    a.push(dev.to_string());
    a
}

/// luks-format a single device (eg. the md array in the integrity stack).
pub fn crypt_format_device(cfg: &Config, dev: &str, note: impl Into<String>) -> Step {
    Step::run_owned(note, cryptsetup_format_argv(cfg, dev)).secret()
}

/// unlock a single device under the given mapper name.
pub fn crypt_open_device(dev: &str, name: &str, note: impl Into<String>) -> Step {
    Step::run_owned(
        note,
        vec![
            "cryptsetup".to_string(),
            "luksOpen".to_string(),
            dev.to_string(),
            name.to_string(),
        ],
    )
    .secret()
}

/// luks-format the root partition on every member disk, then restore each disk's
/// original luks uuid (a no-op at install, load-bearing on replace).
pub fn crypt_format_disks(cfg: &Config, layout: &Layout) -> Vec<Step> {
    let mut s = Vec::new();
    for d in &layout.members {
        let dev = layout.part(d, 3);
        s.push(crypt_format_device(cfg, &dev, format!("luks-format {dev}")));
        s.push(crypt_preserve_uuid(&dev, &layout.crypt_name(d)));
    }
    s
}

/// stamp the disk's original luks uuid -- read from the running /etc/crypttab --
/// onto the freshly luks-formatted header. a no-op at install (no entry exists
/// yet); on replace it keeps the installed crypttab's `UUID=` reference valid so
/// the disk unlocks at the next boot. without it, a reboot after replace drops to
/// the initramfs because the replaced members never unlock and the array cannot
/// assemble. luksUUID needs no passphrase. mirrors the esp-uuid preservation.
/// root md array health detail, shared by the md-backed stacks (md~lvm~ext4 and
/// dm-integrity).
pub fn md_status() -> Vec<Step> {
    vec![Step::run("md array detail", &["mdadm", "--detail", ROOT_MD_DEVICE]).best_effort()]
}

/// start and wait for a check scrub of the root md array, shared by the md-backed
/// stacks.
pub fn md_scrub() -> Vec<Step> {
    vec![
        Step::run(
            "start a check scrub",
            &["mdadm", "--action=check", ROOT_MD_DEVICE],
        ),
        Step::run("wait for the scrub", &["mdadm", "--wait", ROOT_MD_DEVICE]),
    ]
}

/// shared md replace: zero each replacement member's stale superblock, re-add it
/// to the root array, then wait for the rebuild. `member` maps a disk to its
/// array-member device (a crypt device, or an integrity device).
pub fn md_replace(disks: &[String], member: impl Fn(&str) -> String) -> Vec<Step> {
    let mut s = Vec::new();
    for d in disks {
        let dev = member(d);
        s.push(
            Step::run_owned(
                format!("clear stale superblock on {dev}"),
                vec![
                    "mdadm".to_string(),
                    "--zero-superblock".to_string(),
                    dev.clone(),
                ],
            )
            .best_effort(),
        );
        s.push(Step::run_owned(
            format!("re-add {dev} to the root array"),
            vec![
                "mdadm".to_string(),
                "--add".to_string(),
                ROOT_MD_DEVICE.to_string(),
                dev,
            ],
        ));
    }
    s.push(Step::run(
        "wait for the rebuild",
        &["mdadm", "--wait", ROOT_MD_DEVICE],
    ));
    s
}

/// shared md remove: fail+remove each disk's member from the root array, clear any
/// vacant slot left by a wholly-lost disk, then tear down each member's lower
/// layer. `member` maps a disk to its array-member device; `teardown` produces the
/// per-disk lock/close step (luksClose for crypt, integritysetup close for
/// integrity).
pub fn md_remove(
    disks: &[String],
    member: impl Fn(&str) -> String,
    teardown: impl Fn(&str) -> Step,
) -> Vec<Step> {
    let mut s = Vec::new();
    for d in disks {
        let dev = member(d);
        s.push(
            Step::run_owned(
                format!("fail {dev} in the root array"),
                vec![
                    "mdadm".to_string(),
                    "--fail".to_string(),
                    ROOT_MD_DEVICE.to_string(),
                    dev.clone(),
                ],
            )
            .best_effort(),
        );
        s.push(
            Step::run_owned(
                format!("remove {dev} from the root array"),
                vec![
                    "mdadm".to_string(),
                    "--remove".to_string(),
                    ROOT_MD_DEVICE.to_string(),
                    dev,
                ],
            )
            .best_effort(),
        );
    }
    // clear any slot left behind by a wholly-lost disk (no device node to target).
    s.extend(md_drop_missing(ROOT_MD_DEVICE));
    for d in disks {
        s.push(teardown(d));
    }
    s
}

/// the recovery actions for the md-backed stacks (md~lvm~ext4/xfs and
/// dm-integrity): the crypt layer is already open by the initramfs, so assemble +
/// run the array (--run kicks a dirty-degraded array that stalled), activate lvm,
/// then mount /dev/vg0/root at `at`. shared so the two md stacks recover identically.
pub fn md_recover_actions(at: &str) -> Vec<RecoverAction> {
    vec![
        RecoverAction::new(
            "assemble and run the root array, activate lvm",
            vec![
                md_assemble(ROOT_MD_NAME).best_effort(),
                Step::run_owned(
                    format!("run {ROOT_MD_DEVICE} (kick a stalled degraded array)"),
                    vec!["mdadm".into(), "--run".into(), ROOT_MD_DEVICE.into()],
                )
                .best_effort(),
                lvm_activate(),
            ],
        ),
        RecoverAction::new(
            format!("mount the root filesystem at {at}"),
            vec![Step::run_owned(
                format!("mount /dev/vg0/root at {at}"),
                vec!["mount".into(), "/dev/vg0/root".into(), at.into()],
            )],
        ),
    ]
}

/// the shared `partition_root` for the per-disk dm-crypt stacks (md~lvm~ext4,
/// btrfs, zfs): create each disk's root partition, luks-format it (restoring its
/// uuid on replace), and open it. the dm-integrity stack differs (integrity below
/// md) and keeps its own.
pub fn crypt_partition_root(cfg: &Config, layout: &Layout) -> Vec<Step> {
    let mut s = create_root_partitions(cfg, layout);
    s.extend(crypt_format_disks(cfg, layout));
    s.extend(crypt_open_disks(layout));
    s
}

fn crypt_preserve_uuid(dev: &str, name: &str) -> Step {
    Step::sh(
        format!("preserve {name} luks uuid from /etc/crypttab (replace)"),
        format!(
            "uuid=$(awk -v n={name} '$1==n {{print $2}}' /etc/crypttab 2>/dev/null | sed 's/^UUID=//'); \
             if [ -n \"$uuid\" ]; then cryptsetup -q luksUUID {dev} --uuid \"$uuid\"; fi"
        ),
    )
}

/// unlock each member's root partition.
pub fn crypt_open_disks(layout: &Layout) -> Vec<Step> {
    layout
        .members
        .iter()
        .map(|d| {
            let dev = layout.part(d, 3);
            let name = layout.crypt_name(d);
            crypt_open_device(&dev, &name, format!("unlock {dev} as {name}"))
        })
        .collect()
}

/// best-effort lock of every member's root mapping.
pub fn crypt_close_disks(layout: &Layout) -> Vec<Step> {
    layout
        .crypt_names()
        .into_iter()
        .map(|name| {
            Step::run_owned(
                format!("lock {name}"),
                vec!["cryptsetup".to_string(), "luksClose".to_string(), name],
            )
            .best_effort()
        })
        .collect()
}

/// create an md array across the given member devices. `yes |` answers mdadm's
/// "may not be suitable as a boot device" prompt so creation is non-interactive.
pub fn md_create(name: &str, level: &str, bitmap: &str, devices: &[String]) -> Step {
    let cmd = format!(
        "yes | mdadm --create --name={name} --level={level} --raid-devices={} --bitmap={bitmap} /dev/md/{name} {}",
        devices.len(),
        devices.join(" ")
    );
    Step::sh(format!("create md array {name} (level {level})"), cmd)
}

/// assemble a previously created md array by name.
pub fn md_assemble(name: &str) -> Step {
    Step::run_owned(
        format!("assemble md array {name}"),
        vec![
            "mdadm".to_string(),
            "--assemble".to_string(),
            format!("--name={name}"),
            format!("/dev/md/{name}"),
        ],
    )
}

/// clear vanished members from an md array. a wholly-lost disk has no device
/// node, so a per-device `mdadm --remove <dev>` is a no-op and the array keeps
/// the vacant slot -- which then blocks repartitioning the disk and re-adding the
/// member ("Device or resource busy"). `failed`/`detached` clear the slot without
/// needing the device node.
pub fn md_drop_missing(device: &str) -> Vec<Step> {
    ["failed", "detached"]
        .into_iter()
        .map(|which| {
            Step::run_owned(
                format!("drop {which} members from {device}"),
                vec![
                    "mdadm".to_string(),
                    "--remove".to_string(),
                    device.to_string(),
                    which.to_string(),
                ],
            )
            .best_effort()
        })
        .collect()
}

/// best-effort stop of an md array.
pub fn md_stop(device: &str) -> Step {
    Step::run_owned(
        format!("stop md array {device}"),
        vec![
            "mdadm".to_string(),
            "--stop".to_string(),
            device.to_string(),
        ],
    )
    .best_effort()
}

/// best-effort: stop whatever md array currently holds any of `devices`, located
/// via /sys/block/<dev>/holders. this catches an array assembled under a
/// non-canonical node (eg. md127 from a hand-create or a prior boot's
/// auto-assembly) that md_stop by the /dev/md/<name> node would miss. devices
/// that are absent or hold no array are skipped, so passing every candidate is
/// safe. run it after the upper layers are down and before the member devices are
/// closed, so the array is free to stop and its members can then be released.
pub fn md_stop_holders(devices: &[String]) -> Step {
    Step::sh(
        "stop any md array holding the member devices",
        format!(
            "for d in {}; do \
               b=$(readlink -f \"$d\" 2>/dev/null); [ -n \"$b\" ] || continue; \
               n=$(basename \"$b\"); \
               for h in /sys/block/\"$n\"/holders/md*; do \
                 [ -e \"$h\" ] && mdadm --stop \"/dev/$(basename \"$h\")\"; \
               done; \
             done; true",
            devices.join(" ")
        ),
    )
    .best_effort()
}

/// create the vg0/root logical volume on the given physical volume.
pub fn lvm_create_root(pv: &str) -> Vec<Step> {
    vec![
        Step::run_owned(
            format!("create physical volume on {pv}"),
            vec!["pvcreate".to_string(), pv.to_string()],
        ),
        Step::run_owned(
            "create volume group vg0",
            vec!["vgcreate".to_string(), "vg0".to_string(), pv.to_string()],
        ),
        Step::run(
            "create logical volume vg0/root",
            &["lvcreate", "--extents=90%FREE", "--name=root", "vg0"],
        ),
    ]
}

pub fn lvm_activate() -> Step {
    Step::run("activate vg0", &["vgchange", "-a", "y", "vg0"])
}

pub fn lvm_deactivate() -> Step {
    Step::run("deactivate vg0", &["vgchange", "-a", "n", "vg0"]).best_effort()
}

/// format vg0/root as ext4. stride/stripe-width are aligned to the real md
/// geometry at run time, which is why no -E options appear here.
pub fn mkfs_ext4_root() -> Step {
    Step::run(
        "format vg0/root as ext4 (aligned to md geometry at run time)",
        &["mkfs.ext4", "-m", "0", "/dev/vg0/root"],
    )
}

/// append the ext4 root line to the target fstab.
pub fn fstab_root_ext4() -> Step {
    Step::append(
        "add the root filesystem to fstab",
        "/mnt/etc/fstab",
        "/dev/vg0/root / ext4 rw,relatime,errors=remount-ro 0 0\n",
    )
}

/// format vg0/root as xfs. mkfs.xfs autodetects the md stripe geometry (sunit/
/// swidth), so no explicit alignment is passed; -f overwrites any stale signature.
pub fn mkfs_xfs_root() -> Step {
    Step::run(
        "format vg0/root as xfs",
        &["mkfs.xfs", "-f", "/dev/vg0/root"],
    )
}

/// append the xfs root line to the target fstab. xfs recovers via its own log
/// (no fsck pass) and does not take ext4's errors=remount-ro option.
pub fn fstab_root_xfs() -> Step {
    Step::append(
        "add the root filesystem to fstab",
        "/mnt/etc/fstab",
        "/dev/vg0/root / xfs defaults 0 0\n",
    )
}

/// append the btrfs root line to the target fstab, keyed by the filesystem uuid
/// and mounted at /, preserving the live mount options. the live mount is at
/// /mnt during install, so the device/mountpoint must be rewritten: a uuid and /
/// (not the captured /dev/mapper path and /mnt) are what let the installed system
/// remount rw at boot via systemd-remount-fs (R8). the options are kept verbatim
/// so any btrfs-specific tuning (subvol, csum, discard) the kernel chose survives.
pub fn fstab_root_btrfs(layout: &Layout) -> Step {
    let dev = layout
        .crypt_devices()
        .into_iter()
        .next()
        .unwrap_or_default();
    Step::sh(
        "add the btrfs root to fstab",
        format!(
            "uuid=$(blkid -s UUID -o value {dev}); \
             opts=$(awk '$2==\"/mnt\" && $3==\"btrfs\" {{print $4}}' /proc/self/mounts); \
             echo \"UUID=$uuid / btrfs ${{opts:-defaults}} 0 0\" >> /mnt/etc/fstab"
        ),
    )
}

/// append the bcachefs root line to the target fstab, keyed by the filesystem
/// uuid (blkid reports it on any member); the mount helper resolves the uuid to
/// the multi-device set at boot.
pub fn fstab_root_bcachefs(layout: &Layout) -> Step {
    let dev = layout
        .crypt_devices()
        .into_iter()
        .next()
        .unwrap_or_default();
    Step::sh(
        "add the bcachefs root to fstab",
        format!(
            "uuid=$(blkid -s UUID -o value {dev}); echo \"UUID=$uuid / bcachefs defaults 0 0\" >> /mnt/etc/fstab"
        ),
    )
}

/// load a kernel module on the live host (eg. an out-of-tree dkms module).
pub fn modprobe(module: &str) -> Step {
    Step::run_owned(
        format!("load the {module} kernel module"),
        vec!["modprobe".to_string(), module.to_string()],
    )
}

/// write the crypttab, resolving each member's luks uuid via blkid, to `target`
/// (eg. "/mnt/etc/crypttab" at install, "/etc/crypttab" for a running-system
/// regen after a `replace --with` swap).
pub fn crypttab_step(layout: &Layout, opts: &str, target: &str) -> Step {
    let mut script = String::from("{\n");
    for disk in &layout.members {
        let dev = layout.part(disk, 3);
        let name = layout.crypt_name(disk);
        script.push_str(&format!(
            "uuid=$(blkid -s UUID -o value {dev}); echo \"{name} UUID=$uuid none {opts}\"\n"
        ));
    }
    script.push_str(&format!("}} > {target}\n"));
    Step::sh(format!("write {target} ({opts})"), script)
}

/// back up each member's luks header onto /boot for disaster recovery.
/// the initrd binaries common to every dm-crypt stack: cryptsetup, and the
/// decrypt_keyctl keyscript with its keyctl (the type-once-cache-for-all-members
/// unlock). per-stack assemble/mount tools are added by `Stack::initramfs_binaries`.
pub fn crypt_initramfs_binaries() -> Vec<&'static str> {
    vec!["cryptsetup", "keyctl", "decrypt_keyctl"]
}

/// back up each member's luks header under `<boot>/luks` ("/mnt/boot" at install,
/// "/boot" for doctor's re-backup fix). cryptsetup refuses to overwrite, so callers
/// that may re-run (doctor) clear the dir first.
pub fn backup_luks_headers(layout: &Layout, boot: &str) -> Vec<Step> {
    let dir = format!("{boot}/luks");
    let mut s = vec![Step::run_owned(
        "create luks header backup directory on /boot".to_string(),
        vec!["mkdir".to_string(), "-p".to_string(), dir.clone()],
    )];
    for d in &layout.members {
        let dev = layout.part(d, 3);
        s.push(Step::run_owned(
            format!("back up luks header for {dev}"),
            vec![
                "cryptsetup".to_string(),
                "luksHeaderBackup".to_string(),
                dev,
                "--header-backup-file".to_string(),
                format!("{dir}/{d}3-headers.bin"),
            ],
        ));
    }
    s
}

/// install keyutils in the target so decrypt_keyctl can cache the passphrase.
pub fn install_keyutils() -> Step {
    Step::run(
        "install keyutils in target",
        &["apt-get", "install", "-y", "keyutils"],
    )
    .chroot()
}

/// rebuild the target initrd for all kernels.
pub fn update_initramfs() -> Step {
    Step::run(
        "rebuild the initramfs",
        &["update-initramfs", "-c", "-k", "all"],
    )
    .chroot()
}

/// rebuild the initrd for all kernels in UPDATE mode (vs `update_initramfs`'s
/// create). `chroot` for the install target, direct for the running system. shared
/// by install's recovery bake and doctor's initrd/recover-bundle fixes.
pub fn update_initramfs_u(chroot: bool) -> Step {
    let s = Step::run(
        "rebuild the initramfs",
        &["update-initramfs", "-u", "-k", "all"],
    );
    if chroot {
        s.chroot()
    } else {
        s
    }
}

/// install the raiden recovery initramfs hook under `root` ("/mnt" at install, ""
/// on the running system). the hook bakes raiden + the manifest into the initrd so
/// `raiden recover` is available at the rescue shell. the one establish form, shared
/// by the install pipeline and doctor's recover-bundle fix.
pub fn raiden_recovery_hook_step(root: &str) -> Step {
    Step::write_mode(
        "install the raiden recovery initramfs hook",
        format!("{root}{RAIDEN_RECOVERY_HOOK}"),
        INITRAMFS_HOOK_RAIDEN,
        0o755,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // the kernel postinst.d/postrm.d hooks are run by run-parts with the kernel
    // version and bootdir as positional args ($1, $2). `raiden sync boot` takes no
    // positional, so a hook that forwards "$@" makes clap reject the call (exit 2);
    // because the boot hook propagates its exit code, that fails the hook and
    // blocks every kernel/initramfs upgrade. the hook must invoke sync without
    // forwarding those args (sync always mirrors the whole /boot regardless).
    #[test]
    fn boot_mirror_hook_does_not_forward_runparts_args() {
        assert!(BOOT_MIRROR_HOOK_CONTENT.contains("raiden sync boot --yes"));
        assert!(
            !BOOT_MIRROR_HOOK_CONTENT.contains("$@"),
            "boot hook must not forward run-parts positional args to `raiden sync boot`"
        );
    }
}
