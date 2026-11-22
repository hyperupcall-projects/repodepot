//! Backend tasks for Import: previewing where repos would go, then moving them.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;

use super::{Shared, lock, start_run, with_db};
use crate::git::import::{self, Discovery, ImportPreview, PlanContext};
use crate::jobs::{RunSpec, Target};

/// Moves one repo. `mv` renames when it can and copies across filesystems; `-n` refuses to
/// overwrite, and the check afterwards turns a refusal into a failure.
const MOVE_SCRIPT: &str = r#"set -e
mkdir -p -- "$(dirname -- "$DEPO_DEST")"
mv -n -T -- "$DEPO_SOURCE" "$DEPO_DEST"
if [ -e "$DEPO_SOURCE" ]; then
  echo "Not moved: something is already at $DEPO_DEST" >&2
  exit 1
fi
echo "Moved $DEPO_SOURCE to $DEPO_DEST"
set +e
if [ -n "$(ls -A "$DEPO_DEST/.git/worktrees" 2>/dev/null)" ]; then
  git -C "$DEPO_DEST" worktree repair && echo "Repaired linked worktrees"
fi
if [ -n "$DEPO_RENAME_ORIGIN" ]; then
  git -C "$DEPO_DEST" remote rename origin "$DEPO_RENAME_ORIGIN" \
    && echo "Renamed remote origin to $DEPO_RENAME_ORIGIN"
fi
exit 0"#;

/// One planned move, as confirmed in the preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportMove {
    pub source: PathBuf,
    /// Relative to the clone root.
    pub destination: String,
    pub rename_origin: bool,
}

fn clone_root(shared: &Shared) -> Result<PathBuf> {
    let root = lock(&shared.config)
        .clone_root
        .clone()
        .ok_or_else(|| anyhow!("Set a clone root in Settings first."))?;
    if !root.is_dir() {
        bail!("The clone root {} doesn't exist.", root.display());
    }
    Ok(root)
}

/// Searches `folder` (or takes `paths` as the repos) and plans each move.
pub(super) async fn preview(
    shared: &Shared,
    folder: Option<PathBuf>,
    paths: Vec<PathBuf>,
) -> Result<ImportPreview> {
    let root = clone_root(shared)?;
    let discovery = match &folder {
        Some(folder) => {
            let folder = folder.clone();
            spawn_blocking(move || import::discover(&folder)).await??
        }
        None => Discovery {
            repos: paths,
            ..Default::default()
        },
    };
    let (existing_paths, clones_by_repo) =
        with_db(shared, |db| Ok((db.clone_paths()?, db.clones_by_repo()?))).await?;
    let (default_remote, github_aliases) = {
        let config = lock(&shared.config);
        (
            config.remotes.default_name.clone(),
            config.github.ssh_host_aliases.clone(),
        )
    };
    let cx = PlanContext {
        root,
        default_remote,
        github_aliases,
        existing_paths,
        clones_by_repo,
    };
    Ok(import::plan(discovery, folder, &cx).await)
}

/// Moves repos into the layout through the job engine, then rescans.
pub(super) async fn run_import(
    shared: Arc<Shared>,
    moves: Vec<ImportMove>,
    scan_tx: mpsc::UnboundedSender<()>,
) -> Result<()> {
    if moves.is_empty() {
        bail!("Nothing selected to move.");
    }
    let root = clone_root(&shared)?;
    let config = lock(&shared.config).clone();
    let targets: Vec<Target> = moves
        .iter()
        .map(|m| {
            let rename = if m.rename_origin {
                config.remotes.default_name.clone()
            } else {
                String::new()
            };
            Target {
                name: m.destination.clone(),
                path: root.clone(),
                tags: Vec::new(),
                env: vec![
                    ("DEPO_SOURCE".to_owned(), m.source.display().to_string()),
                    ("DEPO_DEST".to_owned(), m.destination.clone()),
                    ("DEPO_RENAME_ORIGIN".to_owned(), rename),
                ],
            }
        })
        .collect();
    let count = targets.len();
    let spec = RunSpec {
        title: format!(
            "Move {count} {} into place",
            if count == 1 { "repo" } else { "repos" }
        ),
        command: MOVE_SCRIPT.to_owned(),
        shell: "/bin/sh".to_owned(),
        targets,
        // Renames are instant; copies across disks are heavy, so keep it to two.
        parallel: 2,
        timeout: None,
        line_limit: config.runner.line_limit as usize,
        colors: false,
    };
    let (_, handle) = start_run(&shared, spec, None);
    let _ = handle.await;
    let _ = scan_tx.send(());
    Ok(())
}
