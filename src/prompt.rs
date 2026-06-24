// interactive prompts: the encryption password (echo disabled) and the
// destructive-operation confirmation.

use std::io::{self, BufRead, Write};

use anyhow::{bail, Result};

/// read the encryption password without echoing it; when `verify` is set, read it
/// twice and re-prompt until the two entries match. an empty entry re-prompts too,
/// so a typo or a stray enter does not abort the operation -- a mistyped disk
/// password killing a long install was the motivating annoyance.
pub fn read_password(verify: bool) -> Result<String> {
    read_password_with(verify, hidden)
}

/// the prompt/verify/retry loop, parameterized over the line reader so it is
/// testable without a tty. the reader returns Err at end of input, so a closed
/// stdin breaks the loop instead of spinning on empty reads.
fn read_password_with(
    verify: bool,
    mut read: impl FnMut(&str) -> Result<String>,
) -> Result<String> {
    loop {
        let pw = read("encryption password: ")?;
        if pw.is_empty() {
            eprintln!("password must not be empty; try again");
            continue;
        }
        if !verify {
            return Ok(pw);
        }
        if pw == read("encryption password (verify): ")? {
            return Ok(pw);
        }
        eprintln!("passwords did not match; try again");
    }
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
    let n = io::stdin().lock().read_line(&mut line)?;
    if off {
        let _ = std::process::Command::new("stty").arg("echo").status();
    }
    println!();
    if n == 0 {
        bail!("unexpected end of input while reading the password");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// a line reader over a fixed script of inputs; panics if asked for more than
    /// scripted, which would mean the retry loop diverged from what we expect.
    fn scripted(inputs: Vec<&'static str>) -> impl FnMut(&str) -> Result<String> {
        let mut it = inputs.into_iter();
        move |_| {
            Ok(it
                .next()
                .expect("read_password asked for more input than scripted")
                .to_string())
        }
    }

    #[test]
    fn verify_retries_until_the_pair_matches() {
        // first pair mismatches (a/b), second pair matches.
        let pw = read_password_with(true, scripted(vec!["a", "b", "secret", "secret"])).unwrap();
        assert_eq!(pw, "secret");
    }

    #[test]
    fn reprompts_on_an_empty_entry() {
        let pw = read_password_with(true, scripted(vec!["", "ok", "ok"])).unwrap();
        assert_eq!(pw, "ok");
    }

    #[test]
    fn without_verify_takes_the_first_nonempty() {
        let pw = read_password_with(false, scripted(vec!["", "once"])).unwrap();
        assert_eq!(pw, "once");
    }

    #[test]
    fn end_of_input_aborts_instead_of_looping() {
        // a reader that always errs (closed stdin) must not spin forever.
        let r = read_password_with(true, |_| anyhow::bail!("eof"));
        assert!(r.is_err());
    }
}
