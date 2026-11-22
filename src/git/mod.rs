//! Running `git` and reading what it says.

pub mod remote_url;
pub mod scan;
pub mod status;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};

/// Runs `git -C <dir> <args>` and returns stdout.
///
/// Never prompts, and uses `GIT_OPTIONAL_LOCKS=0` so a background `git status`
/// doesn't take the index lock while you're working in the repo.
pub async fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let child = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(60), child)
        .await
        .with_context(|| format!("git {} took over a minute", args.join(" ")))?
        .context("couldn't run git (is it installed?)")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        bail!(
            "{}",
            if message.is_empty() {
                format!("git {} failed ({})", args.join(" "), output.status)
            } else {
                message.to_owned()
            }
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
