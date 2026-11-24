//! Backend tasks for GitHub sync, triage and cloning.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;

use super::{Event, Shared, lock, start_run, with_db};
use crate::db::Source;
use crate::git;
use crate::github::sync::{self, Lookup, RestCheck, SyncRequest};
use crate::jobs::{RunSpec, Target, TargetStatus};
use crate::layout;
use crate::log;
use crate::token;

/// Clones one repo. `set -e` covers only the clone itself: failing to add the upstream remote
/// is reported but doesn't fail (and so doesn't delete) a good clone.
const CLONE_SCRIPT: &str = r#"set -e
git clone --progress --origin "$DEPO_REMOTE" $DEPO_CLONE_FLAGS -- "$DEPO_URL" "$DEPO_DEST"
set +e
if [ -n "$DEPO_UPSTREAM_URL" ]; then
  if git -C "$DEPO_DEST" remote add "$DEPO_UPSTREAM" "$DEPO_UPSTREAM_URL"; then
    echo "Added remote $DEPO_UPSTREAM: $DEPO_UPSTREAM_URL"
  else
    echo "Couldn't add remote $DEPO_UPSTREAM: $DEPO_UPSTREAM_URL" >&2
  fi
fi
exit 0"#;

pub struct SyncReport {
    /// Repos with at least one source.
    pub relevant: usize,
    pub new_forks: usize,
    pub counts: BTreeMap<Source, usize>,
    pub warnings: Vec<String>,
    pub took: Duration,
}

async fn github_token() -> Result<String> {
    spawn_blocking(token::find)
        .await?
        .found
        .map(|found| found.token)
        .ok_or_else(|| anyhow!("Not signed in to GitHub. Add a token in Settings."))
}

fn http(shared: &Shared) -> Result<&reqwest::Client> {
    shared.http.as_ref().map_err(|e| anyhow!(e.clone()))
}

pub(super) async fn run_sync(shared: &Shared) -> Result<SyncReport> {
    let started = Instant::now();
    let http = http(shared)?;
    let token = github_token().await?;
    let (manual_repos, manual_owners) =
        with_db(shared, |db| Ok((db.manual_repos()?, db.manual_owners()?))).await?;
    let request = SyncRequest {
        sources: lock(&shared.config).github.sources.clone(),
        manual_repos,
        manual_owners,
    };
    let emitter = shared.emitter.clone();
    let progress = move |message: String| emitter.emit(Event::SyncProgress(message));
    let fetched = sync::fetch_all(http, &token, &request, &progress).await?;

    let repos = fetched.repos;
    let applied = with_db(shared, move |db| db.apply_sync(&repos)).await?;

    // Clones of repos no source lists: ask whether they moved or are gone.
    let mut warnings = fetched.warnings;
    let unlisted = with_db(shared, |db| db.unlisted_clone_repos()).await?;
    for (id, owner, name) in unlisted {
        progress(format!("Checking {owner}/{name}"));
        match sync::check_repo(http, &token, &owner, &name).await {
            Ok(RestCheck::Exists) => with_db(shared, move |db| db.mark_found(id)).await?,
            Ok(RestCheck::NotFound) => with_db(shared, move |db| db.mark_not_found(id)).await?,
            Ok(RestCheck::Moved(meta)) => {
                with_db(shared, move |db| db.mark_renamed(id, &meta)).await?;
            }
            Err(e) => warnings.push(format!("{owner}/{name}: {e:#}")),
        }
    }

    Ok(SyncReport {
        relevant: applied.relevant,
        new_forks: applied.new_forks,
        counts: fetched.counts,
        warnings,
        took: started.elapsed(),
    })
}

pub(super) async fn refresh_catalog(shared: &Shared) {
    match with_db(shared, |db| db.catalog()).await {
        Ok(catalog) => shared.emitter.emit(Event::Catalog(catalog)),
        Err(e) => shared
            .emitter
            .log(log::Entry::error("Couldn't build the repo lists").detail(format!("{e:#}"))),
    }
}

/// Adds a repo or owner by hand. Returns a message, and whether a sync is needed to fetch repos.
pub(super) async fn add_manual(shared: &Shared, text: String) -> Result<(String, bool)> {
    let http = http(shared)?;
    let token = github_token().await?;
    match sync::look_up(http, &token, &text).await? {
        Lookup::Repo(meta) => {
            let name = meta.full_name();
            with_db(shared, move |db| db.add_manual_repo(&meta)).await?;
            Ok((format!("Added {name}."), false))
        }
        Lookup::Owner(login) => {
            let message = format!("Added every repo of {login}. Syncing to fetch them…");
            with_db(shared, move |db| db.add_manual_owner(&login)).await?;
            Ok((message, true))
        }
        Lookup::NotFound => bail!(
            "GitHub has nothing called {} (or the token can't see it).",
            text.trim()
        ),
    }
}

/// Clones repos into the layout through the job engine, then rescans.
pub(super) async fn clone_repos(
    shared: Arc<Shared>,
    repo_ids: Vec<i64>,
    scan_tx: mpsc::UnboundedSender<()>,
) -> Result<()> {
    let config = lock(&shared.config).clone();
    let root = config
        .clone_root
        .clone()
        .ok_or_else(|| anyhow!("Set a clone root in Settings before cloning."))?;
    if !root.is_dir() {
        bail!("The clone root {} doesn't exist.", root.display());
    }
    let ids = repo_ids.clone();
    let (items, existing) = with_db(&shared, move |db| {
        Ok((db.clone_items(&ids)?, db.clone_paths()?))
    })
    .await?;
    if items.is_empty() {
        bail!("Nothing to clone.");
    }

    let mut flags = Vec::new();
    if config.clone.partial {
        flags.push("--filter=blob:none");
    }
    if config.clone.recurse_submodules {
        flags.push("--recurse-submodules");
    }
    let mut taken: HashSet<String> = existing.iter().map(|p| p.to_lowercase()).collect();
    let mut targets = Vec::new();
    let mut destinations = Vec::new();
    for item in &items {
        let rel = layout::assign_path(&item.name, |candidate| {
            taken.contains(&candidate.to_lowercase())
                || layout::exists_ignoring_case(&root, candidate)
        });
        taken.insert(rel.to_lowercase());
        let env = [
            ("DEPO_URL", item.ssh_url.clone()),
            ("DEPO_DEST", rel.clone()),
            ("DEPO_REMOTE", config.remotes.default_name.clone()),
            ("DEPO_UPSTREAM", config.remotes.upstream_name.clone()),
            (
                "DEPO_UPSTREAM_URL",
                item.upstream_url.clone().unwrap_or_default(),
            ),
            ("DEPO_CLONE_FLAGS", flags.join(" ")),
        ];
        targets.push(Target {
            name: item.full_name.clone(),
            path: root.clone(),
            tags: Vec::new(),
            env: env.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        });
        destinations.push(root.join(&rel));
    }

    let count = items.len();
    let spec = RunSpec {
        title: format!(
            "Clone {count} {}",
            if count == 1 { "repo" } else { "repos" }
        ),
        command: CLONE_SCRIPT.to_owned(),
        shell: "/bin/sh".to_owned(),
        targets,
        parallel: config.runner.parallel.max(1) as usize,
        timeout: None,
        line_limit: config.runner.line_limit as usize,
        colors: false,
    };
    let (run, handle) = start_run(&shared, spec, None);
    let _ = handle.await;

    // Each destination was free when it was picked, so anything there after a failure is
    // what's left of that clone.
    for (index, destination) in destinations.iter().enumerate() {
        let cloned = matches!(run.status(index), TargetStatus::Exited { code: 0, .. });
        if !cloned
            && destination.exists()
            && let Err(e) = std::fs::remove_dir_all(destination)
        {
            shared.emitter.log(
                log::Entry::warning(format!(
                    "Couldn't remove what's left of a failed clone at {}",
                    destination.display()
                ))
                .detail(e.to_string()),
            );
        }
    }
    shared.emitter.emit(Event::ClonesFinished(repo_ids));
    let _ = scan_tx.send(());
    Ok(())
}

/// `git remote add` (when `add`) or `git remote set-url`.
pub(super) async fn set_remote(path: PathBuf, name: String, url: String, add: bool) -> Result<()> {
    let verb = if add { "add" } else { "set-url" };
    git::run(&path, &["remote", verb, &name, &url]).await?;
    Ok(())
}
