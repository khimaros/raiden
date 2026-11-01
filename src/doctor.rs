// `raiden doctor`: a read-only health check that walks the installed system and
// reports the state of each layer the manifest says should be present. it is a
// bolt-on (hand-written checks, not declarative) so each check can carry the
// exact reasoning the others cannot. resolves config from the manifest like the
// other post-install ops; every check is best-effort, so a dead disk fails its
// own check and the rest still run. exit 0 iff every check passes, non-zero if
// any fail.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};

use crate::cli::{BootSyncArgs, EfiSyncArgs, SyncTarget};
use crate::config::{Config, Family};
use crate::disk_id;
use crate::efi;
use crate::layout::{Layout, BOOT_MD_DEVICE, ESP_MOUNT};
use crate::manifest::Manifest;
use crate::prompt;
use crate::stack;
use crate::step::Step;
use crate::sync;

const OK: &str = "ok";
const WARN: &str = "warn";
const FAIL: &str = "fail";
const FIXED: &str = "fixed";

const CRYPTTAB_PATH: &str = "/etc/crypttab";
const SYS_BLOCK: &str = "/sys/class/block";
const LUKS_DM_UUID_PREFIX: &str = "CRYPT-LUKS";
const INTEGRITY_SUFFIX: &str = "_dif";

/// which mirror set a uuid re-stamp targets.
#[derive(Clone, Copy)]
enum UuidKind {
    Boot,
    Esp,
}

/// a check whose failure can be auto-repaired by `doctor --fix`.
#[derive(Clone)]
enum Fix {
    /// write the boot-mirror kernel hook into the named kernel hook dir.
    BootHook(&'static str),
    /// write the esp-mirror grub.d hook.
    EfiHook,
    /// re-sync the independent /boot mirrors.
    SyncBoot,
    /// re-sync the esp mirrors.
    SyncEfi,
    /// re-stamp the shared fs uuid onto divergent mirror partitions, then re-sync
    /// their content. re-observes live state at apply time (see restamp_uuid).
    RestampUuid(UuidKind),
    /// reconcile the per-disk efibootmgr entries: prune stale/duplicate raiden
    /// entries and register one clean shim entry per member (see efibootmgr_fix).
    EfiBootEntries,
    /// re-set grub-efi's debconf to the values raiden owns (see debconf_fix).
    Debconf,
    /// append the missing /boot or /boot/efi fstab entry.
    Fstab,
    /// regenerate /etc/crypttab from the layout (the stack's crypttab_regen), rename
    /// the open mappings to match, and rebuild + sync + verify the initrds.
    Crypttab,
    /// record the members' discovered stable ids in the manifest, then rebuild +
    /// sync + verify the initrds (they bake the manifest for recovery).
    MemberIds,
    /// re-run grub-install on the primary esp (named + removable).
    GrubInstall,
    /// rebuild the initramfs so it carries cryptsetup (update-initramfs -u).
    Initrd,
    /// install the raiden recovery initramfs hook, then rebuild so the initrd
    /// carries raiden + the manifest (the stock hooks do not pull them in).
    RecoverBundle,
    /// re-back-up the luks headers to /boot.
    LuksBackup,
}

impl Fix {
    /// a short imperative phrase for the per-fix confirmation prompt.
    fn action(&self) -> &'static str {
        match self {
            Fix::BootHook(_) => "install the boot-mirror kernel hook",
            Fix::EfiHook => "install the esp-mirror grub.d hook",
            Fix::SyncBoot => "re-sync the /boot mirrors",
            Fix::SyncEfi => "re-sync the esp mirrors",
            Fix::RestampUuid(UuidKind::Boot) => "re-stamp divergent /boot mirror uuids (tune2fs)",
            Fix::RestampUuid(UuidKind::Esp) => {
                "re-stamp divergent esp mirror uuids (reformats them)"
            }
            Fix::EfiBootEntries => {
                "prune stale efibootmgr entries and register per-disk shim entries"
            }
            Fix::Debconf => "re-set grub-efi debconf (removable fallback on, grub nvram off)",
            Fix::Fstab => "append the missing fstab entry",
            Fix::Crypttab => "regenerate /etc/crypttab and rebuild the initramfs",
            Fix::MemberIds => "record the members' stable ids and rebuild the initramfs",
            Fix::GrubInstall => "re-run grub-install on the primary esp",
            Fix::Initrd => "rebuild the initramfs (update-initramfs -u)",
            Fix::RecoverBundle => "install the raiden recovery hook and rebuild the initramfs",
            Fix::LuksBackup => "re-back-up the luks headers to /boot",
        }
    }
}

/// the result of one check: a status (ok/warn/fail/fixed) and a detail line.
struct Check {
    name: &'static str,
    status: &'static str,
    detail: String,
    fix: Option<Fix>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: OK,
            detail: detail.into(),
            fix: None,
        }
    }
    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: WARN,
            detail: detail.into(),
            fix: None,
        }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: FAIL,
            detail: detail.into(),
            fix: None,
        }
    }
    fn with_fix(mut self, fix: Fix) -> Self {
        self.fix = Some(fix);
        self
    }
    #[cfg(test)]
    fn passed(&self) -> bool {
        self.status == OK || self.status == FIXED
    }
}

pub fn run(cfg: &Config, verbose: bool, fix: bool, yes: bool, dry_run: bool) -> Result<()> {
    let family = cfg.family().unwrap_or(Family::Md);
    let efi = cfg.install.boot_mode == "efi";
    // the stack drives the initrd-binary check and the fixes; cfg.validate() ran in
    // cmd_doctor, so this select cannot fail on an invalid id.
    let stack = stack::select(&cfg.raid.stack)?;
    // a manifest without stable ids (an older install) addresses members by kernel
    // name, which may now be a different disk. derive the ids from crypttab's luks
    // uuids and judge every check against that, so no fix acts on a renamed disk.
    let found = discover_ids(cfg, stack.as_ref());
    let mut cfg = cfg.clone();
    cfg.disks.ids.extend(found.clone());
    let cfg = &cfg;
    let layout = &Layout::derive(cfg);
    let mut checks = Vec::new();

    checks.push(check_member_ids(cfg, layout, &found));
    checks.extend(check_disk_presence(layout));
    checks.extend(check_boot_mount(layout));
    checks.extend(check_mount_consistency(layout, efi));
    if efi {
        checks.extend(check_esp_mount());
    }
    checks.extend(check_fstab(efi));
    checks.extend(check_uuid_sharing(layout, efi));
    checks.extend(check_crypttab(layout));
    checks.extend(check_luks_headers(layout));
    checks.extend(check_luks_backup(layout));
    checks.extend(check_array_status(layout, family));
    checks.extend(check_boot_mirrors(layout));
    if !layout.boot_raid() {
        checks.extend(check_boot_drift(layout));
        checks.extend(check_boot_bootloader(layout));
    }
    if efi {
        checks.extend(check_esp_mirrors(layout));
        checks.extend(check_esp_drift(layout));
        checks.extend(check_esp_bootloader(layout));
    }
    checks.extend(check_grub(layout, efi));
    checks.extend(check_initrd(
        stack.as_ref(),
        layout,
        cfg.install.initramfs_recovery,
    ));
    checks.extend(check_kernel_hooks());
    if efi {
        checks.extend(check_efi_hook());
        checks.extend(check_efibootmgr(layout));
        checks.extend(check_debconf());
    }
    checks.extend(check_manifest());

    // --fix --dry-run shows the fix flow (the exact commands, in order), not the
    // checks table -- a look-before-you-leap for the destructive re-stamp.
    if fix && dry_run {
        preview_fixes(&checks, layout, yes);
        return Ok(());
    }
    if fix {
        apply_fixes(&mut checks, cfg, layout, verbose, yes, stack.as_ref())?;
    }

    let any_fail = checks.iter().any(|c| c.status == FAIL);
    let any_warn = checks.iter().any(|c| c.status == WARN);
    print_table(&checks, verbose);
    if any_fail {
        eprintln!("\ndoctor: one or more checks failed");
    } else if any_warn {
        eprintln!("\ndoctor: all checks passed with warnings");
    } else {
        eprintln!("\ndoctor: all checks passed");
    }
    if any_fail {
        std::process::exit(1);
    }
    Ok(())
}

/// `doctor --fix --dry-run`: print the fix flow -- each fixable check and the exact
/// commands it would run, in order -- and nothing else (no checks table, no
/// mutation). honors --yes only in the wording: without it each step would prompt,
/// with it each would run unattended.
fn preview_fixes(checks: &[Check], layout: &Layout, yes: bool) {
    let fixable: Vec<&Check> = checks.iter().filter(|c| c.fix.is_some()).collect();
    if fixable.is_empty() {
        eprintln!("doctor --fix: nothing to repair");
        return;
    }
    eprintln!(
        "doctor --fix --dry-run: the commands below would run, in order; nothing has\n\
         been changed. {}\n",
        if yes {
            "--yes: each runs without confirmation."
        } else {
            "without --yes each step prompts first; decline to skip it."
        }
    );
    for (i, c) in fixable.iter().enumerate() {
        let fix = c.fix.as_ref().unwrap();
        println!("[{}] {}: {}", i + 1, c.name, fix.action());
        println!("    # {}", first_line(&c.detail));
        for cmd in fix_commands(fix, layout) {
            println!("    {cmd}");
        }
        println!();
    }
    eprintln!("re-run without --dry-run to apply.");
}

/// the exact commands a fix would run, in order, for the dry-run preview.
fn fix_commands(fix: &Fix, layout: &Layout) -> Vec<String> {
    match fix {
        Fix::BootHook(dir) => vec![format!("write {} (mode 0755)", hook_path(dir))],
        Fix::EfiHook => vec![format!("write {} (mode 0755)", efi_hook_path())],
        Fix::SyncBoot => vec!["raiden sync boot --yes".into()],
        Fix::SyncEfi => vec!["raiden sync efi --yes".into()],
        Fix::RestampUuid(kind) => restamp_commands(*kind, layout),
        Fix::EfiBootEntries => efibootmgr_commands(layout),
        Fix::Debconf => debconf_commands(),
        Fix::Fstab => vec!["append the missing /boot or /boot/efi UUID= entry to /etc/fstab".into()],
        Fix::Crypttab => {
            let mut c = vec![format!(
                "write {CRYPTTAB_PATH}: one entry per member, by its luks uuid"
            )];
            c.extend(
                open_renames(layout)
                    .iter()
                    .map(|(old, new)| format!("dmsetup rename {old} {new}")),
            );
            c.extend(refresh_commands(layout));
            c
        }
        Fix::MemberIds => {
            let mut c = vec![format!(
                "write [disks.ids] to {} and {}",
                crate::manifest::BOOT_MIRROR_PATH,
                crate::manifest::DEFAULT_PATH
            )];
            c.extend(refresh_commands(layout));
            c
        }
        Fix::GrubInstall => efi::grub_install_steps(false)
            .iter()
            .flat_map(|s| s.describe())
            .collect(),
        Fix::Initrd => stack::update_initramfs_u(false).describe(),
        Fix::RecoverBundle => {
            let mut c = vec![format!("write {} (mode 0755)", stack::RAIDEN_RECOVERY_HOOK)];
            c.extend(stack::update_initramfs_u(false).describe());
            c
        }
        Fix::LuksBackup => vec![
            "rm -f /boot/luks/*-headers.bin".into(),
            "cryptsetup luksHeaderBackup <member>3 --header-backup-file /boot/luks/<member>3-headers.bin (per member)".into(),
        ],
    }
}

/// the ordered commands a uuid re-stamp would run: a tune2fs/mkfs per divergent
/// mirror, then the re-sync. re-observes live state, exactly like the apply path.
fn restamp_commands(kind: UuidKind, layout: &Layout) -> Vec<String> {
    let (mount, uuid, targets) = match restamp_targets(kind, layout) {
        Ok(t) => t,
        Err(e) => return vec![format!("(cannot preview: {e})")],
    };
    if targets.is_empty() {
        return vec![format!(
            "(nothing to do: all {mount} mirrors already share {uuid})"
        )];
    }
    let sync_cmd = match kind {
        UuidKind::Boot => "raiden sync boot --yes",
        UuidKind::Esp => "raiden sync efi --yes",
    };
    let mut lines: Vec<String> = targets
        .iter()
        .map(|d| restamp_argv(kind, d, &uuid).join(" "))
        .collect();
    lines.push(format!(
        "{sync_cmd}   # rsync the bootloader from {mount} onto the re-stamped mirrors"
    ));
    lines
}

/// apply every available fix, mutating the check list in place so the printed
/// table reflects the post-fix state. each fix is confirmed individually (--yes
/// auto-accepts); a declined fix is left as-is and noted. a fix that errors leaves
/// the original status and appends the error to the detail.
fn apply_fixes(
    checks: &mut [Check],
    cfg: &Config,
    layout: &Layout,
    verbose: bool,
    yes: bool,
    stack: &dyn stack::Stack,
) -> Result<()> {
    for c in checks.iter_mut() {
        let Some(fix) = c.fix.clone() else {
            continue;
        };
        let question = format!("{} ({}): {}?", c.name, first_line(&c.detail), fix.action());
        if !prompt::confirm_or_yes(yes, &question)? {
            c.detail = format!("{} (fix skipped)", c.detail);
            continue;
        }
        match apply_one(&fix, cfg, layout, verbose, stack) {
            Ok(detail) => {
                c.status = FIXED;
                c.detail = detail;
                c.fix = None;
            }
            Err(e) => {
                c.detail = format!("{} (fix failed: {e})", c.detail);
            }
        }
    }
    Ok(())
}

/// the first line of a possibly-multi-line detail, for compact prompts/rows.
fn first_line(detail: &str) -> &str {
    detail.lines().next().unwrap_or("")
}

fn apply_one(
    fix: &Fix,
    cfg: &Config,
    layout: &Layout,
    verbose: bool,
    stack: &dyn stack::Stack,
) -> Result<String> {
    match fix {
        Fix::MemberIds => member_ids_fix(cfg, layout, stack, verbose),
        Fix::Fstab => fstab_fix(layout),
        Fix::Crypttab => crypttab_fix(layout, stack, verbose),
        Fix::GrubInstall => run_steps(efi::grub_install_steps(false), "re-ran grub-install"),
        Fix::Initrd => {
            stack::update_initramfs_u(false).execute(None)?;
            Ok("rebuilt the initramfs".into())
        }
        Fix::RecoverBundle => recover_bundle_fix(),
        Fix::LuksBackup => luks_backup_fix(layout),
        Fix::BootHook(dir) => {
            install_boot_hook(dir)?;
            Ok(format!("installed {}", hook_path(dir)))
        }
        Fix::EfiHook => {
            install_efi_hook()?;
            Ok(format!("installed {}", efi_hook_path()))
        }
        Fix::SyncBoot => {
            let target = SyncTarget::Boot(BootSyncArgs { force: false });
            sync::run(&target, layout, true, false, verbose, false)?;
            Ok("re-synced /boot mirrors".into())
        }
        Fix::SyncEfi => {
            let target = SyncTarget::Efi(EfiSyncArgs { force: false });
            sync::run(&target, layout, true, false, verbose, false)?;
            Ok("re-synced esp mirrors".into())
        }
        Fix::RestampUuid(kind) => restamp_uuid(*kind, layout, verbose),
        Fix::EfiBootEntries => efibootmgr_fix(layout),
        Fix::Debconf => debconf_fix(),
    }
}

/// re-stamp the shared fs uuid onto every divergent mirror, then re-sync content.
/// re-observes current state rather than trusting the check-time snapshot (a
/// reconcile: read the live source, act only on the delta, converge). safety: it
/// requires the live mount present (its uuid is the truth being propagated), never
/// touches the live source device, skips already-shared mirrors, and only acts on
/// configured member partitions. /boot is re-stamped in place (tune2fs, non-
/// destructive); an esp is reformatted with the shared volume id (vfat has no in-
/// place serial change with base tools) and repopulated by the sync that follows.
fn restamp_uuid(kind: UuidKind, layout: &Layout, verbose: bool) -> Result<String> {
    let (mount, uuid, targets) = restamp_targets(kind, layout)?;
    for dev in &targets {
        run_ok(&restamp_argv(kind, dev, &uuid))?;
    }
    // repopulate content onto the (esp: reformatted) mirrors from the live source.
    let target = match kind {
        UuidKind::Boot => SyncTarget::Boot(BootSyncArgs { force: false }),
        UuidKind::Esp => SyncTarget::Efi(EfiSyncArgs { force: false }),
    };
    sync::run(&target, layout, true, false, verbose, false)?;
    Ok(format!(
        "re-stamped {} {mount} mirror(s) to {uuid} and re-synced",
        targets.len()
    ))
}

/// resolve the live source uuid and the divergent mirror devices to re-stamp:
/// present, not the live source, not already sharing the uuid. shared by the apply
/// path and the dry-run preview so they agree exactly.
fn restamp_targets(kind: UuidKind, layout: &Layout) -> Result<(&'static str, String, Vec<String>)> {
    let (mount, devices) = match kind {
        UuidKind::Boot => ("/boot", layout.boot_devices()),
        UuidKind::Esp => (ESP_MOUNT, layout.esp_devices()),
    };
    let src = mount_source(mount)
        .ok_or_else(|| anyhow!("{mount} is not mounted; mount it first, then re-run --fix"))?;
    let uuid =
        blkid_uuid(&src).ok_or_else(|| anyhow!("cannot read the live {mount} uuid from {src}"))?;
    let targets: Vec<String> = devices
        .iter()
        .filter(|d| !sync::is_same_device(d, &src) && Path::new(d).exists())
        .filter(|d| blkid_uuid(d).as_deref() != Some(uuid.as_str()))
        .cloned()
        .collect();
    Ok((mount, uuid, targets))
}

/// the command that re-stamps one mirror partition with the shared uuid. /boot
/// uses tune2fs (in place); an esp is reformatted with the shared vfat volume id
/// (its content is reconstructible and the caller re-syncs it). returned as argv
/// so the dry-run preview can print it and the real run can execute it.
fn restamp_argv(kind: UuidKind, dev: &str, uuid: &str) -> Vec<String> {
    match kind {
        // /boot is re-stamped in place; an esp is reformatted (vfat has no in-place
        // serial change) with the shared volume id -- the same mkfs as install/replace.
        UuidKind::Boot => ["tune2fs", "-U", uuid, dev].map(String::from).to_vec(),
        UuidKind::Esp => efi::mkfs_esp_argv(dev, Some(&uuid.replace('-', ""))),
    }
}

/// run a command, inheriting stdio; ok iff it succeeds.
fn run_ok(argv: &[String]) -> Result<()> {
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("running {}", argv.join(" ")))?;
    if status.success() {
        Ok(())
    } else {
        bail!("{} exited {status}", argv.join(" "))
    }
}

/// run each step now (the trivial-fix path: doctor builds the same establish step
/// install/replace do, parameterized for the running system, and executes it).
fn run_steps(steps: Vec<Step>, ok_msg: &str) -> Result<String> {
    for s in &steps {
        s.execute(None)?;
    }
    Ok(ok_msg.to_string())
}

/// append the missing /boot and /boot/efi fstab entries (idempotent: only adds an
/// entry whose mount point is not already present). the line format matches the
/// install's fstab steps.
fn fstab_fix(layout: &Layout) -> Result<String> {
    let mut sh = String::new();
    if let Some(dev) = layout.boot_devices().into_iter().next() {
        sh.push_str(&format!(
            "grep -q ' /boot ' /etc/fstab || {{ u=$(blkid -s UUID -o value {dev}); \
             echo \"UUID=$u /boot ext4 defaults,nofail 0 2\" >> /etc/fstab; }}; "
        ));
    }
    if let Some(esp) = layout.esp_devices().into_iter().next() {
        sh.push_str(&format!(
            "grep -q ' {ESP_MOUNT} ' /etc/fstab || {{ u=$(blkid -s UUID -o value {esp}); \
             echo \"UUID=$u {ESP_MOUNT} vfat {opts},nofail 0 0\" >> /etc/fstab; }}",
            opts = crate::pipeline::EFI_OPTS,
        ));
    }
    Step::sh("ensure fstab entries", sh).execute(None)?;
    Ok("appended the missing fstab entry(ies)".into())
}

/// regenerate /etc/crypttab from the layout -- the stack's crypttab_regen, the same
/// builder replace uses -- then bring the running mappings and every initrd in
/// line with it: an initrd still carrying the old crypttab is what unlocks the
/// next boot. None means an array-based crypttab (md_integrity), which a member
/// change does not affect.
fn crypttab_fix(layout: &Layout, stack: &dyn stack::Stack, verbose: bool) -> Result<String> {
    let Some(step) = stack.crypttab_regen(layout) else {
        return Ok("crypttab is array-based; nothing to regenerate".into());
    };
    step.execute(None)?;
    let renamed = rename_open_mappings(layout)?;
    refresh_boot(layout, stack, verbose)?;
    Ok(format!(
        "regenerated {CRYPTTAB_PATH}, renamed {renamed} open mapping(s), rebuilt and verified the initramfs"
    ))
}

/// record the discovered stable ids in the manifest, then rebuild the initrd
/// (which bakes the manifest for `raiden recover`) and push it to the mirrors.
fn member_ids_fix(
    cfg: &Config,
    layout: &Layout,
    stack: &dyn stack::Stack,
    verbose: bool,
) -> Result<String> {
    Manifest::from_config(cfg).save()?;
    refresh_boot(layout, stack, verbose)?;
    Ok(format!(
        "recorded {} stable id(s) in the manifest, rebuilt and verified the initramfs",
        cfg.disks.ids.len()
    ))
}

/// rename each member's open crypt mapping to its configured name, so the running
/// system agrees with the rewritten crypttab (dmsetup renames a live device in
/// place). returns how many were renamed.
fn rename_open_mappings(layout: &Layout) -> Result<usize> {
    let renames = open_renames(layout);
    for (old, new) in &renames {
        run_ok(&["dmsetup".into(), "rename".into(), old.clone(), new.clone()])?;
    }
    Ok(renames.len())
}

/// the (open name, wanted name) dm renames that bring every member in line with
/// the layout.
fn open_renames(layout: &Layout) -> Vec<(String, String)> {
    layout
        .members
        .iter()
        .filter_map(|d| {
            let part = disk_id::kernel_name_of(&layout.part(d, 3))?;
            Some(member_renames(
                Path::new(SYS_BLOCK),
                &part,
                &layout.crypt_name(d),
            ))
        })
        .flatten()
        .collect()
}

/// the dm renames for one member's root partition: its crypt device and, when
/// luks2 built-in integrity stacks one under it, the matching <name>_dif device,
/// so the pair keeps the naming cryptsetup itself gives them at open.
fn member_renames(sys: &Path, part: &str, want: &str) -> Vec<(String, String)> {
    let chain = crypt_chain(sys, part);
    let Some((open, below)) = chain.split_last() else {
        return Vec::new();
    };
    if open == want {
        return Vec::new();
    }
    let open_dif = format!("{open}{INTEGRITY_SUFFIX}");
    let mut renames = vec![(open.clone(), want.to_string())];
    if below.contains(&open_dif) {
        renames.push((open_dif, format!("{want}{INTEGRITY_SUFFIX}")));
    }
    renames
}

/// the dm names stacked on a block device, bottom up, ending at its dm-crypt
/// device; empty when none is open. the luks uuid prefix is what identifies the
/// crypt layer: the integrity layer below it is CRYPT-SUBDEV-.
fn crypt_chain(sys: &Path, kernel: &str) -> Vec<String> {
    let dm_attr = |dev: &str, attr: &str| {
        fs::read_to_string(sys.join(dev).join("dm").join(attr))
            .ok()
            .map(|s| s.trim().to_string())
    };
    let Ok(holders) = fs::read_dir(sys.join(kernel).join("holders")) else {
        return Vec::new();
    };
    for holder in holders.flatten() {
        let dev = holder.file_name().to_string_lossy().into_owned();
        let Some(name) = dm_attr(&dev, "name") else {
            continue;
        };
        let is_crypt = dm_attr(&dev, "uuid").is_some_and(|u| u.starts_with(LUKS_DM_UUID_PREFIX));
        let above = if is_crypt {
            Vec::new()
        } else {
            crypt_chain(sys, &dev)
        };
        if is_crypt || !above.is_empty() {
            return std::iter::once(name).chain(above).collect();
        }
    }
    Vec::new()
}

/// rebuild every initrd, push /boot to the independent mirrors, then confirm each
/// initrd's crypttab unlocks every member -- a server whose initrd misses a member
/// cannot assemble its array at boot, so this fails loudly rather than reporting a
/// fix that did not take.
fn refresh_boot(layout: &Layout, stack: &dyn stack::Stack, verbose: bool) -> Result<()> {
    stack::update_initramfs_u(false).execute(None)?;
    if !layout.boot_raid() {
        let target = SyncTarget::Boot(BootSyncArgs { force: false });
        sync::run(&target, layout, true, false, verbose, false)?;
    }
    let problems = initrd_crypttab_problems(layout, stack);
    if !problems.is_empty() {
        bail!("initrd verification failed: {}", problems.join("; "));
    }
    Ok(())
}

/// the commands refresh_boot runs, for the dry-run preview.
fn refresh_commands(layout: &Layout) -> Vec<String> {
    let mut c = stack::update_initramfs_u(false).describe();
    if !layout.boot_raid() {
        c.push("raiden sync boot --yes".into());
    }
    c.push("verify each /boot/initrd.img-*'s cryptroot/crypttab lists every member".into());
    c
}

/// for per-member crypttab stacks, each /boot initrd whose baked crypttab lacks a
/// member (or cannot be read). empty when all is well or the stack's crypttab is
/// array-based.
fn initrd_crypttab_problems(layout: &Layout, stack: &dyn stack::Stack) -> Vec<String> {
    if stack.crypttab_regen(layout).is_none() {
        return Vec::new();
    }
    let names = layout.crypt_names();
    let mut initrds: Vec<String> = fs::read_dir("/boot")
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("initrd.img-"))
                .map(|e| e.path().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    initrds.sort();
    initrds
        .iter()
        .filter_map(|i| match sync::initrd_crypttab(i) {
            Ok(t) => {
                let miss = missing_crypttab_names(&t, &names);
                (!miss.is_empty()).then(|| format!("{i} lacks {}", miss.join(", ")))
            }
            Err(e) => Some(e),
        })
        .collect()
}

/// stable ids for members the manifest has none for (an install predating them).
/// a per-member crypttab stack maps each member through its crypttab entry: the
/// luks uuid identifies the partition the member was installed on, whatever the
/// kernel calls it now. md_integrity's crypttab is array-wide, so its members
/// fall back to their current kernel name (its mapper names are kernel-derived
/// at boot anyway).
fn discover_ids(cfg: &Config, stack: &dyn stack::Stack) -> BTreeMap<String, String> {
    let layout = Layout::derive(cfg);
    let per_member = stack.crypttab_regen(&layout).is_some();
    let crypttab = fs::read_to_string(CRYPTTAB_PATH).unwrap_or_default();
    layout
        .members
        .iter()
        .filter(|m| !cfg.disks.ids.contains_key(*m))
        .filter_map(|m| {
            let id = if per_member {
                let uuid = crypttab_uuid_of(&crypttab, &layout.crypt_name(m))?;
                let part = disk_id::kernel_name_of(&format!("/dev/disk/by-uuid/{uuid}"))?;
                disk_id::by_id_for(&disk_id::parent_disk(&part)?)
            } else {
                disk_id::by_id_for(m)
            };
            id.map(|i| (m.clone(), i))
        })
        .collect()
}

/// every member is recorded by a stable id, so a kernel rename cannot point raiden
/// at the wrong disk. `found` are ids discovered this run but not yet persisted.
fn check_member_ids(cfg: &Config, layout: &Layout, found: &BTreeMap<String, String>) -> Check {
    let describe = |m: &String| match cfg.disks.ids.get(m) {
        Some(id) => format!("{m} -> {id} (now {})", layout.disk(m)),
        None => format!("{m} (no by-id link; kernel name)"),
    };
    let all: Vec<String> = layout.members.iter().map(describe).collect();
    if found.is_empty() {
        return Check::ok("member ids", all.join("\n"));
    }
    let new: Vec<String> = found.keys().map(describe).collect();
    Check::warn(
        "member ids",
        format!(
            "not recorded in the manifest: {}\n{}",
            new.join(", "),
            all.join("\n")
        ),
    )
    .with_fix(Fix::MemberIds)
}

/// re-back-up the luks headers to /boot, the same builder install uses (targeting
/// /boot, not /mnt/boot). clears the existing backups first since cryptsetup refuses
/// to overwrite.
fn luks_backup_fix(layout: &Layout) -> Result<String> {
    run_ok(&[
        "sh".into(),
        "-c".into(),
        "rm -f /boot/luks/*-headers.bin".into(),
    ])?;
    run_steps(
        stack::backup_luks_headers(layout, "/boot"),
        "re-backed-up the luks headers to /boot",
    )
}

fn print_table(checks: &[Check], verbose: bool) {
    let width = checks
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let header = format!("{:<width$}  status  detail", "", width = width);
    println!("{header}");
    for c in checks {
        let mark = match c.status {
            OK => "ok",
            WARN => "warn",
            FIXED => "fixed",
            _ => "fail",
        };
        let detail = if verbose {
            c.detail.clone()
        } else {
            // collapse multi-line detail to its first line in the compact view.
            first_line(&c.detail).to_string()
        };
        let row = format!(
            "{:<width$}  {:<5}   {}",
            c.name,
            mark,
            detail,
            width = width
        );
        println!("{row}");
    }
}

/// each member disk exists as a /dev node.
fn check_disk_presence(layout: &Layout) -> Vec<Check> {
    layout
        .members
        .iter()
        .map(|d| {
            let dev = layout.disk(d);
            if std::path::Path::new(&dev).exists() {
                Check::ok("disk presence", format!("{d}: {dev} present"))
            } else {
                Check::fail("disk presence", format!("{d}: {dev} missing"))
            }
        })
        .collect()
}

/// /boot is mounted, and from a configured boot device.
fn check_boot_mount(layout: &Layout) -> Vec<Check> {
    let mut out = Vec::new();
    match mount_source("/boot") {
        Some(s) => {
            let known = layout.boot_devices();
            let on_member = known.iter().any(|d| sync::is_same_device(d, &s));
            if on_member {
                out.push(Check::ok("boot mount", format!("/boot mounted from {s}")));
            } else {
                out.push(Check::warn(
                    "boot mount",
                    format!("/boot mounted from {s}, not a configured boot device"),
                ));
            }
        }
        None => out.push(Check::fail("boot mount", "/boot is not mounted")),
    }
    out
}

/// /boot/efi is mounted (efi mode).
fn check_esp_mount() -> Vec<Check> {
    match mount_source(ESP_MOUNT) {
        Some(s) => vec![Check::ok(
            "esp mount",
            format!("{ESP_MOUNT} mounted from {s}"),
        )],
        None => vec![Check::fail(
            "esp mount",
            format!("{ESP_MOUNT} is not mounted"),
        )],
    }
}

/// /boot and (efi) /boot/efi fstab entries exist and reference a resolvable uuid.
fn check_fstab(efi: bool) -> Vec<Check> {
    let mut out = Vec::new();
    if fstab_uuid("/boot").is_some() {
        out.push(Check::ok("fstab", "/boot entry present"));
    } else {
        out.push(Check::fail("fstab", "no /boot entry in /etc/fstab").with_fix(Fix::Fstab));
    }
    if efi {
        if fstab_uuid(ESP_MOUNT).is_some() {
            out.push(Check::ok("fstab", format!("{ESP_MOUNT} entry present")));
        } else {
            out.push(
                Check::fail("fstab", format!("no {ESP_MOUNT} entry in /etc/fstab"))
                    .with_fix(Fix::Fstab),
            );
        }
    }
    out
}

/// the shared-uuid invariant behind "mount /boot and /boot/efi from any survivor":
/// every member's /boot carries one ext4 uuid and (efi) every member's esp one
/// vfat uuid, matching the fstab entry. a member whose partition has a divergent
/// uuid cannot serve the mount when the primary is lost -- a silent loss of
/// redundancy no other check catches (content drift is separate). a warn (the
/// system still boots from the primary); rebuild that member to re-stamp the
/// shared uuid (`raiden replace --disks <m> --esp`/`--boot`).
fn check_uuid_sharing(layout: &Layout, efi: bool) -> Vec<Check> {
    let mut out = Vec::new();
    if !layout.boot_raid() {
        out.push(uuid_set_check(
            "boot uuid",
            UuidKind::Boot,
            &layout.boot_devices(),
            "/boot",
        ));
    }
    if efi {
        out.push(uuid_set_check(
            "esp uuid",
            UuidKind::Esp,
            &layout.esp_devices(),
            ESP_MOUNT,
        ));
    }
    out
}

/// the I/O half: collect each present device's uuid and the canonical (fstab)
/// uuid, then hand off to the pure decision. kept thin so the policy is testable
/// without disks -- the first step of splitting the predicate from presentation.
fn uuid_set_check(name: &'static str, kind: UuidKind, devices: &[String], mount: &str) -> Check {
    let present: Vec<(String, String)> = devices
        .iter()
        .filter(|d| Path::new(d).exists())
        .filter_map(|d| blkid_uuid(d).map(|u| (d.clone(), u)))
        .collect();
    let expected = fstab_uuid(mount).or_else(|| present.first().map(|(_, u)| u.clone()));
    uuid_set_result(name, kind, &present, expected.as_deref())
}

/// the pure half: decide ok/warn from the collected uuids and attach the re-stamp
/// fix on divergence. every present member must share one uuid, matching the
/// canonical `expected` when known; fewer than two present is nothing to compare.
fn uuid_set_result(
    name: &'static str,
    kind: UuidKind,
    present: &[(String, String)],
    expected: Option<&str>,
) -> Check {
    if present.len() < 2 {
        return Check::ok(
            name,
            format!(
                "{} device(s) with a uuid; nothing to compare",
                present.len()
            ),
        );
    }
    let expected = expected.unwrap_or(present[0].1.as_str());
    let diverged = uuid_divergences(present, expected);
    if diverged.is_empty() {
        Check::ok(name, format!("{} members share {expected}", present.len()))
    } else {
        Check::warn(
            name,
            format!("not shared (expected {expected}): {}", diverged.join(", ")),
        )
        .with_fix(Fix::RestampUuid(kind))
    }
}

/// the "dev=uuid" of every member whose uuid is not the expected one. pure.
fn uuid_divergences(present: &[(String, String)], expected: &str) -> Vec<String> {
    present
        .iter()
        .filter(|(_, u)| u != expected)
        .map(|(d, u)| format!("{d}={u}"))
        .collect()
}

/// each member's root partition has a crypttab entry for its luks uuid, under
/// the member's mapper name.
fn check_crypttab(layout: &Layout) -> Vec<Check> {
    let crypttab = fs::read_to_string(CRYPTTAB_PATH).unwrap_or_default();
    layout
        .members
        .iter()
        .map(|d| {
            let uuid = blkid_uuid(&layout.part(d, 3));
            crypttab_check(&layout.crypt_name(d), uuid.as_deref(), &crypttab)
        })
        .collect()
}

/// (name, uuid) of each `<name> UUID=<uuid> ...` crypttab entry. pure.
fn crypttab_entries(crypttab: &str) -> impl Iterator<Item = (&str, &str)> {
    crypttab
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let name = f.next()?;
            Some((name, f.next()?.strip_prefix("UUID=")?))
        })
}

/// the luks uuid crypttab records for a mapper name. pure.
fn crypttab_uuid_of(crypttab: &str, name: &str) -> Option<String> {
    crypttab_entries(crypttab)
        .find(|(n, _)| *n == name)
        .map(|(_, u)| u.to_string())
}

/// judge one member's crypttab entry. the entry is found by the disk's luks uuid
/// (identity), not by mapper name: a name-keyed lookup flagged a correct crypttab
/// whenever the kernel renamed the disks. pure.
fn crypttab_check(name: &str, uuid: Option<&str>, crypttab: &str) -> Check {
    let Some(uuid) = uuid else {
        return Check::warn(
            "crypttab",
            format!("{name}: cannot read the disk's luks uuid"),
        );
    };
    match crypttab_entries(crypttab).find(|(_, u)| *u == uuid) {
        Some((n, _)) if n == name => Check::ok("crypttab", format!("{name}: uuid matches")),
        Some((n, _)) => Check::warn(
            "crypttab",
            format!("{name}: luks {uuid} is listed as {n}, expected {name}"),
        )
        .with_fix(Fix::Crypttab),
        None => Check::fail("crypttab", format!("{name}: no entry for luks {uuid}"))
            .with_fix(Fix::Crypttab),
    }
}

/// the mapper names an initrd crypttab lacks (each would not unlock at boot). pure.
fn missing_crypttab_names(crypttab: &str, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|n| !crypttab_entries(crypttab).any(|(e, _)| e == n.as_str()))
        .cloned()
        .collect()
}

/// each member's luks header is valid (cryptsetup luksDump succeeds).
fn check_luks_headers(layout: &Layout) -> Vec<Check> {
    layout
        .members
        .iter()
        .map(|d| {
            let dev = layout.part(d, 3);
            let out = Command::new("cryptsetup").args(["luksDump", &dev]).output();
            match out {
                Ok(o) if o.status.success() => {
                    Check::ok("luks headers", format!("{dev}: header valid"))
                }
                Ok(o) => Check::fail(
                    "luks headers",
                    format!(
                        "{dev}: luksDump failed: {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                ),
                Err(e) => Check::fail("luks headers", format!("{dev}: {e}")),
            }
        })
        .collect()
}

/// each member's luks header is backed up to /boot/luks (install writes them, for
/// disaster recovery). a missing backup means a corrupted header cannot be restored.
fn check_luks_backup(layout: &Layout) -> Vec<Check> {
    let missing: Vec<String> = layout
        .members
        .iter()
        .filter(|d| !Path::new(&format!("/boot/luks/{d}3-headers.bin")).exists())
        .map(|d| format!("{d}3-headers.bin"))
        .collect();
    if missing.is_empty() {
        vec![Check::ok(
            "luks backup",
            format!("{} header backup(s) on /boot", layout.members.len()),
        )]
    } else {
        vec![
            Check::warn("luks backup", format!("missing: {}", missing.join(", ")))
                .with_fix(Fix::LuksBackup),
        ]
    }
}

/// array health: md detail, zpool status, or btrfs device stats.
fn check_array_status(layout: &Layout, family: Family) -> Vec<Check> {
    match family {
        Family::Md => {
            // two distinct checks -- name them so the boot and root arrays are not
            // two ambiguous "md array" lines. the boot array is absent under
            // independent /boot (the default), so it reports "not used" there.
            let mut out = md_check("md boot", BOOT_MD_DEVICE, layout.boot_raid());
            out.extend(md_check("md root", "/dev/md/root", true));
            out
        }
        Family::Zfs => match Command::new("zpool").arg("status").output() {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                if text.contains("DEGRADED") {
                    vec![Check::warn("zfs status", "pool degraded")]
                } else if text.contains("FAULTED") {
                    vec![Check::fail("zfs status", "pool has a faulted vdev")]
                } else {
                    vec![Check::ok("zfs status", "pool online")]
                }
            }
            _ => vec![Check::fail("zfs status", "zpool status failed")],
        },
        Family::Btrfs | Family::Bcachefs => {
            // btrfs: device stats per member; bcachefs: device state via show-super.
            let dev = layout.crypt_device(&layout.members[0]);
            match Command::new("btrfs")
                .args(["device", "stats", &dev])
                .output()
            {
                Ok(o) if o.status.success() => {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let errors = text.lines().filter(|l| l.contains("[*]")).count();
                    if errors == 0 {
                        vec![Check::ok("btrfs status", "no device errors")]
                    } else {
                        vec![Check::warn(
                            "btrfs status",
                            format!("{errors} device error(s)"),
                        )]
                    }
                }
                _ => {
                    vec![Check::warn(
                        "fs status",
                        "could not read device stats (bcachefs?)",
                    )]
                }
            }
        }
    }
}

/// one md array (named `name`, eg. "md boot" / "md root"): present, not failed,
/// not degraded. `expected` is false for an array that should not exist (the boot
/// array under independent /boot), which reports "not used".
fn md_check(name: &'static str, device: &str, expected: bool) -> Vec<Check> {
    if !expected {
        return vec![Check::ok(name, "not used (independent boot)")];
    }
    match Command::new("mdadm").args(["--detail", device]).output() {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            if text.contains("State : active") || text.contains("State : clean") {
                vec![Check::ok(name, format!("{device}: active"))]
            } else if text.contains("degraded") {
                vec![Check::warn(name, format!("{device}: degraded"))]
            } else {
                vec![Check::warn(
                    name,
                    format!("{device}: {}", first_state(&text)),
                )]
            }
        }
        _ => vec![Check::fail(
            name,
            format!("{device}: mdadm --detail failed"),
        )],
    }
}

fn first_state(md_detail: &str) -> String {
    md_detail
        .lines()
        .find(|l| l.trim_start().starts_with("State :"))
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| "state unknown".into())
}

/// independent /boot: each mirror's boot device exists and is a block device.
fn check_boot_mirrors(layout: &Layout) -> Vec<Check> {
    if layout.boot_raid() {
        return vec![Check::ok("boot mirrors", "n/a (md raid1 /boot)")];
    }
    layout
        .boot_devices()
        .iter()
        .map(|dev| {
            if std::path::Path::new(dev).exists() {
                Check::ok("boot mirrors", format!("{dev} present"))
            } else {
                Check::fail("boot mirrors", format!("{dev} missing"))
            }
        })
        .collect()
}

/// efi: each mirror esp exists and is a block device.
fn check_esp_mirrors(layout: &Layout) -> Vec<Check> {
    layout
        .esp_devices()
        .iter()
        .map(|dev| {
            if std::path::Path::new(dev).exists() {
                Check::ok("esp mirrors", format!("{dev} present"))
            } else {
                Check::fail("esp mirrors", format!("{dev} missing"))
            }
        })
        .collect()
}

/// grub-install ran: efi -> shimx64.efi on each esp; bios -> grub-install present.
fn check_grub(layout: &Layout, efi: bool) -> Vec<Check> {
    if efi {
        // /boot/efi mounts by the shared esp uuid, so it can land on any survivor
        // (not necessarily the first member); check the bootloader on whichever esp
        // is actually live, the others are cold mirrors. fall back to the configured
        // primary if findmnt cannot name the backing device, so the live esp's
        // bootloader is always checked.
        let mounted = mount_source(ESP_MOUNT);
        let is_live = |dev: &str| {
            mounted
                .as_deref()
                .is_some_and(|m| sync::is_same_device(dev, m))
        };
        let any_live = layout.esp_devices().iter().any(|d| is_live(d));
        layout
            .esp_devices()
            .iter()
            .map(|dev| {
                let check_here = if any_live {
                    is_live(dev)
                } else {
                    layout.esp_is_primary(&member_of(dev, layout))
                };
                if check_here {
                    let shim = format!("{ESP_MOUNT}/{}", efi::SHIM_FILE);
                    if std::path::Path::new(&shim).exists() {
                        Check::ok("grub", format!("{dev}: shimx64.efi present (live esp)"))
                    } else {
                        Check::fail("grub", format!("{shim} missing (grub-install?)"))
                            .with_fix(Fix::GrubInstall)
                    }
                } else {
                    Check::ok("grub", format!("{dev}: mirror (cold)"))
                }
            })
            .collect()
    } else {
        // bios: grub-install writes to the mbr; the grub-pc package being installed
        // is the best proxy without inspecting the mbr.
        match Command::new("dpkg-query")
            .args(["-W", "-f", "${Status}", "grub-pc"])
            .output()
        {
            Ok(o)
                if o.status.success()
                    && String::from_utf8_lossy(&o.stdout).contains("install ok installed") =>
            {
                vec![Check::ok("grub", "grub-pc installed")]
            }
            _ => vec![Check::fail("grub", "grub-pc not installed")],
        }
    }
}

/// map a /dev/<disk><p><n> device path back to its member disk name.
fn member_of(part_dev: &str, layout: &Layout) -> String {
    for m in &layout.members {
        if layout.part(m, 1) == part_dev
            || layout.part(m, 2) == part_dev
            || layout.part(m, 3) == part_dev
        {
            return m.clone();
        }
    }
    String::new()
}

/// one parsed efibootmgr boot entry: its number and the rest of the line (label +
/// device path), lowercased for matching.
struct BootEntry {
    num: String,
    desc: String,
}

/// parse `efibootmgr -v` output into the BootNNNN entries (ignoring the
/// BootCurrent/BootOrder/Timeout header lines). pure, so it is unit-tested.
fn parse_boot_entries(out: &str) -> Vec<BootEntry> {
    out.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("Boot")?;
            let num: String = rest.chars().take(4).collect();
            if num.len() != 4 || !num.chars().all(|c| c.is_ascii_hexdigit()) {
                return None; // BootCurrent/BootOrder/etc.
            }
            let desc = rest[4..].trim_start_matches('*').trim().to_lowercase();
            Some(BootEntry { num, desc })
        })
        .collect()
}

/// the per-disk efibootmgr entries: every member disk should have exactly one
/// boot entry that loads shim from its own esp. under the shared esp uuid (and
/// with raiden owning the nvram -- grub's update_nvram is off) the firmware boots
/// each disk by this entry or the removable EFI/BOOT fallback; a member with none
/// cannot be booted by name. duplicates and shim-bypassing grub entries accumulate
/// when something else writes nvram, so they are flagged too. warn, fixable.
fn check_efibootmgr(layout: &Layout) -> Vec<Check> {
    let Some(out) = efibootmgr_output() else {
        return vec![Check::warn(
            "efibootmgr",
            "could not read efibootmgr -v (efivars unavailable?)",
        )];
    };
    let entries = parse_boot_entries(&out);
    let mut problems = Vec::new();
    for m in &layout.members {
        let esp = layout.part(m, 1);
        if !Path::new(&esp).exists() {
            continue; // a missing disk has no entry; covered by the presence check
        }
        let Some(partuuid) = blkid_partuuid(&esp).map(|p| p.to_lowercase()) else {
            problems.push(format!("{m}: no partuuid for {esp}"));
            continue;
        };
        let mine: Vec<&BootEntry> = entries
            .iter()
            .filter(|e| e.desc.contains(&partuuid) && e.desc.contains(r"\efi\debian\"))
            .collect();
        let shim = mine
            .iter()
            .filter(|e| e.desc.contains("shimx64.efi"))
            .count();
        if shim == 0 {
            problems.push(format!("{m}: no shim boot entry"));
        } else if mine.len() > 1 {
            problems.push(format!("{m}: {} duplicate/grub entries", mine.len()));
        }
    }
    if problems.is_empty() {
        vec![Check::ok(
            "efibootmgr",
            format!(
                "{} member(s) have a clean shim boot entry",
                layout.members.len()
            ),
        )]
    } else {
        vec![Check::warn("efibootmgr", problems.join("; ")).with_fix(Fix::EfiBootEntries)]
    }
}

/// reconcile the per-disk efibootmgr entries (raiden owns the nvram): delete the
/// stale/duplicate raiden entries -- those referencing a member esp and loading
/// from EFI/debian (shim OR grub) -- then register one clean shim entry per member,
/// in reverse so the first member sorts first in BootOrder. leaves the removable
/// EFI/BOOT fallback and non-member entries (other OSes, pxe) untouched.
fn efibootmgr_fix(layout: &Layout) -> Result<String> {
    let (delete, register) = efibootmgr_plan(layout)?;
    for num in &delete {
        run_ok(&["efibootmgr".into(), "-b".into(), num.clone(), "-B".into()])?;
    }
    for disk in register.iter().rev() {
        run_ok(&efi::register_argv(&layout.disk(disk), disk))?;
    }
    Ok(format!(
        "pruned {} stale entry(ies), registered {} member shim entry(ies)",
        delete.len(),
        register.len()
    ))
}

/// resolve the efibootmgr reconcile plan: the boot numbers to delete and the
/// member disks to (re-)register. shared by the apply and the dry-run preview.
fn efibootmgr_plan(layout: &Layout) -> Result<(Vec<String>, Vec<String>)> {
    let out = efibootmgr_output().ok_or_else(|| anyhow!("could not read efibootmgr -v"))?;
    let entries = parse_boot_entries(&out);
    let members: Vec<(String, String)> = layout
        .members
        .iter()
        .filter(|m| Path::new(&layout.part(m, 1)).exists())
        .filter_map(|m| blkid_partuuid(&layout.part(m, 1)).map(|pu| (m.clone(), pu.to_lowercase())))
        .collect();
    let delete: Vec<String> = entries
        .iter()
        .filter(|e| {
            e.desc.contains(r"\efi\debian\") && members.iter().any(|(_, pu)| e.desc.contains(pu))
        })
        .map(|e| e.num.clone())
        .collect();
    let register: Vec<String> = members.into_iter().map(|(d, _)| d).collect();
    Ok((delete, register))
}

/// the ordered commands the efibootmgr fix would run, for the dry-run preview.
fn efibootmgr_commands(layout: &Layout) -> Vec<String> {
    let (delete, register) = match efibootmgr_plan(layout) {
        Ok(p) => p,
        Err(e) => return vec![format!("(cannot preview: {e})")],
    };
    let mut lines: Vec<String> = delete
        .iter()
        .map(|n| format!("efibootmgr -b {n} -B   # remove stale/duplicate entry"))
        .collect();
    lines.extend(
        register
            .iter()
            .rev()
            .map(|d| efi::register_argv(&layout.disk(d), d).join(" ")),
    );
    if lines.is_empty() {
        lines.push("(nothing to do: every member has a clean shim entry)".into());
    }
    lines
}

/// `efibootmgr -v` stdout, or None when it cannot run (no efivars).
fn efibootmgr_output() -> Option<String> {
    let out = Command::new("efibootmgr").arg("-v").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// raiden owns grub-efi's debconf (efi::GRUB_DEBCONF): the removable EFI/BOOT
/// fallback stays maintained on upgrades, but grub's own nvram management is off so
/// it does not re-add the per-disk entries raiden registers itself. a value flipped
/// back (eg. by dpkg-reconfigure) silently reintroduces nvram cruft or drops the
/// fallback on the next grub upgrade. warn, fixable by re-setting the debconf.
fn check_debconf() -> Vec<Check> {
    let out = match Command::new("debconf-show").arg(efi::GRUB_PKG).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => {
            return vec![Check::warn(
                "debconf",
                format!("could not read debconf-show {}", efi::GRUB_PKG),
            )]
        }
    };
    let wrong = debconf_mismatches(&out);
    if wrong.is_empty() {
        vec![Check::ok(
            "debconf",
            "grub-efi debconf keys as raiden owns them",
        )]
    } else {
        vec![Check::warn("debconf", wrong.join("; ")).with_fix(Fix::Debconf)]
    }
}

/// the grub-efi debconf keys whose shown value differs from what raiden owns. pure.
fn debconf_mismatches(show_out: &str) -> Vec<String> {
    efi::GRUB_DEBCONF
        .iter()
        .filter_map(|(key, want)| {
            let got = debconf_value(show_out, key);
            (got.as_deref() != Some(*want))
                .then(|| format!("{key}={} (want {want})", got.as_deref().unwrap_or("unset")))
        })
        .collect()
}

/// the value of a key in `debconf-show` output (lines like "* key: value"). pure.
fn debconf_value(show_out: &str, key: &str) -> Option<String> {
    show_out.lines().find_map(|l| {
        let (k, v) = l.trim_start_matches(['*', ' ']).split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

/// re-set grub-efi's debconf on the running system to the values raiden owns -- the
/// same selections the install preseeds (efi::grub_debconf_selections).
fn debconf_fix() -> Result<String> {
    let mut child = Command::new("debconf-set-selections")
        .stdin(Stdio::piped())
        .spawn()
        .context("running debconf-set-selections")?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(efi::grub_debconf_selections().as_bytes())?;
    if child.wait()?.success() {
        Ok("re-set the grub-efi debconf keys".into())
    } else {
        bail!("debconf-set-selections failed")
    }
}

/// the commands the debconf fix would run, for the dry-run preview.
fn debconf_commands() -> Vec<String> {
    efi::GRUB_DEBCONF
        .iter()
        .map(|(k, v)| {
            format!(
                "echo '{} {k} boolean {v}' | debconf-set-selections",
                efi::GRUB_PKG
            )
        })
        .collect()
}

/// the initrd carries the binaries needed to unlock and mount (or recover) the
/// root, and -- when initramfs recovery is enabled -- raiden + the manifest too.
fn check_initrd(stack: &dyn stack::Stack, layout: &Layout, recover_bundle: bool) -> Vec<Check> {
    let Some(initrd) = first_glob("/boot", "initrd.img-") else {
        return vec![Check::fail("initrd", "no initrd.img-* on /boot")];
    };
    let listing = match sync::initrd_listing(&initrd) {
        Ok(l) => l,
        Err(e) => return vec![Check::warn("initrd", e)],
    };
    // every binary needed to unlock and mount (or recover) the root must be in the
    // initrd -- the decrypt_keyctl keyscript + its keyctl, cryptsetup, and the
    // stack's assemble/mount tools. update-initramfs -u re-pulls them (from the
    // stock hooks; the packages are installed) when the initrd is stale.
    let missing = missing_initrd_binaries(&listing, &stack.initramfs_binaries());
    let mut out = if missing.is_empty() {
        vec![Check::ok(
            "initrd",
            format!("{initrd}: carries the boot/recovery binaries"),
        )]
    } else {
        vec![Check::warn(
            "initrd",
            format!("{initrd}: missing {}", missing.join(", ")),
        )
        .with_fix(Fix::Initrd)]
    };
    // the crypttab baked into each initrd is what unlocks the members at boot; a
    // stale one (eg. crypttab rewritten without a rebuild) misses a member.
    if stack.crypttab_regen(layout).is_some() {
        let problems = initrd_crypttab_problems(layout, stack);
        out.push(if problems.is_empty() {
            Check::ok("initrd crypttab", "every initrd unlocks every member")
        } else {
            Check::warn("initrd crypttab", problems.join("\n")).with_fix(Fix::Initrd)
        });
    }
    if recover_bundle {
        // the raiden initramfs hook is the establish mechanism: it bakes raiden +
        // the manifest into the initrd. check it is installed, executable, and
        // current, so a FUTURE rebuild keeps baking them in -- a removed, unmode, or
        // stale hook would silently drop raiden (or bake a stale one) on the next
        // update-initramfs, which the initrd-content check below (current state)
        // cannot foresee. matches the mirror-hook checks.
        out.push(check_executable_hook(
            "recover hook",
            stack::RAIDEN_RECOVERY_HOOK,
            stack::INITRAMFS_HOOK_RAIDEN,
            Fix::RecoverBundle,
        ));
        // and that the initrd currently carries raiden + the manifest. unlike the
        // stack tooling (stock hooks pull it in), a plain rebuild cannot add them if
        // the hook is absent -- the fix installs the hook first, then rebuilds.
        let miss = missing_initrd_binaries(&listing, &["raiden", "manifest.toml"]);
        out.push(if miss.is_empty() {
            Check::ok("recover", "raiden + manifest baked into the initrd")
        } else {
            Check::warn("recover", format!("initrd missing {}", miss.join(", ")))
                .with_fix(Fix::RecoverBundle)
        });
    }
    out
}

/// the required binaries absent from an lsinitramfs listing, matched by basename so
/// `keyctl` and `decrypt_keyctl` are distinct. pure.
fn missing_initrd_binaries(listing: &str, required: &[&str]) -> Vec<String> {
    required
        .iter()
        .filter(|b| !listing.lines().any(|l| l.rsplit('/').next() == Some(**b)))
        .map(|b| b.to_string())
        .collect()
}

/// the boot-mirror kernel hooks (postinst.d/postrm.d) are installed, executable,
/// and current. run-parts skips a non-executable hook, so a present-but-unmode
/// file is a silent failure; a stale hook from an older raiden actively breaks
/// kernel upgrades -- all take the same fix (re-install the current content).
fn check_kernel_hooks() -> Vec<Check> {
    let hook = stack::BOOT_MIRROR_HOOK_NAME;
    let mut out = Vec::new();
    for dir in ["postinst.d", "postrm.d"] {
        let path = format!("/etc/kernel/{dir}/{hook}");
        out.push(check_executable_hook(
            "kernel hooks",
            &path,
            stack::BOOT_MIRROR_HOOK_CONTENT,
            Fix::BootHook(dir),
        ));
    }
    out
}

/// the esp-mirror grub.d hook is installed and executable.
fn check_efi_hook() -> Vec<Check> {
    vec![check_executable_hook(
        "esp hook",
        &efi_hook_path(),
        stack::EFI_MIRROR_WRAPPER,
        Fix::EfiHook,
    )]
}

/// a mirror hook must exist, be executable, AND carry this raiden's canonical
/// content. run-parts (kernel postinst.d) and grub-mkconfig (grub.d) both skip a
/// non-executable script, and a STALE hook left by an older raiden can actively
/// break upgrades (e.g. an old boot hook that forwarded run-parts' positional args
/// to `raiden sync boot`, which clap rejects -- failing the hook and blocking the
/// kernel upgrade). a missing, unmode, or out-of-date hook all take the same fix:
/// re-install, which overwrites with the current content at 0755.
fn check_executable_hook(name: &'static str, path: &str, expected: &str, fix: Fix) -> Check {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return Check::warn(name, format!("{path} missing")).with_fix(fix),
    };
    if meta.permissions().mode() & 0o111 == 0 {
        return Check::warn(
            name,
            format!("{path} not executable (run-parts would skip it)"),
        )
        .with_fix(fix);
    }
    match fs::read_to_string(path) {
        Ok(c) if c == expected => Check::ok(name, format!("{path} present, executable, current")),
        Ok(_) => Check::warn(
            name,
            format!("{path} out of date (content differs from this raiden)"),
        )
        .with_fix(fix),
        Err(_) => Check::warn(name, format!("{path} unreadable")).with_fix(fix),
    }
}

/// note when /boot and /boot/efi are mounted from different physical disks.
/// this is expected and benign: both mount by a shared fs uuid (so any survivor
/// can serve either), and the two resolve independently -- so they may land on
/// different disks. it does not affect sync (both are source-driven). surfaced
/// so an operator is not surprised -- no action is needed.
fn check_mount_consistency(layout: &Layout, efi: bool) -> Vec<Check> {
    let boot = mount_source("/boot");
    let esp = if efi { mount_source(ESP_MOUNT) } else { None };
    match (boot.as_deref(), esp.as_deref()) {
        (Some(b), Some(e)) => {
            let same_disk = layout.members.iter().any(|m| {
                let p1 = layout.part(m, 1);
                let p2 = layout.part(m, 2);
                sync::is_same_device(&p2, b) && sync::is_same_device(&p1, e)
            });
            if same_disk {
                vec![Check::ok(
                    "mount consistency",
                    format!("/boot and {ESP_MOUNT} on the same disk ({b})"),
                )]
            } else {
                vec![Check::ok(
                    "mount consistency",
                    format!("/boot on {b}, {ESP_MOUNT} on {e}"),
                )]
            }
        }
        _ => Vec::new(),
    }
}

/// independent /boot: compare each mirror against the live source via a
/// read-only rsync dry-run. drift is a warn (the system still boots from any
/// copy), fixable by `raiden sync boot`.
fn check_boot_drift(layout: &Layout) -> Vec<Check> {
    let (src, mirrors) = sync::boot_mirror_set(layout);
    let Some(src) = src else {
        return vec![Check::ok("boot drift", "/boot not mounted, skipped")];
    };
    drift_checks("boot drift", &src, &mirrors, true, Fix::SyncBoot)
}

/// efi: compare each esp mirror against the live /boot/efi source. drift is a
/// warn, fixable by `raiden sync efi`.
fn check_esp_drift(layout: &Layout) -> Vec<Check> {
    let (src, mirrors) = sync::efi_mirror_set(layout);
    let Some(src) = src else {
        return vec![Check::ok(
            "esp drift",
            format!("{ESP_MOUNT} not mounted, skipped"),
        )];
    };
    drift_checks("esp drift", &src, &mirrors, false, Fix::SyncEfi)
}

/// efi: every member esp independently carries the bootloader (shim + grub).
fn check_esp_bootloader(layout: &Layout) -> Vec<Check> {
    bootloader_set_check(
        "esp bootloader",
        layout.esp_devices(),
        sync::verify_efi,
        ESP_MOUNT,
        "espchk",
        Fix::SyncEfi,
    )
}

/// independent /boot: every member /boot independently carries grub.cfg, a kernel,
/// and a cryptsetup-bearing initrd.
fn check_boot_bootloader(layout: &Layout) -> Vec<Check> {
    bootloader_set_check(
        "boot bootloader",
        layout.boot_devices(),
        sync::verify_boot_files,
        "/boot",
        "bootchk",
        Fix::SyncBoot,
    )
}

/// every member partition in a set is independently bootable, checked by transient-
/// mounting each one read-only and verifying its bootloader files -- not just the
/// live mount. under shared esp/boot uuids the mount can land on any member, so
/// each must carry its own bootloader; a missing one is a silent loss of redundancy
/// (and, if the live mount lands on it, a broken boot path) that the drift check --
/// which trusts the source -- cannot catch. warn, fixable by a re-sync from a good
/// source.
fn bootloader_set_check(
    name: &'static str,
    devices: Vec<String>,
    verify: fn(&str) -> std::result::Result<(), Vec<String>>,
    live_mount: &str,
    prefix: &'static str,
    fix: Fix,
) -> Vec<Check> {
    // the presence check covers missing devices; only verify the ones that exist,
    // and report that count so an all-absent set does not claim members are healthy.
    let present: Vec<&String> = devices.iter().filter(|d| Path::new(d).exists()).collect();
    // the member backing the live mount must be verified in place: a second
    // `mount -o ro` of an already-rw-mounted device fails "would change RO state",
    // and re-mounting it rw would break doctor's read-only contract.
    let live_dev = mount_source(live_mount);
    let mut bad = Vec::new();
    for dev in &present {
        let is_live = live_dev
            .as_deref()
            .is_some_and(|s| sync::is_same_device(dev, s));
        let result = if is_live {
            verify(live_mount)
        } else {
            match sync::mount_transient(dev, prefix, true) {
                Ok(tmp) => {
                    let r = verify(&tmp);
                    sync::unmount_transient(&tmp);
                    r
                }
                Err(e) => {
                    bad.push(format!("{dev}: cannot mount ({e})"));
                    continue;
                }
            }
        };
        if let Err(fails) = result {
            bad.push(format!("{dev}: {}", fails.join(", ")));
        }
    }
    if !bad.is_empty() {
        vec![Check::warn(name, bad.join("; ")).with_fix(fix)]
    } else if present.is_empty() {
        vec![Check::ok(name, "no member partitions present to check")]
    } else {
        vec![Check::ok(
            name,
            format!("{} member(s) carry the bootloader", present.len()),
        )]
    }
}

/// shared drift loop for boot and esp: mount each mirror read-only, run rsync in
/// dry-run --itemize-changes mode, and report the changed paths. a single warn
/// covers all drifted mirrors (one rsync failure does not hide the others).
fn drift_checks(
    name: &'static str,
    src: &str,
    mirrors: &[String],
    one_fs: bool,
    fix: Fix,
) -> Vec<Check> {
    let mut drifted = Vec::new();
    for dev in mirrors {
        // skip a mirror whose device node is gone (the presence check covers it);
        // comparing against a missing device would just error.
        if !Path::new(dev).exists() {
            continue;
        }
        match sync::drift(src, dev, one_fs) {
            Ok(changes) if changes.is_empty() => {}
            Ok(changes) => drifted.push(format!("{}: {} changed", dev, changes.len())),
            Err(e) => drifted.push(format!("{dev}: could not compare ({e})")),
        }
    }
    if drifted.is_empty() {
        vec![Check::ok(
            name,
            format!("{} mirror(s) in sync", mirrors.len()),
        )]
    } else {
        vec![Check::warn(name, drifted.join("; ")).with_fix(fix)]
    }
}

// --- hook install helpers (doctor --fix) ---

/// the installed (non-/mnt) path of the esp-mirror grub.d hook.
fn efi_hook_path() -> String {
    format!("/etc/grub.d/{}", stack::EFI_MIRROR_HOOK_NAME)
}

/// the installed path of the boot-mirror kernel hook in a given hook dir.
fn hook_path(dir: &str) -> String {
    format!("/etc/kernel/{dir}/{}", stack::BOOT_MIRROR_HOOK_NAME)
}

/// write the boot-mirror kernel hook into a kernel hook dir, executable.
fn install_boot_hook(dir: &str) -> Result<()> {
    let path = hook_path(dir);
    fs::create_dir_all(format!("/etc/kernel/{dir}"))?;
    fs::write(&path, stack::BOOT_MIRROR_HOOK_CONTENT)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// write the esp-mirror grub.d hook, executable.
fn install_efi_hook() -> Result<()> {
    let path = efi_hook_path();
    fs::create_dir_all("/etc/grub.d")?;
    fs::write(&path, stack::EFI_MIRROR_WRAPPER)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// install the raiden recovery hook, ensure the manifest is at /etc/raiden (the
/// path the hook copies into the initrd), then rebuild so the initrd carries
/// raiden + the manifest. installing the hook is the load-bearing step -- a plain
/// rebuild cannot add them when the hook is absent (eg. a legacy install). it
/// builds and runs the SAME establish steps install uses (parameterized for the
/// running system: root "", no chroot), so the two cannot drift.
fn recover_bundle_fix() -> Result<String> {
    stack::raiden_recovery_hook_step("").execute(None)?;
    // the hook copies /etc/raiden/manifest.toml; re-mirror from whichever copy
    // loads so the rebuild does not fail when only the /boot copy is present.
    if let Ok(m) = Manifest::load() {
        let _ = m.save();
    }
    stack::update_initramfs_u(false).execute(None)?;
    Ok(format!(
        "installed {} and rebuilt the initramfs",
        stack::RAIDEN_RECOVERY_HOOK
    ))
}

/// the install manifest exists and parses.
fn check_manifest() -> Vec<Check> {
    match Manifest::load() {
        Ok(_) => vec![Check::ok("manifest", "loads and parses")],
        Err(e) => vec![Check::fail("manifest", format!("cannot load: {e}"))],
    }
}

// --- shared helpers ---

/// the block device backing a mounted path, or None.
fn mount_source(path: &str) -> Option<String> {
    sync::findmnt_source(path).ok().flatten()
}

/// the UUID= value of the /etc/fstab entry for `mount`, or None.
fn fstab_uuid(mount: &str) -> Option<String> {
    let fstab = std::fs::read_to_string("/etc/fstab").ok()?;
    fstab.lines().find_map(|l| {
        let mut f = l.split_whitespace();
        let src = f.next()?;
        (f.next()? == mount)
            .then(|| src.strip_prefix("UUID=").map(String::from))
            .flatten()
    })
}

/// the UUID of a block device via blkid, or None.
fn blkid_uuid(dev: &str) -> Option<String> {
    blkid_value(dev, "UUID")
}

/// the gpt partition guid (PARTUUID) of a partition via blkid, or None. distinct
/// from the fs UUID: efibootmgr device paths reference the partition by PARTUUID.
fn blkid_partuuid(dev: &str) -> Option<String> {
    blkid_value(dev, "PARTUUID")
}

/// a single blkid field (`-s <field> -o value`) of a device, or None.
fn blkid_value(dev: &str, field: &str) -> Option<String> {
    let out = Command::new("blkid")
        .args(["-s", field, "-o", "value", dev])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// the first entry in `dir` whose name starts with `prefix`, as a full path.
fn first_glob(dir: &str, prefix: &str) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path().to_string_lossy().to_string())
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// a fake /sys/class/block: `stack` lists a partition's holders bottom up, as
    /// (dm name, dm uuid). a device's dm/ dir holds name + uuid; its holders/ dir
    /// links each holder by kernel name.
    fn fake_sys(test: &str, part: &str, stack: &[(&str, &str)]) -> std::path::PathBuf {
        let sys = std::env::temp_dir().join(format!("raiden-sys-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&sys);
        let mut below = part.to_string();
        fs::create_dir_all(sys.join(part).join("holders")).unwrap();
        for (i, (name, uuid)) in stack.iter().enumerate() {
            let kernel = format!("dm-{i}");
            fs::create_dir_all(sys.join(&kernel).join("dm")).unwrap();
            fs::create_dir_all(sys.join(&kernel).join("holders")).unwrap();
            fs::write(sys.join(&kernel).join("dm/name"), format!("{name}\n")).unwrap();
            fs::write(sys.join(&kernel).join("dm/uuid"), format!("{uuid}\n")).unwrap();
            symlink(
                format!("../../{kernel}"),
                sys.join(&below).join("holders").join(&kernel),
            )
            .unwrap();
            below = kernel;
        }
        sys
    }

    const DIF_UUID: &str = "CRYPT-SUBDEV-44e87242c7034ff68e2c993529782a72-nvme0n1p3_crypt_dif";
    const CRYPT_UUID: &str = "CRYPT-LUKS2-44e87242c7034ff68e2c993529782a72-nvme0n1p3_crypt";

    fn renames(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn aead_rename_moves_the_crypt_layer_and_its_integrity_layer() {
        let sys = fake_sys(
            "aead",
            "nvme0n1p3",
            &[
                ("nvme0n1p3_crypt_dif", DIF_UUID),
                ("nvme0n1p3_crypt", CRYPT_UUID),
            ],
        );
        assert_eq!(
            member_renames(&sys, "nvme0n1p3", "raiden0_crypt"),
            renames(&[
                ("nvme0n1p3_crypt", "raiden0_crypt"),
                ("nvme0n1p3_crypt_dif", "raiden0_crypt_dif"),
            ])
        );
    }

    #[test]
    fn aead_mapping_already_named_is_left_alone() {
        // the first holder is X_dif, which must not read as a misnamed X.
        let sys = fake_sys(
            "named",
            "nvme0n1p3",
            &[
                ("nvme0n1p3_crypt_dif", DIF_UUID),
                ("nvme0n1p3_crypt", CRYPT_UUID),
            ],
        );
        assert_eq!(
            member_renames(&sys, "nvme0n1p3", "nvme0n1p3_crypt"),
            renames(&[])
        );
    }

    #[test]
    fn plain_crypt_rename_moves_the_single_layer() {
        let sys = fake_sys("plain", "vda3", &[("vda3_crypt", CRYPT_UUID)]);
        assert_eq!(
            member_renames(&sys, "vda3", "raiden0_crypt"),
            renames(&[("vda3_crypt", "raiden0_crypt")])
        );
    }

    #[test]
    fn a_holder_stack_without_a_crypt_device_is_not_renamed() {
        let sys = fake_sys(
            "nocrypt",
            "vda3",
            &[("vda3_int", "CRYPT-INTEGRITY-vda3_int")],
        );
        assert_eq!(member_renames(&sys, "vda3", "raiden0_crypt"), renames(&[]));
    }

    #[test]
    fn ok_passes_warn_and_fail_do_not() {
        assert!(Check::ok("x", "d").passed());
        assert!(!Check::warn("x", "d").passed());
        assert!(!Check::fail("x", "d").passed());
        // a fixed check passes too.
        let mut c = Check::warn("x", "d");
        c.status = FIXED;
        assert!(c.passed());
    }

    #[test]
    fn with_fix_attaches_a_fix() {
        let c = Check::warn("x", "missing").with_fix(Fix::EfiHook);
        assert_eq!(c.status, WARN);
        assert!(matches!(c.fix, Some(Fix::EfiHook)));
    }

    #[test]
    fn hook_path_and_efi_hook_path_use_the_constants() {
        assert_eq!(
            hook_path("postinst.d"),
            format!("/etc/kernel/postinst.d/{}", stack::BOOT_MIRROR_HOOK_NAME)
        );
        assert_eq!(
            efi_hook_path(),
            format!("/etc/grub.d/{}", stack::EFI_MIRROR_HOOK_NAME)
        );
    }

    // a hook is only ok when it is present, executable, AND its content matches
    // this raiden's canonical text. missing, non-executable, and stale-content all
    // warn with the same re-install fix -- a stale hook (e.g. the old boot hook that
    // forwarded run-parts' args) silently passes presence/exec checks yet still
    // breaks upgrades, so doctor must notice the content drift too.
    #[test]
    fn executable_hook_warns_unless_present_executable_and_current() {
        let expected = "#!/bin/sh\ncanonical\n";
        let dir = std::env::temp_dir().join(format!("raiden-hook-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let exec_write = |name: &str, body: &str| {
            let p = dir.join(name);
            fs::write(&p, body).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            p
        };

        // missing -> warn + fix.
        let missing = dir.join("missing");
        let c = check_executable_hook(
            "esp hook",
            missing.to_str().unwrap(),
            expected,
            Fix::EfiHook,
        );
        assert_eq!(c.status, WARN);
        assert!(c.fix.is_some());

        // present but not executable -> warn + fix (run-parts would skip it).
        let unmode = dir.join("unmode");
        fs::write(&unmode, expected).unwrap();
        let c = check_executable_hook("esp hook", unmode.to_str().unwrap(), expected, Fix::EfiHook);
        assert_eq!(c.status, WARN);
        assert!(c.fix.is_some());

        // present + executable but STALE content -> warn + fix (the regression).
        let stale = exec_write("stale", "#!/bin/sh\nexec raiden sync efi --yes \"$@\"\n");
        let c = check_executable_hook("esp hook", stale.to_str().unwrap(), expected, Fix::EfiHook);
        assert_eq!(c.status, WARN);
        assert!(c.fix.is_some());

        // present, executable, content matches the canonical text -> ok, no fix.
        let exec = exec_write("exec", expected);
        let c = check_executable_hook("esp hook", exec.to_str().unwrap(), expected, Fix::EfiHook);
        assert_eq!(c.status, OK);
        assert!(c.fix.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_state_extracts_the_md_state_line() {
        assert_eq!(
            first_state("foo\n       State : active\nbar"),
            "State : active"
        );
        assert_eq!(first_state("no state here"), "state unknown");
    }

    #[test]
    fn uuid_divergences_flags_only_the_odd_ones_out() {
        let shared = "1111-1111";
        let all_match = vec![
            ("/dev/vda1".into(), shared.into()),
            ("/dev/vdb1".into(), shared.into()),
        ];
        assert!(uuid_divergences(&all_match, shared).is_empty());

        let one_off = vec![
            ("/dev/vda1".into(), shared.into()),
            ("/dev/vdb1".into(), "2222-2222".into()),
        ];
        let d = uuid_divergences(&one_off, shared);
        assert_eq!(d, vec!["/dev/vdb1=2222-2222".to_string()]);
    }

    #[test]
    fn uuid_set_result_warns_with_a_fix_only_when_a_member_diverges() {
        let shared = "1111-1111";
        let pair = |a: &str, b: &str| {
            vec![
                ("/dev/vda1".to_string(), a.to_string()),
                ("/dev/vdb1".to_string(), b.to_string()),
            ]
        };

        // fewer than two present: nothing to compare, ok, no fix.
        let c = uuid_set_result("esp uuid", UuidKind::Esp, &[], Some(shared));
        assert!(c.passed() && c.fix.is_none());

        // all share the expected uuid: ok, no fix.
        let c = uuid_set_result(
            "esp uuid",
            UuidKind::Esp,
            &pair(shared, shared),
            Some(shared),
        );
        assert!(c.passed() && c.fix.is_none());

        // one diverges: warn, with the re-stamp fix attached.
        let c = uuid_set_result(
            "esp uuid",
            UuidKind::Esp,
            &pair(shared, "2222-2222"),
            Some(shared),
        );
        assert_eq!(c.status, WARN);
        assert!(matches!(c.fix, Some(Fix::RestampUuid(UuidKind::Esp))));
    }

    #[test]
    fn fix_commands_preview_the_exact_operations() {
        let layout = layout_with(&["vda", "vdb"]);
        // hooks preview a single file write; sync previews the sync command. the
        // re-stamp's commands are runtime-resolved (covered by the vm scenario).
        assert_eq!(
            fix_commands(&Fix::BootHook("postinst.d"), &layout),
            vec![format!(
                "write /etc/kernel/postinst.d/{} (mode 0755)",
                stack::BOOT_MIRROR_HOOK_NAME
            )]
        );
        assert_eq!(
            fix_commands(&Fix::SyncEfi, &layout),
            vec!["raiden sync efi --yes".to_string()]
        );
        // the recover-bundle fix INSTALLS the raiden initramfs hook (no stock hook
        // bakes raiden) and then rebuilds -- a plain rebuild alone cannot add it. the
        // rebuild is the same -k all update install runs (shared update_initramfs_u).
        let recover = fix_commands(&Fix::RecoverBundle, &layout);
        assert!(recover[0].contains(stack::RAIDEN_RECOVERY_HOOK));
        assert!(recover.iter().any(|c| c == "update-initramfs -u -k all"));
    }

    fn layout_with(members: &[&str]) -> Layout {
        let mut c = Config::default();
        c.disks.members = members.iter().map(|s| s.to_string()).collect();
        Layout::derive(&c)
    }

    #[test]
    fn parse_boot_entries_skips_headers_and_lowercases() {
        let out = "BootCurrent: 0003\nTimeout: 1 seconds\nBootOrder: 0003,0001\n\
            Boot0000* debian-nvme3n1\tHD(1,GPT,3C491BB4-6487-49FF-BA29-D7120BE8D80C,0x800)/\\EFI\\DEBIAN\\SHIMX64.EFI\n\
            Boot0009* UEFI OS\tHD(1,GPT,70f606da-f086-41a3-8d52-a767a5f3304a,0x800)/\\EFI\\BOOT\\BOOTX64.EFI\n";
        let entries = parse_boot_entries(out);
        // the three header lines are skipped; the two BootNNNN lines parse.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].num, "0000");
        // lowercased, so a member partuuid + the debian shim path both match.
        assert!(entries[0]
            .desc
            .contains("3c491bb4-6487-49ff-ba29-d7120be8d80c"));
        assert!(entries[0].desc.contains(r"\efi\debian\shimx64.efi"));
        // the removable fallback is a debian-free path, so the fix leaves it alone.
        assert!(!entries[1].desc.contains(r"\efi\debian\"));
    }

    #[test]
    fn debconf_mismatches_flags_only_wrong_values() {
        // debconf-show format: "* key: value" (asked) or "  key: value".
        let good = "* grub2/force_efi_extra_removable: true\n  grub2/update_nvram: false\n";
        assert!(debconf_mismatches(good).is_empty());

        let bad = "* grub2/force_efi_extra_removable: true\n  grub2/update_nvram: true\n";
        let m = debconf_mismatches(bad);
        assert_eq!(m.len(), 1);
        assert!(m[0].contains("update_nvram=true (want false)"));

        // an unset key is a mismatch too (so a fresh/reconfigured db is repaired).
        let m = debconf_mismatches("");
        assert_eq!(m.len(), efi::GRUB_DEBCONF.len());
        assert!(m.iter().any(|s| s.contains("unset")));
    }

    #[test]
    fn missing_initrd_binaries_matches_by_basename() {
        let listing = "usr/sbin/cryptsetup\nusr/bin/keyctl\n\
            usr/lib/cryptsetup/scripts/decrypt_keyctl\nusr/bin/btrfs\n";
        // present (incl. keyctl vs decrypt_keyctl distinguished by basename).
        assert!(missing_initrd_binaries(
            listing,
            &["cryptsetup", "keyctl", "decrypt_keyctl", "btrfs"]
        )
        .is_empty());
        // a substring of a present path is NOT a false positive, and a real gap is flagged.
        let m = missing_initrd_binaries(listing, &["keyctl", "mdadm"]);
        assert_eq!(m, vec!["mdadm".to_string()]);
    }

    const CRYPTTAB: &str = "# <target> <source> <key> <options>\n\
        nvme2n1p3_crypt UUID=fca35681-aaaa none luks,initramfs\n\
        nvme3n1p3_crypt UUID=d9620e23-bbbb none luks,initramfs\n";

    #[test]
    fn crypttab_check_matches_by_luks_uuid_not_by_kernel_name() {
        // a correct crypttab is ok whatever node the disk has now: the entry is
        // found by the member's luks uuid, then its name is compared.
        let c = crypttab_check("nvme2n1p3_crypt", Some("fca35681-aaaa"), CRYPTTAB);
        assert_eq!(c.status, OK);
        // the right disk under the wrong mapper name is repairable drift.
        let c = crypttab_check("raiden0_crypt", Some("fca35681-aaaa"), CRYPTTAB);
        assert_eq!(c.status, WARN);
        assert!(matches!(c.fix, Some(Fix::Crypttab)));
        // a disk crypttab does not know about at all cannot unlock at boot.
        let c = crypttab_check("nvme4n1p3_crypt", Some("0000-cccc"), CRYPTTAB);
        assert_eq!(c.status, FAIL);
        // an unreadable disk is reported, not "fixed" by rewriting crypttab.
        let c = crypttab_check("nvme2n1p3_crypt", None, CRYPTTAB);
        assert_eq!(c.status, WARN);
        assert!(c.fix.is_none());
    }

    #[test]
    fn crypttab_uuid_of_reads_the_named_entry() {
        assert_eq!(
            crypttab_uuid_of(CRYPTTAB, "nvme3n1p3_crypt").as_deref(),
            Some("d9620e23-bbbb")
        );
        assert_eq!(crypttab_uuid_of(CRYPTTAB, "nvme9n1p3_crypt"), None);
    }

    #[test]
    fn missing_crypttab_names_flags_members_the_initrd_cannot_unlock() {
        let names = ["nvme2n1p3_crypt".to_string(), "nvme4n1p3_crypt".to_string()];
        assert_eq!(
            missing_crypttab_names(CRYPTTAB, &names),
            vec!["nvme4n1p3_crypt"]
        );
    }
}
