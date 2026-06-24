mod bad_files;
mod benchmark;
mod checkpoint;
mod cli;
mod config;
mod init;
mod layout;
mod ops;
mod pipeline;
mod prompt;
mod stack;
mod state;
mod step;

use std::io::IsTerminal;
use std::path::Path;

use anyhow::{bail, Result};
use clap::Parser;

use checkpoint::Checkpoint;
use cli::{BenchmarkArgs, Cli, Command, ConfigCmd, Global, InstallArgs, StatusArgs};
use config::{Config, Family, Overrides};
use layout::Layout;
use state::State;
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
        Command::Status(args) => cmd_status(g, &overrides, args),
        Command::Scrub { wait: _ } => run_op(g, &overrides, "scrub", false, |c, l, s| {
            Ok(ops::scrub(c, l, s))
        }),
        Command::Rescue => run_op(g, &overrides, "rescue", false, |c, l, s| {
            Ok(ops::rescue(c, l, s))
        }),
        Command::Mount(args) => {
            // full mount lands at /mnt (mount_root is /mnt-relative); --boot honors
            // --at so the running system can be fixed in place with `--at /`.
            let boot = args.boot;
            let at = if boot {
                args.at.clone()
            } else {
                "/mnt".to_string()
            };
            run_op(g, &overrides, "mount", false, move |c, l, s| {
                Ok(ops::mount(c, l, s, boot, &at))
            })
        }
        Command::Close => run_op(g, &overrides, "close", false, |c, l, s| {
            Ok(ops::close(c, l, s))
        }),
        Command::Replace {
            disks,
            esp,
            boot,
            root,
        } => {
            let d = config::split_disks(disks);
            let parts = ops::ReplaceParts::from_flags(*esp, *boot, *root);
            run_op(g, &overrides, "replace", true, move |c, l, s| {
                ops::replace(c, l, s, &d, &parts)
            })
        }
        Command::Remove { disks } => {
            let d = config::split_disks(disks);
            run_op(g, &overrides, "remove", true, move |c, l, s| {
                ops::remove(c, l, s, &d)
            })
        }
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
    if State::exists() {
        let mut c = State::load()?.config;
        c.merge(overrides);
        Ok(c)
    } else {
        Config::load(&g.config, overrides)
    }
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
    run_plan("install", &plan, &cfg, g)?;
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

fn run_op<F>(g: &Global, overrides: &Overrides, op: &str, destructive: bool, build: F) -> Result<()>
where
    F: FnOnce(&Config, &Layout, &dyn stack::Stack) -> Result<Vec<Phase>>,
{
    let cfg = resolve_config(g, overrides)?;
    cfg.validate()?;
    let layout = Layout::derive(&cfg);
    let stack = stack::select(&cfg.raid.stack)?;
    let plan = build(&cfg, &layout, stack.as_ref())?;

    if g.dry_run {
        step::print_plan(&plan);
        return Ok(());
    }
    if destructive && !g.yes {
        guard(op)?;
    }
    run_plan(op, &plan, &cfg, g)?;
    if op == "replace" && State::exists() {
        let _ = State::from_config(&cfg).save();
    }
    Ok(())
}

/// execute a plan with checkpointing and resume.
fn run_plan(op: &str, plan: &[Phase], cfg: &Config, g: &Global) -> Result<()> {
    let path = Path::new(checkpoint::PATH);
    let hash = checkpoint::config_hash(cfg);
    let start = resume_start(op, &hash, g.resume, path)?;
    let pw = if step::needs_password(plan) {
        Some(obtain_password(op, g)?)
    } else {
        None
    };
    let op_owned = op.to_string();
    let result = step::execute_plan(
        plan,
        pw.as_deref(),
        start,
        g.verbose,
        |phase, step, name| {
            Checkpoint {
                operation: op_owned.clone(),
                config_hash: hash.clone(),
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
fn resume_start(op: &str, hash: &str, resume: bool, path: &Path) -> Result<(usize, usize)> {
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
    prompt::read_password(op == "install")
}

fn guard(op: &str) -> Result<()> {
    eprintln!("WARNING: {op} is destructive and can erase data on the configured disks.");
    if !prompt::confirm("continue?")? {
        bail!("aborted");
    }
    Ok(())
}

fn cmd_status(g: &Global, overrides: &Overrides, args: &StatusArgs) -> Result<()> {
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
    println!("manifest:     {} (written at install)", state::DEFAULT_PATH);
    Ok(())
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
