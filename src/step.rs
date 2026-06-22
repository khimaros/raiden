// a step is one action: run a command, write a file, or append to a file. a plan
// is an ordered list of phases, each a named group of steps. the runner prints
// the plan under --dry-run or executes it. the encryption password is fed to
// secret steps on stdin and is never printed or stored in a step.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

pub struct RunSpec {
    pub argv: Vec<String>,
    pub chroot: bool,
    pub secret_stdin: bool,
    // text prepended to the password on stdin (eg. "root:" for chpasswd).
    pub secret_prefix: String,
    pub best_effort: bool,
}

pub enum Action {
    Run(RunSpec),
    Write {
        path: String,
        content: String,
        mode: Option<u32>,
    },
    Append {
        path: String,
        content: String,
    },
}

pub struct Step {
    pub note: String,
    pub action: Action,
}

impl Step {
    pub fn run(note: impl Into<String>, argv: &[&str]) -> Self {
        Self::run_owned(note, argv.iter().map(|s| s.to_string()).collect())
    }

    pub fn run_owned(note: impl Into<String>, argv: Vec<String>) -> Self {
        Step {
            note: note.into(),
            action: Action::Run(RunSpec {
                argv,
                chroot: false,
                secret_stdin: false,
                secret_prefix: String::new(),
                best_effort: false,
            }),
        }
    }

    /// a command run through `sh -c`, for the few cases that need runtime values
    /// (eg. a uuid from blkid) or shell redirection.
    pub fn sh(note: impl Into<String>, script: impl Into<String>) -> Self {
        Self::run_owned(
            note,
            vec!["sh".to_string(), "-c".to_string(), script.into()],
        )
    }

    pub fn write(
        note: impl Into<String>,
        path: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Step {
            note: note.into(),
            action: Action::Write {
                path: path.into(),
                content: content.into(),
                mode: None,
            },
        }
    }

    pub fn write_mode(
        note: impl Into<String>,
        path: impl Into<String>,
        content: impl Into<String>,
        mode: u32,
    ) -> Self {
        Step {
            note: note.into(),
            action: Action::Write {
                path: path.into(),
                content: content.into(),
                mode: Some(mode),
            },
        }
    }

    pub fn append(
        note: impl Into<String>,
        path: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Step {
            note: note.into(),
            action: Action::Append {
                path: path.into(),
                content: content.into(),
            },
        }
    }

    pub fn chroot(mut self) -> Self {
        if let Action::Run(r) = &mut self.action {
            r.chroot = true;
        }
        self
    }

    pub fn secret(mut self) -> Self {
        if let Action::Run(r) = &mut self.action {
            r.secret_stdin = true;
        }
        self
    }

    /// feed `prefix` + the password on stdin (eg. "root:" so chpasswd sets the
    /// root account non-interactively).
    pub fn secret_prefixed(mut self, prefix: &str) -> Self {
        if let Action::Run(r) = &mut self.action {
            r.secret_stdin = true;
            r.secret_prefix = prefix.to_string();
        }
        self
    }

    pub fn best_effort(mut self) -> Self {
        if let Action::Run(r) = &mut self.action {
            r.best_effort = true;
        }
        self
    }

    fn is_secret(&self) -> bool {
        matches!(&self.action, Action::Run(r) if r.secret_stdin)
    }

    /// human-readable lines for --dry-run, with the password masked.
    fn describe(&self) -> Vec<String> {
        match &self.action {
            Action::Run(r) => {
                let mut cmd = String::new();
                if r.secret_stdin {
                    cmd.push_str(&format!("echo {}<password> | ", r.secret_prefix));
                }
                if r.chroot {
                    cmd.push_str("chroot /mnt ");
                }
                cmd.push_str(&r.argv.join(" "));
                if r.best_effort {
                    cmd.push_str("    # best-effort");
                }
                vec![cmd]
            }
            Action::Write {
                path,
                content,
                mode,
            } => {
                let m = mode.map(|m| format!(" (mode {m:04o})")).unwrap_or_default();
                let mut lines = vec![format!("write {path}{m}:")];
                lines.extend(content.lines().map(|l| format!("  | {l}")));
                lines
            }
            Action::Append { path, content } => content
                .trim_end_matches('\n')
                .lines()
                .map(|l| format!("append {path}: {l}"))
                .collect(),
        }
    }

    fn execute(&self, password: Option<&str>) -> Result<()> {
        match &self.action {
            Action::Run(r) => run_command(r, password),
            Action::Write {
                path,
                content,
                mode,
            } => {
                std::fs::write(path, content).with_context(|| format!("writing {path}"))?;
                if let Some(m) = mode {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(*m))
                        .with_context(|| format!("chmod {path}"))?;
                }
                Ok(())
            }
            Action::Append { path, content } => {
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .with_context(|| format!("opening {path} for append"))?;
                f.write_all(content.as_bytes())
                    .with_context(|| format!("appending to {path}"))?;
                Ok(())
            }
        }
    }
}

fn run_command(r: &RunSpec, password: Option<&str>) -> Result<()> {
    let mut cmd = if r.chroot {
        let mut c = Command::new("chroot");
        c.arg("/mnt");
        c.args(&r.argv);
        c
    } else {
        let mut c = Command::new(&r.argv[0]);
        c.args(&r.argv[1..]);
        c
    };

    if r.secret_stdin {
        let pw = password
            .ok_or_else(|| anyhow::anyhow!("a password is required but none was provided"))?;
        cmd.stdin(Stdio::piped());
        let mut child = cmd.spawn().context("spawning command")?;
        let data = format!("{}{}", r.secret_prefix, pw);
        child
            .stdin
            .take()
            .unwrap()
            .write_all(data.as_bytes())
            .context("writing password to stdin")?;
        check(child.wait()?.success(), r)
    } else {
        let status = cmd.status().context("running command")?;
        check(status.success(), r)
    }
}

fn check(ok: bool, r: &RunSpec) -> Result<()> {
    if ok || r.best_effort {
        Ok(())
    } else {
        bail!("command failed: {}", r.argv.join(" "));
    }
}

pub struct Phase {
    pub name: String,
    pub steps: Vec<Step>,
}

impl Phase {
    pub fn new(name: impl Into<String>, steps: Vec<Step>) -> Self {
        Self {
            name: name.into(),
            steps,
        }
    }
}

/// true if any step in the plan needs the encryption password.
pub fn needs_password(plan: &[Phase]) -> bool {
    plan.iter().any(|p| p.steps.iter().any(|s| s.is_secret()))
}

/// print every phase and its actions without executing anything.
pub fn print_plan(plan: &[Phase]) {
    for phase in plan {
        println!("=== phase: {} ===", phase.name);
        if phase.steps.is_empty() {
            println!("  (no steps)");
        }
        for step in &phase.steps {
            println!("  # {}", step.note);
            for line in step.describe() {
                println!("  {line}");
            }
        }
        println!();
    }
}

/// execute the plan in order, streaming output. `start` is the (phase, step)
/// cursor of the first step to run; everything before it is skipped (already
/// applied). `on_done` is called with the cursor after each successful step, so
/// the caller can persist a checkpoint for resume.
pub fn execute_plan(
    plan: &[Phase],
    password: Option<&str>,
    start: (usize, usize),
    verbose: bool,
    mut on_done: impl FnMut(usize, usize, &str) -> Result<()>,
) -> Result<()> {
    for (pi, phase) in plan.iter().enumerate() {
        println!(">>> phase: {}", phase.name);
        for (si, step) in phase.steps.iter().enumerate() {
            if pi < start.0 || (pi == start.0 && si < start.1) {
                println!("  - {} (already done, skipping)", step.note);
                continue;
            }
            println!("  - {}", step.note);
            // in verbose mode echo the exact command (password masked); the
            // command's own stdout/stderr already stream to the console.
            if verbose {
                for line in step.describe() {
                    println!("    {line}");
                }
            }
            step.execute(password)?;
            on_done(pi, si, &phase.name)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("raiden-step-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_plan(dir: &std::path::Path) -> Vec<Phase> {
        let step = |n: &str| {
            Step::write(
                n.to_string(),
                dir.join(n).to_string_lossy().into_owned(),
                "x",
            )
        };
        vec![Phase::new("p", vec![step("a"), step("b"), step("c")])]
    }

    #[test]
    fn runs_every_step_from_the_start() {
        let dir = tmpdir("full");
        execute_plan(&write_plan(&dir), None, (0, 0), false, |_, _, _| Ok(())).unwrap();
        for f in ["a", "b", "c"] {
            assert!(dir.join(f).exists(), "{f} should have been written");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_skips_already_applied_steps() {
        let dir = tmpdir("resume");
        // resume past steps 0 and 1: only the third step should run.
        execute_plan(&write_plan(&dir), None, (0, 2), false, |_, _, _| Ok(())).unwrap();
        assert!(!dir.join("a").exists(), "a must be skipped on resume");
        assert!(!dir.join("b").exists(), "b must be skipped on resume");
        assert!(dir.join("c").exists(), "c must run on resume");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn needs_password_detects_secret_steps() {
        let plain = vec![Phase::new("p", vec![Step::run("x", &["true"])])];
        let secret = vec![Phase::new("p", vec![Step::run("x", &["true"]).secret()])];
        assert!(!needs_password(&plain));
        assert!(needs_password(&secret));
    }
}
