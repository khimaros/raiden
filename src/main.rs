mod bad_files;
mod benchmark;
mod checkpoint;
mod cli;
mod config;
mod doctor;
mod efi;
mod init;
mod layout;
mod manifest;
mod ops;
mod pipeline;
mod prompt;
mod recover;
mod stack;
mod step;
mod sync;

use std::io::IsTerminal;
use std::path::Path;

use anyhow::{bail, Result};
use clap::Parser;

use checkpoint::Checkpoint;
use cli::{
    BenchmarkArgs, Cli, Command, ConfigCmd, DoctorArgs, Global, InstallArgs, RecoverArgs,
    StatusArgs, SyncArgs, SyncTarget,
};
use config::{Config, Family, Overrides};
use layout::Layout;
use manifest::Manifest;
use step::Phase;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let g = &cli.global;
    let overrides = overrides_from(g);

    match &cli.command {
        Command::Init(args) => init::run(g, &overrides, args),
        Command::Install(args) => cmd_install(g, &overrides, args),
        Command::Config(ConfigCmd::Validate) => {
            Config::load(&g.config, &overrides)?.validate()?;
            println!("config ok");
            Ok(())
        }
        Command::Config(ConfigCmd::Show) => cmd_config_show(g, &overrides),
        Command::Benchmark(args) => cmd_benchmark(g, &overrides, args),
        Command::Devices => cmd_devices(g, &overrides),
        Command::Doctor(args) => cmd_doctor(g, &overrides, args),
        Command::Status(args) => cmd_status(g, &overrides, args),
        Command::Scrub { wait: _ } => {
            require_installed(g)?;
            let (cfg, stack) = resolve_op(g, &overrides)?;
            run_op(g, &cfg, stack.as_ref(), "scrub", false, "", |c, l, s| {
                Ok(ops::scrub(c, l, s))
            })
        }
        Command::Rescue => {
            let (cfg, stack) = resolve_op(g, &overrides)?;
            run_op(g, &cfg, stack.as_ref(), "rescue", false, "", |c, l, s| {
                Ok(ops::rescue(c, l, s))
            })
        }
        Command::Recover(args) => cmd_recover(g, &overrides, args),
        Command::Mount(args) => {
            // full mount lands at /mnt (mount_root is /mnt-relative); --boot honors
            // --at so the running system can be fixed in place with `--at /`.
            let boot = args.boot;
            let at = if boot {
                args.at.clone()
            } else {
                "/mnt".to_string()
            };
            let (cfg, stack) = resolve_op(g, &overrides)?;
            run_op(
                g,
                &cfg,
                stack.as_ref(),
                "mount",
                false,
                "",
                move |c, l, s| Ok(ops::mount(c, l, s, boot, &at)),
            )
        }
        Command::Close => {
            require_installed(g)?;
            let (cfg, stack) = resolve_op(g, &overrides)?;
            run_op(g, &cfg, stack.as_ref(), "close", false, "", |c, l, s| {
                Ok(ops::close(c, l, s))
            })
        }
        Command::Replace {
            disks,
            with,
            esp,
            boot,
            root,
        } => cmd_replace(g, &overrides, disks, with, *esp, *boot, *root),
        Command::Remove { disks } => {
            require_installed(g)?;
            let d = config::split_disks(disks);
            let scope = format!("disks={}", d.join(","));
            let (cfg, stack) = resolve_op(g, &overrides)?;
            run_op(
                g,
                &cfg,
                stack.as_ref(),
                "remove",
                true,
                &scope,
                move |c, l, s| ops::remove(c, l, s, &d),
            )
        }
        Command::Sync(args) => cmd_sync(g, &overrides, args),
    }
}

fn overrides_from(g: &Global) -> Overrides {
    Overrides {
        stack: g.stack.clone(),
        level: g.level.clone(),
        disks: g.members.as_deref().map(config::split_disks),
        release: g.release.clone(),
    }
}

/// operations resolve config from the install manifest when present, falling
/// back to the config file (eg. a fresh livecd rescue before any install).
fn resolve_config(g: &Global, overrides: &Overrides) -> Result<Config> {
    if Manifest::exists() {
        let mut c = Manifest::load()?.config;
        c.merge(overrides);
        Ok(c)
    } else {
        Config::load(&g.config, overrides)
    }
}

/// post-install ops (doctor, sync, status, scrub, replace, remove, close) only make
/// sense on an installed raiden system. require the manifest before doing anything,
/// so a mutation can never run against a non-raiden host that merely has a config
/// file in cwd (eg. `doctor --fix` writing hooks to a dev box's /etc/kernel). a
/// `--dry-run` only previews a plan and is exempt; rescue/mount/recover run from a
/// livecd or the initramfs (manifest baked in) and do not call this.
fn require_installed(g: &Global) -> Result<()> {
    if !g.dry_run && !Manifest::exists() {
        bail!(
            "no raiden install manifest found ({} or {}); this command only runs on \
             an installed raiden system. use --dry-run to preview a plan.",
            manifest::BOOT_MIRROR_PATH,
            manifest::DEFAULT_PATH
        );
    }
    Ok(())
}

fn cmd_install(g: &Global, overrides: &Overrides, args: &InstallArgs) -> Result<()> {
    if args.list_phases {
        for p in pipeline::PHASES {
            println!("{p}");
        }
        return Ok(());
    }
    if g.resume && (args.from.is_some() || args.only.is_some()) {
        bail!("--resume cannot be combined with --from/--only");
    }
    // with a config file, install from it; without one, generate a config for this
    // machine (discover disks + stack-correct crypt) so `raiden install` is a
    // single command from a bare live environment.
    let cfg = if g.config.exists() {
        Config::load(&g.config, overrides)?
    } else {
        let interactive = !g.yes && std::io::stdin().is_terminal();
        init::generate(overrides, None, interactive)?
    };
    cfg.validate()?;
    let layout = Layout::derive(&cfg);
    let stack = stack::select(&cfg.raid.stack)?;
    // an unattended run (password provided non-interactively) also sets the root
    // password non-interactively, to the disk password.
    let auto_root = g.password_file.is_some()
        || std::env::var("RAIDEN_PASSWORD")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
    // the running binary is copied into the target so post-install ops work after
    // reboot; if its path cannot be resolved, the copy step is simply omitted.
    let exe = std::env::current_exe().ok();
    let raiden_bin = exe.as_deref().and_then(Path::to_str);
    let plan = filter_phases(
        pipeline::install(&cfg, &layout, stack.as_ref(), auto_root, raiden_bin),
        &args.from,
        &args.only,
    )?;

    if g.dry_run {
        println!("# install plan for stack {}\n", stack.id());
        step::print_plan(&plan);
        return Ok(());
    }
    // install erases the member disks (R14); confirm before the destructive run
    // unless --yes (the harness and unattended one-liners pass it). a --resume
    // continues an already-confirmed run, so it does not re-confirm.
    if !g.yes && !g.resume {
        install_guard(&cfg)?;
    }
    // the manifest is written into the target by the pipeline's finish phase
    // (while /mnt is still mounted), so it persists into the installed system.
    run_plan("install", &plan, &cfg, g, "")?;
    Ok(())
}

/// confirm a real install before it erases the configured member disks, naming
/// them so an auto-discovered or wrong-disk selection is caught before the wipe.
fn install_guard(cfg: &Config) -> Result<()> {
    eprintln!(
        "WARNING: install will ERASE all data on: {}",
        cfg.disks.members.join(", ")
    );
    eprintln!(
        "  stack {} level {} ({})",
        cfg.raid.stack, cfg.raid.level, cfg.crypt.cipher
    );
    if !prompt::confirm("continue?")? {
        bail!("aborted");
    }
    Ok(())
}

/// `raiden replace`: rebuild named members in place, or --with a physical disk
/// swap. --disks=a,b --with=c,d swaps a->c, b->d by position, mutating the
/// manifest: the new disks adopt the old disks' identifying esp/luks uuids (so
/// fstab/crypttab stay valid) and are provisioned + re-added from the survivors.
/// without --with, --disks are rebuilt in place (backward compatible). the old
/// disks in a swap are detached best-effort (they may already be gone) and are
/// never wiped; the new disks are wiped + provisioned.
fn cmd_replace(
    g: &Global,
    overrides: &Overrides,
    disks: &str,
    with: &Option<String>,
    esp: bool,
    boot: bool,
    root: bool,
) -> Result<()> {
    require_installed(g)?;
    let mut cfg = resolve_config(g, overrides)?;
    cfg.validate()?;
    let stack = stack::select(&cfg.raid.stack)?;
    let parts = ops::ReplaceParts::from_flags(esp, boot, root);
    let d = config::split_disks(disks);

    // --with: a physical swap. build the (old, new) pairs and mutate the config's
    // members so the plan (and the saved manifest) reference the new disks.
    let swap: Option<Vec<(String, String)>> = match with {
        Some(w) => {
            let new_disks = config::split_disks(w);
            if new_disks.len() != d.len() {
                bail!(
                    "--disks has {} entr{{y,ies}} but --with has {}; they must pair 1:1",
                    d.len(),
                    new_disks.len()
                );
            }
            for old in &d {
                if !cfg.disks.members.contains(old) {
                    bail!("{old:?} is not a configured member disk");
                }
            }
            for new in &new_disks {
                if cfg.disks.members.contains(new) {
                    bail!("{new:?} is already a member; --with names new disks");
                }
                if d.contains(new) {
                    bail!("{new:?} appears in both --disks and --with");
                }
            }
            let pairs: Vec<(String, String)> =
                d.iter().cloned().zip(new_disks.iter().cloned()).collect();
            // mutate members: each old disk -> its paired new disk, by position.
            for m in cfg.disks.members.iter_mut() {
                if let Some((_, new)) = pairs.iter().find(|(old, _)| old == m) {
                    *m = new.clone();
                }
            }
            Some(pairs)
        }
        None => None,
    };

    // the disks to provision: the new disks (swap) or --disks (in-place).
    let provision_disks: Vec<String> = swap
        .as_ref()
        .map(|p| p.iter().map(|(_, n)| n.clone()).collect())
        .unwrap_or_else(|| d.clone());

    // resume must match the exact target; capture it so changed --disks/--with/
    // --parts cannot reuse this run's checkpoint cursor.
    let scope = format!(
        "disks={} with={} esp={} boot={} root={}",
        d.join(","),
        with.as_deref().unwrap_or(""),
        parts.esp,
        parts.boot,
        parts.root
    );
    run_op(
        g,
        &cfg,
        stack.as_ref(),
        "replace",
        true,
        &scope,
        move |c, l, s| ops::replace(c, l, s, &provision_disks, &parts, swap.as_deref()),
    )
}

fn run_op<F>(
    g: &Global,
    cfg: &Config,
    stack: &dyn stack::Stack,
    op: &str,
    destructive: bool,
    scope: &str,
    build: F,
) -> Result<()>
where
    F: FnOnce(&Config, &Layout, &dyn stack::Stack) -> Result<Vec<Phase>>,
{
    let layout = Layout::derive(cfg);
    let plan = build(cfg, &layout, stack)?;

    if g.dry_run {
        step::print_plan(&plan);
        return Ok(());
    }
    if destructive && !g.yes {
        guard(op)?;
    }
    run_plan(op, &plan, cfg, g, scope)?;
    if op == "replace" && Manifest::exists() {
        let _ = Manifest::from_config(cfg).save();
    }
    Ok(())
}

/// resolve config + stack for a post-install op (shared by the run_op callers).
fn resolve_op(g: &Global, overrides: &Overrides) -> Result<(Config, Box<dyn stack::Stack>)> {
    let cfg = resolve_config(g, overrides)?;
    cfg.validate()?;
    let stack = stack::select(&cfg.raid.stack)?;
    Ok((cfg, stack))
}

/// execute a plan with checkpointing and resume. `scope` captures the op's
/// destructive cli args (eg. replace's disks/layers) so resume refuses a changed
/// target (which would reuse this cursor against a different plan).
fn run_plan(op: &str, plan: &[Phase], cfg: &Config, g: &Global, scope: &str) -> Result<()> {
    let path = checkpoint::path();
    let path = path.as_path();
    let hash = checkpoint::config_hash(cfg);
    let start = resume_start(op, &hash, scope, g.resume, path)?;
    let pw = if step::needs_password(plan) {
        Some(obtain_password(op, g)?)
    } else {
        None
    };
    let op_owned = op.to_string();
    let scope_owned = scope.to_string();
    let result = step::execute_plan(
        plan,
        pw.as_deref(),
        start,
        g.verbose,
        |phase, step, name| {
            Checkpoint {
                operation: op_owned.clone(),
                config_hash: hash.clone(),
                scope: scope_owned.clone(),
                phase,
                step,
                phase_name: name.to_string(),
            }
            .save(path)
        },
    );
    match result {
        Ok(()) => {
            Checkpoint::clear(path);
            eprintln!("\n{op} complete");
            Ok(())
        }
        Err(e) => {
            eprintln!("\n{op} failed: {e}");
            eprintln!("fix the problem, then resume with: raiden {op} --resume");
            Err(e)
        }
    }
}

/// resolve the (phase, step) cursor to start from. a fresh run clears any stale
/// checkpoint; --resume validates the checkpoint matches this op and config.
fn resume_start(
    op: &str,
    hash: &str,
    scope: &str,
    resume: bool,
    path: &Path,
) -> Result<(usize, usize)> {
    if !resume {
        Checkpoint::clear(path);
        return Ok((0, 0));
    }
    let cp = Checkpoint::load(path)
        .ok_or_else(|| anyhow::anyhow!("no checkpoint to resume; run without --resume"))?;
    if cp.operation != op {
        bail!("checkpoint is for {:?}, not {op:?}", cp.operation);
    }
    if cp.config_hash != hash {
        bail!("config changed since the checkpoint; cannot resume safely");
    }
    if cp.scope != scope {
        // the cursor is meaningless against a different plan; a changed target
        // would skip the wrong steps. start fresh instead.
        bail!(
            "the {op} target changed since the checkpoint (was [{}], now [{}]); \
             run without --resume to start fresh, or --resume with the original arguments",
            cp.scope,
            scope
        );
    }
    eprintln!(
        "resuming {op} after phase {:?}, step {} (already-applied steps are skipped)",
        cp.phase_name, cp.step
    );
    Ok((cp.phase, cp.step + 1))
}

/// resolve the encryption password without prompting when a file or the
/// RAIDEN_PASSWORD env var is provided (for unattended runs and the vm harness).
fn obtain_password(op: &str, g: &Global) -> Result<String> {
    if let Some(path) = &g.password_file {
        let pw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading password file {}: {e}", path.display()))?;
        return Ok(pw.trim_end_matches(['\r', '\n']).to_string());
    }
    if let Ok(pw) = std::env::var("RAIDEN_PASSWORD") {
        if !pw.is_empty() {
            return Ok(pw);
        }
    }
    // install and replace luks-FORMAT a disk with this password, so a typo would
    // silently create a member with a mismatched password (failing to unlock at
    // boot) -- verify those by prompting twice. open-only ops (rescue, mount) do
    // not: a wrong password just fails to unlock, harmlessly and immediately.
    prompt::read_password(matches!(op, "install" | "replace"))
}

fn guard(op: &str) -> Result<()> {
    eprintln!("WARNING: {op} is destructive and can erase data on the configured disks.");
    if !prompt::confirm("continue?")? {
        bail!("aborted");
    }
    Ok(())
}

fn cmd_status(g: &Global, overrides: &Overrides, args: &StatusArgs) -> Result<()> {
    require_installed(g)?;
    let cfg = resolve_config(g, overrides)?;
    cfg.validate()?;
    let is_md = matches!(cfg.family()?, Family::Md);
    // --bad-files narrows status to just the affected-file listing. it is the
    // read-error mapping alone (md stacks only), with none of the array detail.
    if args.bad_files {
        if is_md {
            bad_files::report();
        } else {
            println!("bad-files mapping applies only to md stacks");
        }
        return Ok(());
    }
    let layout = Layout::derive(&cfg);
    let stack = stack::select(&cfg.raid.stack)?;
    let plan = vec![Phase::new(
        "status",
        ops::status_steps(&cfg, &layout, stack.as_ref()),
    )];
    if g.dry_run {
        step::print_plan(&plan);
        return Ok(());
    }
    // status is read-only; its steps are best-effort and write no checkpoint.
    step::execute_plan(&plan, None, (0, 0), g.verbose, |_, _, _| Ok(()))?;
    if is_md {
        bad_files::report();
    }
    Ok(())
}

/// run (or, with --dry-run, print) the fsync-bound fileio benchmark on the array.
/// resolves the benchmark sizing from the manifest/config, overlaid by flags.
fn cmd_benchmark(g: &Global, overrides: &Overrides, args: &BenchmarkArgs) -> Result<()> {
    let cfg = resolve_config(g, overrides)?;
    cfg.validate()?;
    let mut b = benchmark::Bench::from_cfg(&cfg.benchmark);
    if let Some(v) = &args.size {
        b.size = v.clone();
    }
    if let Some(v) = args.passes {
        b.passes = v;
    }
    if let Some(v) = args.rndwr_events {
        b.rndwr_events = v;
    }
    if let Some(v) = args.seqwr_events {
        b.seqwr_events = v;
    }
    if g.dry_run {
        step::print_plan(&benchmark::plan(&b));
        return Ok(());
    }
    benchmark::run(&b, &args.format, g.verbose)
}

fn cmd_config_show(g: &Global, overrides: &Overrides) -> Result<()> {
    let cfg = Config::load(&g.config, overrides)?;
    cfg.validate()?;
    println!("# resolved config\n{}", toml::to_string_pretty(&cfg)?);
    let layout = Layout::derive(&cfg);
    println!("# derived layout");
    println!("members:      {}", layout.members.join(", "));
    println!("esp devices:  {}", layout.esp_devices().join(", "));
    println!("boot devices: {}", layout.boot_devices().join(", "));
    println!("root devices: {}", layout.root_devices().join(", "));
    if layout.boot_raid() {
        println!("boot mode:    md raid1 ({})", layout::BOOT_MD_DEVICE);
    } else {
        println!(
            "boot mode:    independent (live /boot by shared uuid; mirrors synced transiently)"
        );
    }
    println!(
        "esp mount:    {} (primary esp; other esps synced transiently)",
        layout::ESP_MOUNT
    );
    println!("crypt names:  {}", layout.crypt_names().join(", "));
    println!(
        "manifest:     {} (canonical; mirrored to {})",
        manifest::BOOT_MIRROR_PATH,
        manifest::DEFAULT_PATH
    );
    Ok(())
}

fn cmd_sync(g: &Global, overrides: &Overrides, args: &SyncArgs) -> Result<()> {
    // sync runs directly (not as a checkpointed plan): the source and mirror set
    // are resolved at run time, and the kernel/grub hooks call it from a wrapper.
    require_installed(g)?;
    let cfg = resolve_config(g, overrides)?;
    cfg.validate()?;
    let layout = Layout::derive(&cfg);
    let force = match &args.target {
        SyncTarget::Boot(a) => a.force,
        SyncTarget::Efi(a) => a.force,
    };
    sync::run(&args.target, &layout, g.yes, force, g.verbose, g.dry_run)
}

fn cmd_doctor(g: &Global, overrides: &Overrides, args: &DoctorArgs) -> Result<()> {
    // doctor reports installed-system health, resolving config from the manifest
    // like the other post-install ops. --fix repairs the auto-fixable checks,
    // prompting before each one unless --yes; --fix --dry-run previews the exact
    // repairs (commands + devices) without touching anything. no checkpoint.
    require_installed(g)?;
    let cfg = resolve_config(g, overrides)?;
    cfg.validate()?;
    let layout = Layout::derive(&cfg);
    doctor::run(&cfg, &layout, g.verbose, args.fix, g.yes, g.dry_run)
}

/// `raiden recover`: bring a degraded root online from the initramfs (or a livecd)
/// so the boot can continue. resolves config from the manifest (baked into the
/// initrd) or a config file; like rescue/mount it is a recovery op and does not
/// require an install manifest up front. crypt members are already open by the
/// initramfs, so this needs no password.
fn cmd_recover(g: &Global, overrides: &Overrides, args: &RecoverArgs) -> Result<()> {
    let (cfg, stack) = resolve_op(g, overrides)?;
    let layout = Layout::derive(&cfg);
    recover::run(
        &cfg,
        &layout,
        stack.as_ref(),
        &args.at,
        g.yes,
        g.dry_run,
        g.verbose,
    )
}

fn cmd_devices(g: &Global, overrides: &Overrides) -> Result<()> {
    let cfg = resolve_config(g, overrides)?;
    cfg.validate()?;
    let layout = Layout::derive(&cfg);
    println!("configured member disks:");
    for d in &layout.members {
        println!(
            "  {d}: esp {}, boot {}, root {}",
            layout.part(d, 1),
            layout.part(d, 2),
            layout.part(d, 3)
        );
    }
    Ok(())
}

/// restrict the install pipeline to a single phase (--only) or a suffix (--from).
fn filter_phases(
    phases: Vec<Phase>,
    from: &Option<String>,
    only: &Option<String>,
) -> Result<Vec<Phase>> {
    if let Some(name) = only {
        let found: Vec<Phase> = phases.into_iter().filter(|p| &p.name == name).collect();
        if found.is_empty() {
            bail!("unknown phase {name:?}; see `raiden install --list-phases`");
        }
        return Ok(found);
    }
    if let Some(name) = from {
        let start = phases.iter().position(|p| &p.name == name).ok_or_else(|| {
            anyhow::anyhow!("unknown phase {name:?}; see `raiden install --list-phases`")
        })?;
        return Ok(phases.into_iter().skip(start).collect());
    }
    Ok(phases)
}

#[cfg(test)]
mod tests {
    use super::*;

    // resume must refuse a changed destructive scope (eg. different replace
    // --disks/--parts), since the checkpoint cursor is meaningless against a
    // different plan -- the bug that silently skipped the wrong replace steps.
    #[test]
    fn resume_refuses_a_changed_scope() {
        let path =
            std::env::temp_dir().join(format!("raiden-resume-scope-{}.toml", std::process::id()));
        let orig = "disks=nvme0n1,nvme1n1 esp=true boot=true root=true";
        Checkpoint {
            operation: "replace".into(),
            config_hash: "abc".into(),
            scope: orig.into(),
            phase: 2,
            step: 0,
            phase_name: "partition".into(),
        }
        .save(&path)
        .unwrap();

        // same op + config hash but a different target must not resume.
        assert!(resume_start(
            "replace",
            "abc",
            "disks=nvme1n1 esp=false boot=false root=true",
            true,
            &path
        )
        .is_err());
        // the exact original scope resumes from the next step.
        assert_eq!(
            resume_start("replace", "abc", orig, true, &path).unwrap(),
            (2, 1)
        );

        Checkpoint::clear(&path);
    }
}
