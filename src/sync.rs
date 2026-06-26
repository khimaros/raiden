// resync the independent /boot and esp mirrors from the live primary. the two
// syncs are split into `raiden sync boot` and `raiden sync efi` because they fire
// at different moments: /boot must run after `zz-update-grub` (it needs the final
// grub.cfg and the crypttab-aware initrd, so it lives in kernel postinst.d/
// postrm.d), while the esp resync runs during update-grub (a grub.d hook) to
// catch grub-install's esp changes on a grub package upgrade. a single command
// cannot serve both without reintroducing the stale-grub.cfg bug.
//
// the rsync loop (transient mount under /run/raiden, rsync, unmount) is shared.
// the source is verified before any mirror is touched: on a verify failure the
// command prints the diagnostics and exits non-zero without syncing (a broken
// source must never be propagated). verify is on by default for every caller;
// --force disables it (used by no script). per-mirror sync is best-effort-continue:
// a failed mirror is reported and counted, and the exit code is non-zero iff any
// mirror failed. the hook wrappers decide whether to propagate that exit code.
//
// when boot.raid is set, /boot is md raid1 and mdadm handles replication, so
// `sync boot` is a no-op. every /boot shares one fs uuid (so each disk's grub
// finds its local copy); the mirrors have no persistent mount point and are
// mounted transiently by device. --one-file-system keeps the /boot rsync from
// descending into the /boot/efi esp mount.

use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::cli::SyncTarget;
use crate::efi;
use crate::layout::{Layout, ESP_MOUNT};
use crate::prompt;

/// verify the source before syncing. returns Ok, or Err with a list of every
/// failure so the caller can report the whole picture at once. shared with
/// doctor's pre-mirror inspection.
pub fn verify_boot(src_dev: &str, src_mount: &str) -> std::result::Result<(), Vec<String>> {
    let mut fails = check_fsck(src_dev);
    if let Err(more) = verify_boot_files(src_mount) {
        fails.extend(more);
    }
    if fails.is_empty() {
        Ok(())
    } else {
        Err(fails)
    }
}

/// the bootability of a /boot tree without the on-device fsck: a grub.cfg, a
/// kernel, and an initrd that contains cryptsetup. used per-member (doctor's
/// bootloader check transient-mounts each mirror read-only, where running fsck is
/// undesirable) and as the no-fsck half of verify_boot.
pub fn verify_boot_files(src_mount: &str) -> std::result::Result<(), Vec<String>> {
    let mut fails = check_boot_files(src_mount);
    fails.extend(check_initrd(src_mount));
    if fails.is_empty() {
        Ok(())
    } else {
        Err(fails)
    }
}

/// verify the esp source (shim + grub present). shared with doctor.
pub fn verify_efi(src_mount: &str) -> std::result::Result<(), Vec<String>> {
    let fails = check_efi_files(src_mount);
    if fails.is_empty() {
        Ok(())
    } else {
        Err(fails)
    }
}

/// the live mount point and device list for one kind of mirror.
struct MirrorSet {
    /// the mount point being mirrored (the source), eg. "/boot" or "/boot/efi".
    mount: &'static str,
    /// the block device backing the mounted source, or None when not mounted.
    src: Option<String>,
    /// the mirror devices (every member except the source).
    mirrors: Vec<String>,
    /// the transient-mount tmp dir prefix under /run/raiden.
    tmp_prefix: &'static str,
    /// rsync `--one-file-system`? the /boot sync must not recurse into /boot/efi.
    one_file_system: bool,
}

pub fn run(
    target: &SyncTarget,
    layout: &Layout,
    yes: bool,
    force: bool,
    verbose: bool,
    dry_run: bool,
) -> Result<()> {
    let set = resolve(target, layout)?;
    let Some(src) = set.src.clone() else {
        // not mounted. the kernel/grub hooks always run with it mounted, so this
        // only happens on a bare system; no-op (do not fail a hook) rather than
        // error, matching the old scripts' `mountpoint -q ... || exit 0`.
        println!("{} is not mounted; nothing to sync", set.mount);
        return Ok(());
    };
    if set.mirrors.is_empty() {
        println!(
            "no {} mirrors to sync (single member or all already the source)",
            set.mount
        );
        return Ok(());
    }

    // always announce the resolved source and mirrors first, before any verify or
    // prompt, so even a verify failure (which bails without prompting) shows what
    // we were operating on.
    println!("source:  {src}");
    println!("mirrors: {}", set.mirrors.join(", "));

    if dry_run {
        let ofs = if set.one_file_system {
            "--one-file-system "
        } else {
            ""
        };
        for dev in &set.mirrors {
            println!(
                "  rsync {ofs}--times --recursive --delete {src_mount}/ <mount of {dev}>/",
                src_mount = set.mount
            );
        }
        return Ok(());
    }

    // verify the source before propagating it. --force skips verify (no script
    // passes it). a verify failure prints every check and bails without syncing.
    if !force {
        let result = match target {
            SyncTarget::Boot(..) => verify_boot(&src, set.mount),
            SyncTarget::Efi(..) => verify_efi(set.mount),
        };
        if let Err(fails) = result {
            eprintln!("{mount} pre-sync verification failed:", mount = set.mount);
            for f in &fails {
                eprintln!("  - {f}");
            }
            bail!("refusing to mirror a broken {mount}", mount = set.mount);
        }
    }

    // interactive confirmation unless --yes (hooks, install, -y).
    if !prompt::confirm_or_yes(yes, "sync?")? {
        bail!("aborted");
    }

    let mut failed = 0;
    for dev in &set.mirrors {
        if let Err(e) = mirror_one(dev, set.mount, set.tmp_prefix, set.one_file_system, verbose) {
            eprintln!("{mount}: mirror {dev} failed: {e}", mount = set.mount);
            failed += 1;
        }
    }
    if failed != 0 {
        bail!("{failed} {mount} mirror(s) failed", mount = set.mount);
    }
    Ok(())
}

/// resolve the live source and the mirror set for one kind of sync.
fn resolve(target: &SyncTarget, layout: &Layout) -> Result<MirrorSet> {
    match target {
        SyncTarget::Boot(..) => {
            if layout.boot_raid() {
                println!("boot is on md raid1 -- mirroring handled by mdadm, nothing to do");
                // return an empty set so run() exits cleanly via the mirrors check
                return Ok(MirrorSet {
                    mount: "/boot",
                    src: None,
                    mirrors: Vec::new(),
                    tmp_prefix: "boot",
                    one_file_system: true,
                });
            }
            let (src, mirrors) = boot_mirror_set(layout);
            Ok(MirrorSet {
                mount: "/boot",
                src,
                mirrors,
                tmp_prefix: "boot",
                one_file_system: true,
            })
        }
        SyncTarget::Efi(..) => {
            // efi mode only; in bios mode there is no esp to mirror.
            let (src, mirrors) = efi_mirror_set(layout);
            Ok(MirrorSet {
                mount: "/boot/efi",
                src,
                mirrors,
                tmp_prefix: "esp",
                one_file_system: false,
            })
        }
    }
}

/// the live source device and the mirror devices for the independent /boot set.
/// the source is whatever /boot is currently mounted from (by the shared uuid);
/// the mirrors are every other boot device. shared with doctor's drift check.
pub fn boot_mirror_set(layout: &Layout) -> (Option<String>, Vec<String>) {
    let src = findmnt_source("/boot").unwrap_or(None);
    let mirrors = mirrors(layout.boot_devices(), src.as_deref());
    (src, mirrors)
}

/// the live source device and the mirror devices for the esp set. the source is
/// /boot/efi (the primary esp, by its unique uuid); the mirrors are every other
/// esp. shared with doctor's drift check.
pub fn efi_mirror_set(layout: &Layout) -> (Option<String>, Vec<String>) {
    let src = findmnt_source(ESP_MOUNT).unwrap_or(None);
    let mirrors = mirrors(layout.esp_devices(), src.as_deref());
    (src, mirrors)
}

/// the member devices minus the live source, or all of them when the source is
/// not currently mounted (then nothing is skipped).
fn mirrors(devices: Vec<String>, src: Option<&str>) -> Vec<String> {
    match src {
        Some(s) => devices
            .into_iter()
            .filter(|d| !is_same_device(d, s))
            .collect(),
        None => Vec::new(),
    }
}

/// the block device backing the mounted `path`, or None when it is not mounted.
pub fn findmnt_source(path: &str) -> Result<Option<String>> {
    let out = Command::new("findmnt")
        .args(["-no", "SOURCE", path])
        .output()
        .with_context(|| format!("running findmnt on {path}"))?;
    if !out.status.success() {
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        Ok(None)
    } else {
        Ok(Some(s))
    }
}

/// whether two device paths refer to the same block device. resolves symlinks
/// (eg. /dev/disk/by-* -> /dev/sda1) via canonicalize, falling back to a plain
/// string compare when the node cannot be stat'd.
pub fn is_same_device(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(pa), Ok(pb)) => pa == pb,
        _ => a == b,
    }
}

/// read-only ext4 check. e2fsck -n on a clean, mounted filesystem exits 0 (root
/// can open it read-only); a non-zero exit leaves errors uncorrected.
fn check_fsck(boot_dev: &str) -> Vec<String> {
    let out = Command::new("e2fsck").args(["-n", boot_dev]).output();
    match out {
        Ok(o) if o.status.success() => Vec::new(),
        Ok(o) => vec![format!(
            "e2fsck reported errors on {boot_dev}: {}",
            String::from_utf8_lossy(&o.stdout).trim()
        )],
        Err(e) => vec![format!("could not run e2fsck on {boot_dev}: {e}")],
    }
}

/// presence and sanity of grub.cfg, a kernel, and an initrd on /boot.
fn check_boot_files(boot_path: &str) -> Vec<String> {
    let mut fails = Vec::new();
    let grub_cfg = format!("{boot_path}/grub/grub.cfg");
    match std::fs::read_to_string(&grub_cfg) {
        Ok(text) => {
            if text.trim().is_empty() {
                fails.push("grub.cfg is empty".into());
            }
            if !text.contains("menuentry") {
                fails.push("grub.cfg has no menuentry".into());
            }
            if !text.contains("fs-uuid") {
                fails.push(
                    "grub.cfg has no 'search --fs-uuid' (cannot find the root by uuid)".into(),
                );
            }
        }
        Err(_) => fails.push(format!("grub.cfg missing at {grub_cfg}")),
    }
    if !has_glob(boot_path, "vmlinuz-") {
        fails.push("no kernel (vmlinuz-*) found on /boot".into());
    }
    if !has_glob(boot_path, "initrd.img-") {
        fails.push("no initrd (initrd.img-*) found on /boot".into());
    }
    fails
}

/// the initrd must contain cryptsetup so an encrypted root can be unlocked from
/// the initramfs. uses lsinitramfs (debian's initramfs-tools listing tool).
fn check_initrd(boot_path: &str) -> Vec<String> {
    let Some(initrd) = first_glob(boot_path, "initrd.img-") else {
        return Vec::new(); // missing initrd is reported by check_boot_files
    };
    initrd_has_cryptsetup(&initrd).err().into_iter().collect()
}

/// the lsinitramfs file listing of an initrd, or Err with the reason. shared by the
/// cryptsetup probe and doctor's required-binaries check (one lsinitramfs, not many).
pub fn initrd_listing(initrd: &str) -> std::result::Result<String, String> {
    match Command::new("lsinitramfs").arg(initrd).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).to_string()),
        Ok(_) => Err(format!("lsinitramfs failed on {initrd}")),
        Err(_) => Err(format!("lsinitramfs unavailable; cannot verify {initrd}")),
    }
}

/// whether an initrd contains cryptsetup, so an encrypted root can unlock from the
/// initramfs. Ok, or Err with the reason. used by sync's source verification.
pub fn initrd_has_cryptsetup(initrd: &str) -> std::result::Result<(), String> {
    if initrd_listing(initrd)?.contains("cryptsetup") {
        Ok(())
    } else {
        Err(format!(
            "initrd {initrd} does not contain cryptsetup (encrypted root would not unlock)"
        ))
    }
}

/// presence of the bootloader files grub-install writes to an esp: the shim and
/// grub's own efi binary. a missing shim means the firmware cannot boot this esp.
fn check_efi_files(esp_path: &str) -> Vec<String> {
    let mut fails = Vec::new();
    for (rel, label) in [
        (efi::SHIM_FILE, "shimx64.efi"),
        (efi::GRUB_FILE, "grubx64.efi"),
    ] {
        let p = format!("{esp_path}/{rel}");
        if !std::path::Path::new(&p).exists() {
            fails.push(format!(
                "{label} missing at {p} (grub-install has not run on this esp?)"
            ));
        }
    }
    fails
}

/// whether `dir` contains at least one entry whose name starts with `prefix`.
fn has_glob(dir: &str, prefix: &str) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with(prefix))
        })
        .unwrap_or(false)
}

fn first_glob(dir: &str, prefix: &str) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path().to_string_lossy().to_string())
        .next()
}

/// create a transient mount point under /run/raiden and mount `dev` there
/// (read-only). returns the mount path. paired with `unmount_transient`.
/// shared by sync's mirror loop and doctor's drift comparison.
pub fn mount_transient(dev: &str, prefix: &str, read_only: bool) -> Result<String> {
    let tmp = mktemp_dir(prefix)?;
    let mut cmd = Command::new("mount");
    if read_only {
        cmd.arg("-o").arg("ro");
    }
    let status = cmd
        .arg(dev)
        .arg(&tmp)
        .status()
        .with_context(|| format!("mounting {dev} at {tmp}"))?;
    if !status.success() {
        let _ = std::fs::remove_dir(&tmp);
        bail!("mount {dev} at {tmp} exited {status}");
    }
    Ok(tmp)
}

/// unmount and remove a transient mount point created by `mount_transient`.
/// best-effort: never fails the caller (the sync result is what matters).
pub fn unmount_transient(tmp: &str) {
    let _ = run_status(&["umount", tmp]);
    let _ = std::fs::remove_dir(tmp);
}

/// compare a live source against a mirror without writing: mount the mirror
/// read-only and run rsync in dry-run --itemize-changes mode. returns the list of
/// changed paths (empty when in sync). shared with doctor's drift check.
pub fn drift(src_mount: &str, mirror_dev: &str, one_fs: bool) -> Result<Vec<String>> {
    let tmp = mount_transient(mirror_dev, "drift", true)?;
    let res = (|| -> Result<Vec<String>> {
        let mut cmd = Command::new("rsync");
        cmd.args(["--dry-run", "--itemize-changes", "--out-format=%n"])
            .arg("--recursive")
            .arg("--delete");
        if one_fs {
            cmd.arg("--one-file-system");
        }
        cmd.args([format!("{src_mount}/"), format!("{tmp}/")]);
        let out = cmd.output().context("running rsync --dry-run")?;
        // rsync exits non-zero on real errors; --dry-run --itemize-changes prints
        // one path per differing file to stdout. an empty stdout means in sync.
        if !out.status.success() && !out.stdout.is_empty() {
            bail!(
                "rsync exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let changed: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        Ok(changed)
    })();
    unmount_transient(&tmp);
    res
}

/// mount a mirror device transiently, rsync the live source onto it, unmount.
/// the temp dir is removed even on failure. returns Ok iff the rsync succeeded.
fn mirror_one(
    dev: &str,
    src_mount: &str,
    tmp_prefix: &str,
    one_fs: bool,
    verbose: bool,
) -> Result<()> {
    let tmp = mount_transient(dev, tmp_prefix, false)?;
    let rsync = run_rsync(src_mount, &tmp, one_fs, verbose);
    unmount_transient(&tmp);
    rsync.context("rsync failed")
}

fn mktemp_dir(prefix: &str) -> Result<String> {
    let _ = std::fs::create_dir_all("/run/raiden");
    let template = format!("/run/raiden/{prefix}.XXXXXX");
    let out = Command::new("mktemp")
        .args(["-d", &template])
        .output()
        .context("running mktemp")?;
    if !out.status.success() {
        bail!(
            "mktemp failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// rsync the live source onto the mirror mount. stdout/stderr inherit the console
/// so --verbose itemize output is visible.
fn run_rsync(src_mount: &str, tmp: &str, one_fs: bool, verbose: bool) -> Result<()> {
    let mut cmd = Command::new("rsync");
    cmd.args(["--times", "--recursive", "--delete"]);
    if one_fs {
        cmd.arg("--one-file-system");
    }
    if verbose {
        cmd.arg("--itemize-changes");
    }
    cmd.args([format!("{src_mount}/"), format!("{tmp}/")]);
    let status = cmd.status().context("running rsync")?;
    if status.success() {
        Ok(())
    } else {
        bail!("rsync exited {}", status)
    }
}

/// run a command inheriting stdio; ok iff it succeeds.
fn run_status(argv: &[&str]) -> Result<()> {
    let status = Command::new(argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("running {}", argv.join(" ")))?;
    if status.success() {
        Ok(())
    } else {
        bail!("{} exited {}", argv.join(" "), status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_boot_files_passes_a_complete_boot() {
        let dir = std::env::temp_dir().join(format!("raiden-boot-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("grub")).unwrap();
        std::fs::write(
            dir.join("grub/grub.cfg"),
            "menuentry 'debian' {\n search --no-floppy --fs-uuid --set=root abc\n linux /vmlinuz-x\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("vmlinuz-6.1"), "kernel").unwrap();
        std::fs::write(dir.join("initrd.img-6.1"), "initrd").unwrap();
        assert!(check_boot_files(dir.to_str().unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_boot_files_flags_missing_and_empty_grub_cfg() {
        let dir = std::env::temp_dir().join(format!("raiden-boot-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("grub")).unwrap();
        std::fs::write(dir.join("grub/grub.cfg"), "  \n").unwrap(); // empty
                                                                    // no vmlinuz-*, no initrd.img-*
        let fails = check_boot_files(dir.to_str().unwrap());
        assert!(fails.iter().any(|f| f.contains("empty")));
        assert!(fails.iter().any(|f| f.contains("menuentry")));
        assert!(fails.iter().any(|f| f.contains("fs-uuid")));
        assert!(fails.iter().any(|f| f.contains("kernel")));
        assert!(fails.iter().any(|f| f.contains("initrd")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_efi_files_passes_when_shim_and_grub_present() {
        let dir = std::env::temp_dir().join(format!("raiden-efi-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("EFI/debian")).unwrap();
        std::fs::write(dir.join("EFI/debian/shimx64.efi"), "shim").unwrap();
        std::fs::write(dir.join("EFI/debian/grubx64.efi"), "grub").unwrap();
        assert!(check_efi_files(dir.to_str().unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_efi_files_flags_missing_bootloader_files() {
        let dir = std::env::temp_dir().join(format!("raiden-efi-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("EFI/debian")).unwrap();
        // only shim, no grubx64
        std::fs::write(dir.join("EFI/debian/shimx64.efi"), "shim").unwrap();
        let fails = check_efi_files(dir.to_str().unwrap());
        assert!(fails.iter().any(|f| f.contains("grubx64")));
        assert!(!fails.iter().any(|f| f.contains("shimx64")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
