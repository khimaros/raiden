// interactive prompts: the encryption password (echo disabled) and the
// destructive-operation confirmation.

use std::io::{self, BufRead, Write};

use anyhow::{bail, Result};

/// read the encryption password without echoing it; verify it when requested.
pub fn read_password(verify: bool) -> Result<String> {
    let pw = hidden("encryption password: ")?;
    if verify {
        let again = hidden("encryption password (verify): ")?;
        if pw != again {
            bail!("passwords did not match");
        }
    }
    if pw.is_empty() {
        bail!("empty password");
    }
    Ok(pw)
}

fn hidden(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    // toggle terminal echo via stty, matching the predecessor's `read -s`.
    let off = std::process::Command::new("stty")
        .arg("-echo")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    if off {
        let _ = std::process::Command::new("stty").arg("echo").status();
    }
    println!();
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// ask for explicit confirmation of a destructive action.
pub fn confirm(message: &str) -> Result<bool> {
    print!("{message} [y/N]: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// ask for a line of input, returning `default` when the answer is empty.
pub fn ask(prompt: &str, default: &str) -> Result<String> {
    print!("{prompt} [{default}]: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let ans = line.trim();
    Ok(if ans.is_empty() {
        default.to_string()
    } else {
        ans.to_string()
    })
}

/// present a numbered menu and return the chosen 0-based index, defaulting to
/// `default` on an empty answer and re-asking on anything out of range.
pub fn choose(prompt: &str, options: &[&str], default: usize) -> Result<usize> {
    for (i, o) in options.iter().enumerate() {
        println!("  {}) {o}", i + 1);
    }
    loop {
        let ans = ask(prompt, &(default + 1).to_string())?;
        if let Ok(n) = ans.parse::<usize>() {
            if (1..=options.len()).contains(&n) {
                return Ok(n - 1);
            }
        }
        eprintln!("enter a number between 1 and {}", options.len());
    }
}
