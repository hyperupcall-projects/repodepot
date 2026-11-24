//! Backend side of opening repos: detecting apps, writing workspace files, launching.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::task::spawn_blocking;

use super::{Event, Shared, lock};
use crate::launch::{self, WorkspaceKind};

/// What the UI needs to know about an installed launcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherInfo {
    pub name: String,
    pub each: bool,
    pub together: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchRequest {
    pub launcher: String,
    /// One window for all targets, rather than one each.
    pub together: bool,
    /// (`owner/name`, folder) for each repo.
    pub targets: Vec<(String, PathBuf)>,
    /// Names the workspace file, so the same group reuses the same file.
    pub workspace_name: String,
}

pub(super) async fn detect_launchers(shared: &Shared) {
    let custom = lock(&shared.config).launch.custom.clone();
    let found = spawn_blocking(move || {
        // Custom launchers come first, and replace a built-in one with the same name.
        let mut specs = custom;
        for preset in launch::presets() {
            if !specs.iter().any(|s| s.name == preset.name) {
                specs.push(preset);
            }
        }
        launch::detect(&specs)
    })
    .await
    .unwrap_or_default();
    let infos = found
        .iter()
        .map(|l| LauncherInfo {
            name: l.spec.name.clone(),
            each: l.can_open_each(),
            together: l.can_open_together(),
        })
        .collect();
    *lock(&shared.launchers) = found;
    shared.emitter.emit(Event::Launchers(infos));
}

pub(super) async fn open(shared: &Arc<Shared>, request: LaunchRequest) -> Result<()> {
    let launcher = lock(&shared.launchers)
        .iter()
        .find(|l| l.spec.name == request.launcher)
        .cloned()
        .with_context(|| format!("{} isn't installed (any more)", request.launcher))?;
    if request.targets.is_empty() {
        bail!("Nothing to open.");
    }
    if request.together {
        let paths: Vec<PathBuf> = request.targets.iter().map(|(_, p)| p.clone()).collect();
        let workspace = match launcher.spec.workspace {
            WorkspaceKind::None => None,
            kind => {
                let dir = shared.workspaces_dir.clone();
                let (name, targets) = (request.workspace_name.clone(), request.targets.clone());
                Some(
                    spawn_blocking(move || launch::write_workspace(&dir, kind, &name, &targets))
                        .await??,
                )
            }
        };
        let command = launcher.together_command(&paths, workspace.as_deref())?;
        spawn_detached(shared, command)?;
    } else {
        for (_, path) in &request.targets {
            let command = launcher.each_command(path)?;
            spawn_detached(shared, command)?;
        }
    }
    Ok(())
}

/// Starts an app in its own session with no input or output, so it outlives depository.
/// Reports a quick failure (a non-zero exit within a few seconds), and reaps the process.
fn spawn_detached(shared: &Arc<Shared>, command: Vec<String>) -> Result<()> {
    let (program, args) = command.split_first().context("empty command")?;
    let mut process = tokio::process::Command::new(program);
    process
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::jobs::in_new_session(&mut process);
    let mut child = process
        .spawn()
        .with_context(|| format!("couldn't start {program}"))?;
    let shared = shared.clone();
    let program = program.clone();
    tokio::spawn(async move {
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) if !status.success() => shared.emitter.emit(Event::LaunchFailed(
                format!("{program} stopped with {status}"),
            )),
            Ok(_) => {}
            // Still running (most editors): wait for it so it doesn't linger as a zombie.
            Err(_) => {
                let _ = child.wait().await;
            }
        }
    });
    Ok(())
}
