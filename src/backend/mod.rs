//! Background work. The UI sends [`Command`]s; the backend answers with [`Event`]s and asks
//! egui to repaint. Slow work (network, keyring, database, processes) never runs on the UI thread.

mod import;
mod open;
mod repos;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::anyhow;
use eframe::egui;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, spawn_blocking};

pub use import::ImportMove;
pub use open::{LaunchRequest, LauncherInfo, Origin};
pub use repos::SyncReport;

use crate::config::{Config, Paths};
use crate::db::{Catalog, Db, ForkDecision, HistoryEntry, Snippet, TagStore};
use crate::git::import::ImportPreview;
use crate::git::scan::{self, ScanResult, ScanSettings};
use crate::github::{self, RateLimit, Viewer};
use crate::jobs::{self, OnChange, Run, RunSpec, Target};
use crate::tags::{self, Rule};
use crate::token::{self, TokenSource};

/// The most often the UI repaints for streaming output.
const OUTPUT_REPAINT_INTERVAL: Duration = Duration::from_millis(50);

pub enum Command {
    CheckAuth,
    /// Check a pasted token with GitHub, then save it to the keyring if it works.
    SaveToken(String),
    ForgetToken,
    /// Settings were saved. Rescans if anything the scan depends on changed.
    ConfigChanged(Config),
    Scan,
    /// Pick a clone's main remote by hand (`None` goes back to the automatic rule).
    SetMainRemote {
        clone_id: i64,
        remote: Option<String>,
    },
    /// Forget a clone whose folder is gone.
    ForgetClone(i64),
    /// Run a shell command in each target. Shell, timeout and line limit come from the settings.
    StartRun {
        /// Echoed back in [`Event::RunStarted`], so the UI knows where the run belongs.
        request: u64,
        command: String,
        targets: Vec<Target>,
        parallel: usize,
        colors: bool,
    },
    SaveSnippet {
        name: String,
        command: String,
    },
    DeleteSnippet(i64),
    /// Fetch the relevant repos from GitHub.
    Sync,
    /// Clone these repos (by database ID) into the layout.
    Clone(Vec<i64>),
    SetIgnored {
        repo_ids: Vec<i64>,
        ignored: bool,
    },
    SetForkDecision {
        fork_id: i64,
        parent_id: i64,
        decision: ForkDecision,
    },
    /// `owner/name`, a GitHub URL, or a user/org login.
    AddManual(String),
    RemoveManualOwner(String),
    /// `git remote add` (when `add`) or `git remote set-url` in a clone.
    SetRemote {
        path: PathBuf,
        name: String,
        url: String,
        add: bool,
    },
    /// Show the desktop's folder chooser.
    PickFolder(FolderPurpose),
    /// Plan an import: search `folder` for repos, or take `paths` as the repos.
    PreviewImport {
        folder: Option<PathBuf>,
        paths: Vec<PathBuf>,
    },
    Import(Vec<ImportMove>),
    /// Add a manual tag to clones. The name is normalized first.
    AddTag {
        clone_ids: Vec<i64>,
        tag: String,
    },
    RemoveTag {
        clone_ids: Vec<i64>,
        tag: String,
    },
    /// Insert (id 0) or update a rule. It's checked first.
    SaveRule(Rule),
    DeleteRule(i64),
    SetRuleEnabled {
        rule_id: i64,
        enabled: bool,
    },
    SetRuleException {
        rule_id: i64,
        clone_id: i64,
        excepted: bool,
    },
    /// Open repos in an editor or other app.
    Launch(LaunchRequest),
}

/// What a picked folder is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderPurpose {
    Import,
    CloneRoot,
}

pub enum Event {
    Db(Result<DbInfo, String>),
    Auth(AuthState),
    /// Outcome of saving or forgetting the token, for the Settings page.
    AccountMessage(Result<String, String>),
    ScanStarted,
    ScanProgress {
        done: usize,
        total: usize,
    },
    Scan(Result<ScanResult, String>),
    /// Something the user asked for on the Repositories tab failed.
    RepoActionFailed(String),
    Snippets(Vec<Snippet>),
    /// The UI reads the run's progress and output directly, and can cancel it.
    /// `request` is the UI's ID for runs it asked for; `None` for clones and imports.
    RunStarted {
        run: Arc<Run>,
        request: Option<u64>,
    },
    /// Recent commands, newest first.
    History(Vec<HistoryEntry>),
    SyncStarted,
    SyncProgress(String),
    SyncFinished(Result<SyncReport, String>),
    /// Fresh Inbox, Ignored and Forks lists.
    Catalog(Catalog),
    /// A clone run for these repo IDs is over (whether or not it worked).
    ClonesFinished(Vec<i64>),
    /// Something the user asked for on the Repositories tab worked.
    RepoNotice(String),
    FolderPicked {
        purpose: FolderPurpose,
        path: PathBuf,
    },
    ImportSearching,
    ImportPreview(Result<ImportPreview, String>),
    /// Manual tags, rules and repo facts, after any change.
    Tags(TagStore),
    /// Something the user asked for on the Tags tab failed.
    TagActionFailed(String),
    /// The apps that can open repos on this computer.
    Launchers(Vec<LauncherInfo>),
    LaunchFailed {
        origin: Origin,
        message: String,
    },
}

pub struct DbInfo {
    pub path: PathBuf,
    pub schema_version: i64,
}

#[derive(Clone)]
pub enum AuthState {
    Checking,
    NoToken {
        keyring_error: Option<String>,
    },
    SignedIn {
        viewer: Viewer,
        rate: RateLimit,
        source: TokenSource,
        keyring_error: Option<String>,
    },
    Failed {
        source: Option<TokenSource>,
        error: String,
        keyring_error: Option<String>,
    },
}

impl AuthState {
    pub fn keyring_error(&self) -> Option<&str> {
        match self {
            AuthState::Checking => None,
            AuthState::NoToken { keyring_error }
            | AuthState::SignedIn { keyring_error, .. }
            | AuthState::Failed { keyring_error, .. } => keyring_error.as_deref(),
        }
    }

    pub fn source(&self) -> Option<TokenSource> {
        match self {
            AuthState::SignedIn { source, .. } => Some(*source),
            AuthState::Failed { source, .. } => *source,
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Emitter {
    tx: std_mpsc::Sender<Event>,
    ctx: egui::Context,
}

impl Emitter {
    fn emit(&self, event: Event) {
        if self.tx.send(event).is_ok() {
            self.ctx.request_repaint();
        }
    }
}

pub struct Backend {
    commands: mpsc::UnboundedSender<Command>,
    events: std_mpsc::Receiver<Event>,
}

impl Backend {
    pub fn start(ctx: egui::Context, paths: Paths, config: Config) -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = std_mpsc::channel();
        let emitter = Emitter { tx: event_tx, ctx };
        std::thread::Builder::new()
            .name("backend".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        tracing::error!("couldn't start the background runtime: {e}");
                        return;
                    }
                };
                runtime.block_on(run(command_rx, emitter, paths, config));
            })
            .expect("couldn't spawn the backend thread");
        Self {
            commands: command_tx,
            events: event_rx,
        }
    }

    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            tracing::error!("the backend has stopped; command dropped");
        }
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

/// State shared by the backend's tasks.
struct Shared {
    emitter: Emitter,
    http: Result<reqwest::Client, String>,
    db: Option<Arc<Mutex<Db>>>,
    config: Mutex<Config>,
    last_run_id: AtomicU64,
    /// Installed launchers, refreshed when custom ones change.
    launchers: Mutex<Vec<crate::launch::Launcher>>,
    workspaces_dir: PathBuf,
}

enum AuthJob {
    Check,
    Save(String),
    Forget,
}

async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    emitter: Emitter,
    paths: Paths,
    config: Config,
) {
    let db = open_db(&paths, &emitter)
        .await
        .map(|db| Arc::new(Mutex::new(db)));
    let shared = Arc::new(Shared {
        http: github::http_client().map_err(|e| format!("{e:#}")),
        db,
        config: Mutex::new(config),
        emitter,
        last_run_id: AtomicU64::new(0),
        launchers: Mutex::new(Vec::new()),
        workspaces_dir: paths.workspaces_dir.clone(),
    });

    // Auth jobs run one at a time, in order.
    let (auth_tx, mut auth_rx) = mpsc::unbounded_channel::<AuthJob>();
    tokio::spawn({
        let shared = shared.clone();
        async move {
            while let Some(job) = auth_rx.recv().await {
                match job {
                    AuthJob::Check => check_auth(&shared).await,
                    AuthJob::Save(token) => save_token(&shared, token).await,
                    AuthJob::Forget => forget_token(&shared).await,
                }
            }
        }
    });

    // Scans run one at a time; requests that pile up during a scan become one rescan.
    let (scan_tx, mut scan_rx) = mpsc::unbounded_channel::<()>();
    tokio::spawn({
        let shared = shared.clone();
        async move {
            while scan_rx.recv().await.is_some() {
                while scan_rx.try_recv().is_ok() {}
                run_scan(&shared).await;
                // Which repos count as cloned, and what's known about them, may have changed.
                repos::refresh_catalog(&shared).await;
                refresh_tags(&shared).await;
            }
        }
    });

    // Syncs work the same way, and end with a rescan.
    let (sync_tx, mut sync_rx) = mpsc::unbounded_channel::<()>();
    tokio::spawn({
        let shared = shared.clone();
        let scan_tx = scan_tx.clone();
        async move {
            while sync_rx.recv().await.is_some() {
                while sync_rx.try_recv().is_ok() {}
                shared.emitter.emit(Event::SyncStarted);
                let result = repos::run_sync(&shared).await.map_err(|e| format!("{e:#}"));
                if let Err(e) = &result {
                    tracing::warn!("sync failed: {e}");
                }
                shared.emitter.emit(Event::SyncFinished(result));
                let _ = scan_tx.send(());
            }
        }
    });

    let (has_root, sync_on_startup) = {
        let config = lock(&shared.config);
        (config.clone_root.is_some(), config.github.sync_on_startup)
    };
    tokio::spawn({
        let shared = shared.clone();
        async move {
            repos::refresh_catalog(&shared).await;
            refresh_tags(&shared).await;
        }
    });
    if has_root {
        let _ = scan_tx.send(());
    }
    if sync_on_startup {
        let _ = sync_tx.send(());
    }
    tokio::spawn(load_snippets(shared.clone()));
    tokio::spawn({
        let shared = shared.clone();
        async move { open::detect_launchers(&shared).await }
    });

    while let Some(command) = commands.recv().await {
        match command {
            Command::CheckAuth => {
                let _ = auth_tx.send(AuthJob::Check);
            }
            Command::SaveToken(token) => {
                let _ = auth_tx.send(AuthJob::Save(token));
            }
            Command::ForgetToken => {
                let _ = auth_tx.send(AuthJob::Forget);
            }
            Command::ConfigChanged(new) => {
                let (rescan, launchers_changed) = {
                    let mut config = lock(&shared.config);
                    let changed = (
                        scan_inputs_differ(&config, &new),
                        config.launch.custom != new.launch.custom,
                    );
                    *config = new;
                    changed
                };
                if rescan {
                    let _ = scan_tx.send(());
                }
                if launchers_changed {
                    let shared = shared.clone();
                    tokio::spawn(async move { open::detect_launchers(&shared).await });
                }
            }
            Command::Launch(request) => {
                let origin = request.origin;
                if let Err(e) = open::open(&shared, request).await {
                    shared.emitter.emit(Event::LaunchFailed {
                        origin,
                        message: format!("{e:#}"),
                    });
                }
            }
            Command::Scan => {
                let _ = scan_tx.send(());
            }
            Command::SetMainRemote { clone_id, remote } => {
                let shared = shared.clone();
                let scan_tx = scan_tx.clone();
                tokio::spawn(async move {
                    let changed = write_db(&shared, move |db| {
                        db.set_main_remote(clone_id, remote.as_deref())
                    })
                    .await;
                    if changed {
                        let _ = scan_tx.send(());
                    }
                });
            }
            Command::ForgetClone(clone_id) => {
                let shared = shared.clone();
                let scan_tx = scan_tx.clone();
                tokio::spawn(async move {
                    if write_db(&shared, move |db| db.forget_missing_clone(clone_id)).await {
                        let _ = scan_tx.send(());
                    }
                });
            }
            Command::StartRun {
                request,
                command,
                targets,
                parallel,
                colors,
            } => {
                let spec = run_spec(&lock(&shared.config), command, targets, parallel, colors);
                let (run, handle) = start_run(&shared, spec, Some(request));
                let scan_tx = scan_tx.clone();
                let shared = shared.clone();
                tokio::spawn(async move {
                    let _ = handle.await;
                    // The command may have changed branches, files or commits.
                    let _ = scan_tx.send(());
                    record_history(&shared, &run).await;
                });
            }
            Command::SaveSnippet { name, command } => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let saved = with_db(&shared, move |db| db.save_snippet(&name, &command)).await;
                    match saved {
                        Ok(()) => load_snippets(shared).await,
                        Err(e) => tracing::warn!("couldn't save the snippet: {e:#}"),
                    }
                });
            }
            Command::DeleteSnippet(id) => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    match with_db(&shared, move |db| db.delete_snippet(id)).await {
                        Ok(()) => load_snippets(shared).await,
                        Err(e) => tracing::warn!("couldn't delete the snippet: {e:#}"),
                    }
                });
            }
            Command::Sync => {
                let _ = sync_tx.send(());
            }
            Command::Clone(repo_ids) => {
                let shared = shared.clone();
                let scan_tx = scan_tx.clone();
                tokio::spawn(async move {
                    let ids = repo_ids.clone();
                    if let Err(e) = repos::clone_repos(shared.clone(), repo_ids, scan_tx).await {
                        shared
                            .emitter
                            .emit(Event::RepoActionFailed(format!("{e:#}")));
                        shared.emitter.emit(Event::ClonesFinished(ids));
                    }
                });
            }
            Command::SetIgnored { repo_ids, ignored } => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    if write_db(&shared, move |db| db.set_ignored(&repo_ids, ignored)).await {
                        repos::refresh_catalog(&shared).await;
                    }
                });
            }
            Command::SetForkDecision {
                fork_id,
                parent_id,
                decision,
            } => {
                let shared = shared.clone();
                let scan_tx = scan_tx.clone();
                tokio::spawn(async move {
                    let written = write_db(&shared, move |db| {
                        db.set_fork_decision(fork_id, parent_id, decision)
                    })
                    .await;
                    if written {
                        // The Inbox changes, and so may the clones' problems.
                        let _ = scan_tx.send(());
                    }
                });
            }
            Command::AddManual(text) => {
                let shared = shared.clone();
                let sync_tx = sync_tx.clone();
                tokio::spawn(async move {
                    match repos::add_manual(&shared, text).await {
                        Ok((message, needs_sync)) => {
                            shared.emitter.emit(Event::RepoNotice(message));
                            repos::refresh_catalog(&shared).await;
                            if needs_sync {
                                let _ = sync_tx.send(());
                            }
                        }
                        Err(e) => shared
                            .emitter
                            .emit(Event::RepoActionFailed(format!("{e:#}"))),
                    }
                });
            }
            Command::RemoveManualOwner(login) => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    if write_db(&shared, move |db| db.remove_manual_owner(&login)).await {
                        repos::refresh_catalog(&shared).await;
                    }
                });
            }
            Command::PickFolder(purpose) => {
                let shared = shared.clone();
                let start = match purpose {
                    FolderPurpose::CloneRoot => lock(&shared.config).clone_root.clone(),
                    FolderPurpose::Import => None,
                }
                .filter(|p| p.is_dir())
                .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()));
                tokio::spawn(async move {
                    let title = match purpose {
                        FolderPurpose::Import => "Folder to import repos from",
                        FolderPurpose::CloneRoot => "Clone root",
                    };
                    let picked = spawn_blocking(move || {
                        let mut dialog = rfd::FileDialog::new().set_title(title);
                        if let Some(start) = start {
                            dialog = dialog.set_directory(start);
                        }
                        dialog.pick_folder()
                    })
                    .await;
                    match picked {
                        Ok(Some(path)) => {
                            shared.emitter.emit(Event::FolderPicked { purpose, path })
                        }
                        Ok(None) => {}
                        Err(e) => tracing::warn!("the folder chooser crashed: {e}"),
                    }
                });
            }
            Command::PreviewImport { folder, paths } => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    shared.emitter.emit(Event::ImportSearching);
                    let result = import::preview(&shared, folder, paths)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    shared.emitter.emit(Event::ImportPreview(result));
                });
            }
            Command::Import(moves) => {
                let shared = shared.clone();
                let scan_tx = scan_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = import::run_import(shared.clone(), moves, scan_tx).await {
                        shared
                            .emitter
                            .emit(Event::RepoActionFailed(format!("{e:#}")));
                    }
                });
            }
            Command::AddTag { clone_ids, tag } => match tags::normalize_tag(&tag) {
                Ok(tag) => tag_write(&shared, move |db| db.add_tag(&clone_ids, &tag)),
                Err(e) => shared.emitter.emit(Event::TagActionFailed(e)),
            },
            Command::RemoveTag { clone_ids, tag } => match tags::normalize_tag(&tag) {
                Ok(tag) => tag_write(&shared, move |db| db.remove_tag(&clone_ids, &tag)),
                Err(e) => shared.emitter.emit(Event::TagActionFailed(e)),
            },
            Command::SaveRule(rule) => match tags::compile(&rule) {
                Ok(_) => tag_write(&shared, move |db| db.save_rule(&rule).map(|_| ())),
                Err(e) => shared.emitter.emit(Event::TagActionFailed(e)),
            },
            Command::DeleteRule(rule_id) => {
                tag_write(&shared, move |db| db.delete_rule(rule_id));
            }
            Command::SetRuleEnabled { rule_id, enabled } => {
                tag_write(&shared, move |db| db.set_rule_enabled(rule_id, enabled));
            }
            Command::SetRuleException {
                rule_id,
                clone_id,
                excepted,
            } => {
                tag_write(&shared, move |db| {
                    db.set_rule_exception(rule_id, clone_id, excepted)
                });
            }
            Command::SetRemote {
                path,
                name,
                url,
                add,
            } => {
                let shared = shared.clone();
                let scan_tx = scan_tx.clone();
                tokio::spawn(async move {
                    match repos::set_remote(path, name, url, add).await {
                        Ok(()) => {
                            let _ = scan_tx.send(());
                        }
                        Err(e) => shared
                            .emitter
                            .emit(Event::RepoActionFailed(format!("{e:#}"))),
                    }
                });
            }
        }
    }
}

/// Sends the UI fresh tags, rules and repo facts.
async fn refresh_tags(shared: &Shared) {
    match with_db(shared, |db| db.tag_store()).await {
        Ok(store) => shared.emitter.emit(Event::Tags(store)),
        Err(e) => tracing::warn!("couldn't load tags: {e:#}"),
    }
}

/// Runs a tag or rule change, then sends the new state (or the error).
fn tag_write(
    shared: &Arc<Shared>,
    write: impl FnOnce(&mut Db) -> anyhow::Result<()> + Send + 'static,
) {
    let shared = shared.clone();
    tokio::spawn(async move {
        match with_db(&shared, write).await {
            Ok(()) => refresh_tags(&shared).await,
            Err(e) => shared
                .emitter
                .emit(Event::TagActionFailed(format!("{e:#}"))),
        }
    });
}

/// Saves a finished command to the history and sends the UI the new list.
async fn record_history(shared: &Shared, run: &Run) {
    let summary = run.summary();
    let (command, took, repos) = (run.command.clone(), run.elapsed(), run.targets.len());
    let recorded = with_db(shared, move |db| {
        db.add_history(&command, took, repos, summary.succeeded, summary.failed)?;
        db.history(50)
    })
    .await;
    match recorded {
        Ok(history) => shared.emitter.emit(Event::History(history)),
        Err(e) => tracing::warn!("couldn't record the command: {e:#}"),
    }
}

/// Starts a run and hands it to the UI.
fn start_run(shared: &Shared, spec: RunSpec, request: Option<u64>) -> (Arc<Run>, JoinHandle<()>) {
    let id = shared.last_run_id.fetch_add(1, Ordering::Relaxed) + 1;
    let ctx = shared.emitter.ctx.clone();
    let on_change: OnChange = Arc::new(move || ctx.request_repaint_after(OUTPUT_REPAINT_INTERVAL));
    let (run, handle) = jobs::start(id, spec, on_change);
    shared.emitter.emit(Event::RunStarted {
        run: run.clone(),
        request,
    });
    (run, handle)
}

fn run_spec(
    config: &Config,
    command: String,
    targets: Vec<Target>,
    parallel: usize,
    colors: bool,
) -> RunSpec {
    let shell = config
        .runner
        .shell
        .clone()
        .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "/bin/sh".to_owned());
    RunSpec {
        title: command.clone(),
        command,
        shell,
        targets,
        parallel: parallel.max(1),
        timeout: (config.runner.timeout_secs > 0)
            .then(|| Duration::from_secs(config.runner.timeout_secs)),
        line_limit: config.runner.line_limit as usize,
        colors,
    }
}

async fn load_snippets(shared: Arc<Shared>) {
    match with_db(&shared, |db| Ok((db.snippets()?, db.history(50)?))).await {
        Ok((snippets, history)) => {
            shared.emitter.emit(Event::Snippets(snippets));
            shared.emitter.emit(Event::History(history));
        }
        Err(e) => tracing::warn!("couldn't load snippets: {e:#}"),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn scan_inputs_differ(old: &Config, new: &Config) -> bool {
    old.clone_root != new.clone_root
        || old.remotes.default_name != new.remotes.default_name
        || old.github.ssh_host_aliases != new.github.ssh_host_aliases
}

/// Runs database work off the async threads.
async fn with_db<T: Send + 'static>(
    shared: &Shared,
    work: impl FnOnce(&mut Db) -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let db = shared
        .db
        .clone()
        .ok_or_else(|| anyhow!("The database isn't available."))?;
    spawn_blocking(move || work(&mut lock(&db))).await?
}

/// Runs a database write and reports a failure to the UI. Returns whether it worked.
async fn write_db(
    shared: &Shared,
    write: impl FnOnce(&mut Db) -> anyhow::Result<()> + Send + 'static,
) -> bool {
    match with_db(shared, write).await {
        Ok(()) => true,
        Err(e) => {
            shared
                .emitter
                .emit(Event::RepoActionFailed(format!("{e:#}")));
            false
        }
    }
}

async fn run_scan(shared: &Shared) {
    let settings = ScanSettings::from_config(&lock(&shared.config));
    shared.emitter.emit(Event::ScanStarted);
    let result = match &shared.db {
        None => Err("The database isn't available, so scanning is off.".to_owned()),
        Some(db) => {
            let emitter = shared.emitter.clone();
            scan::scan(&settings, db.clone(), move |done, total| {
                emitter.emit(Event::ScanProgress { done, total });
            })
            .await
            .map_err(|e| format!("{e:#}"))
        }
    };
    if let Err(e) = &result {
        tracing::warn!("scan failed: {e}");
    }
    shared.emitter.emit(Event::Scan(result));
}

async fn open_db(paths: &Paths, emitter: &Emitter) -> Option<Db> {
    let path = paths.db_file.clone();
    let opened = spawn_blocking(move || {
        let db = Db::open(&path)?;
        let version = db.schema_version()?;
        anyhow::Ok((db, version))
    })
    .await;
    match opened {
        Ok(Ok((db, schema_version))) => {
            emitter.emit(Event::Db(Ok(DbInfo {
                path: paths.db_file.clone(),
                schema_version,
            })));
            Some(db)
        }
        Ok(Err(e)) => {
            tracing::error!("database: {e:#}");
            emitter.emit(Event::Db(Err(format!("{e:#}"))));
            None
        }
        Err(e) => {
            emitter.emit(Event::Db(Err(format!("opening the database crashed: {e}"))));
            None
        }
    }
}

async fn check_auth(shared: &Shared) {
    let emitter = &shared.emitter;
    emitter.emit(Event::Auth(AuthState::Checking));
    let lookup = match spawn_blocking(token::find).await {
        Ok(lookup) => lookup,
        Err(e) => {
            emitter.emit(Event::Auth(AuthState::Failed {
                source: None,
                error: format!("looking for a token crashed: {e}"),
                keyring_error: None,
            }));
            return;
        }
    };
    let keyring_error = lookup.keyring_error;
    let state = match (lookup.found, &shared.http) {
        (None, _) => AuthState::NoToken { keyring_error },
        (Some(found), Err(e)) => AuthState::Failed {
            source: Some(found.source),
            error: e.clone(),
            keyring_error,
        },
        (Some(found), Ok(http)) => match github::whoami(http, &found.token).await {
            Ok((viewer, rate)) => AuthState::SignedIn {
                viewer,
                rate,
                source: found.source,
                keyring_error,
            },
            Err(e) => AuthState::Failed {
                source: Some(found.source),
                error: format!("{e:#}"),
                keyring_error,
            },
        },
    };
    emitter.emit(Event::Auth(state));
}

async fn save_token(shared: &Shared, token: String) {
    let emitter = &shared.emitter;
    let token = token.trim().to_owned();
    let client = match &shared.http {
        Ok(client) => client,
        Err(e) => {
            emitter.emit(Event::AccountMessage(Err(format!("Not saved: {e}"))));
            return;
        }
    };
    if let Err(e) = github::whoami(client, &token).await {
        emitter.emit(Event::AccountMessage(Err(format!("Not saved: {e:#}"))));
        return;
    }
    let saved = spawn_blocking(move || token::save_to_keyring(&token)).await;
    let message = match saved {
        Ok(Ok(())) => Ok("Token saved to the system keyring.".to_owned()),
        Ok(Err(e)) => Err(format!("Couldn't save to the keyring: {e:#}")),
        Err(e) => Err(format!("Saving crashed: {e}")),
    };
    emitter.emit(Event::AccountMessage(message));
    check_auth(shared).await;
}

async fn forget_token(shared: &Shared) {
    let message = match spawn_blocking(token::forget_from_keyring).await {
        Ok(Ok(())) => Ok("Saved token removed from the keyring.".to_owned()),
        Ok(Err(e)) => Err(format!("Couldn't remove the token: {e:#}")),
        Err(e) => Err(format!("Removing crashed: {e}")),
    };
    shared.emitter.emit(Event::AccountMessage(message));
    check_auth(shared).await;
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::Instant;

    use super::*;
    use crate::jobs::TargetStatus;

    fn wait_for<T>(backend: &Backend, what: &str, mut pick: impl FnMut(Event) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            match backend.try_recv() {
                Some(event) => {
                    if let Some(found) = pick(event) {
                        return found;
                    }
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("timed out waiting for {what}");
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    /// Clones three repos through the backend, using local folders in place of GitHub:
    /// a linked fork (needs `~2` and an upstream remote), a plain repo, and one that fails.
    #[test]
    fn clone_into_the_layout_end_to_end() {
        use std::collections::BTreeSet;

        use crate::db::{FetchedRepo, RepoMeta, Source};

        let base = std::env::temp_dir().join(format!("depository-clone-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let sources = base.join("sources");
        let root = base.join("code");
        let source = |name: &str| {
            let dir = sources.join(name);
            fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q", "-b", "main"]);
            fs::write(dir.join("README"), name).unwrap();
            git(&dir, &["add", "README"]);
            git(
                &dir,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "first",
                ],
            );
            dir.display().to_string()
        };
        let (fork_url, parent_url, tool_url) = (
            source("me-react"),
            source("facebook-react"),
            source("me-tool"),
        );
        // Something already sits at r/react.
        fs::create_dir_all(root.join("r/react")).unwrap();
        git(&root.join("r/react"), &["init", "-q"]);

        let paths = Paths {
            config_file: base.join("config.toml"),
            db_file: base.join("data/depository.db"),
            workspaces_dir: base.join("data/workspaces"),
        };
        {
            let meta = |node: &str, full: &str, url: &str| {
                let (owner, name) = full.split_once('/').unwrap();
                RepoMeta {
                    node_id: node.into(),
                    owner: owner.into(),
                    name: name.into(),
                    ssh_url: Some(url.into()),
                    ..Default::default()
                }
            };
            let owned = BTreeSet::from([Source::Owned]);
            let mut db = Db::open(&paths.db_file).unwrap();
            db.apply_sync(&[
                FetchedRepo {
                    meta: meta("F", "me/react", &fork_url),
                    parent: Some(meta("P", "facebook/react", &parent_url)),
                    sources: owned.clone(),
                },
                FetchedRepo {
                    meta: meta("T", "me/tool", &tool_url),
                    parent: None,
                    sources: owned.clone(),
                },
                FetchedRepo {
                    meta: meta(
                        "B",
                        "me/broken",
                        &sources.join("missing").display().to_string(),
                    ),
                    parent: None,
                    sources: owned,
                },
            ])
            .unwrap();
            let family = db.catalog().unwrap().forks[0].clone();
            db.set_fork_decision(
                family.forks[0].fork_id,
                family.parent_id,
                ForkDecision::Linked,
            )
            .unwrap();
        }

        let config = Config {
            clone_root: Some(root.clone()),
            runner: crate::config::Runner {
                shell: Some("/bin/sh".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let backend = Backend::start(egui::Context::default(), paths, config);
        let catalog = wait_for(&backend, "the inbox", |event| match event {
            Event::Catalog(catalog) if catalog.inbox.len() == 3 => Some(catalog),
            _ => None,
        });
        let ids: Vec<i64> = catalog.inbox.iter().map(|r| r.id).collect();

        backend.send(Command::Clone(ids.clone()));
        let run = wait_for(&backend, "the clone run", |event| match event {
            Event::RunStarted { run, .. } => Some(run),
            _ => None,
        });
        assert_eq!(run.title, "Clone 3 repos");
        let finished = wait_for(&backend, "the clones to finish", |event| match event {
            Event::ClonesFinished(done) => Some(done),
            _ => None,
        });
        assert_eq!(finished, ids);
        let summary = run.summary();
        assert_eq!((summary.succeeded, summary.failed), (2, 1));

        let scan = wait_for(&backend, "the rescan", |event| match event {
            Event::Scan(Ok(scan)) => Some(scan),
            _ => None,
        });
        let paths: Vec<&str> = scan.clones.iter().map(|c| c.rel_path.as_str()).collect();
        assert_eq!(paths, ["r/react", "r/react~2", "t/tool"]);
        assert!(!root.join("b/broken").exists());
        let fork = &scan.clones[1];
        let remotes: Vec<(&str, &str)> = fork
            .remotes
            .iter()
            .map(|r| (r.name.as_str(), r.url.as_str()))
            .collect();
        assert_eq!(
            remotes,
            [("me", fork_url.as_str()), ("upstream", parent_url.as_str())]
        );

        // The failed one is still waiting in the Inbox. (The other two can't be matched back
        // to their GitHub rows here, since their remotes are local paths; the database tests
        // cover that matching.)
        let catalog = wait_for(&backend, "the inbox after cloning", |event| match event {
            Event::Catalog(catalog) => Some(catalog),
            _ => None,
        });
        assert!(catalog.inbox.iter().any(|r| r.full_name() == "me/broken"));

        drop(backend);
        let _ = fs::remove_dir_all(&base);
    }

    /// Imports repos from outside the clone root (renaming origin, repairing a worktree), then
    /// fixes a misplaced clone inside it, checking the clone keeps its database row.
    #[test]
    fn import_end_to_end() {
        let base =
            std::env::temp_dir().join(format!("depository-import-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("code");
        let old = base.join("old");
        let commit = |dir: &Path| {
            fs::write(dir.join("README"), "x").unwrap();
            git(dir, &["add", "README"]);
            git(
                dir,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "x",
                ],
            );
        };
        let repo = |dir: &Path, remotes: &[(&str, &str)]| {
            fs::create_dir_all(dir).unwrap();
            git(dir, &["init", "-q", "-b", "main"]);
            for (name, url) in remotes {
                git(dir, &["remote", "add", name, url]);
            }
        };
        repo(
            &old.join("react"),
            &[("origin", "https://github.com/facebook/react")],
        );
        repo(&old.join("notes"), &[]);
        repo(
            &old.join("withtree"),
            &[("origin", "git@github.com:me/withtree.git")],
        );
        commit(&old.join("withtree"));
        git(
            &old.join("withtree"),
            &["worktree", "add", "-q", "../withtree-wt", "-b", "feature"],
        );
        repo(
            &root.join("x/serde"),
            &[("me", "git@github.com:serde-rs/serde.git")],
        );

        let paths = Paths {
            config_file: base.join("config.toml"),
            db_file: base.join("data/depository.db"),
            workspaces_dir: base.join("data/workspaces"),
        };
        let config = Config {
            clone_root: Some(root.clone()),
            ..Default::default()
        };
        let backend = Backend::start(egui::Context::default(), paths, config);
        let first = wait_for(&backend, "the first scan", |event| match event {
            Event::Scan(Ok(scan)) => Some(scan),
            _ => None,
        });
        let serde_id = first.clones[0].id;

        // Import from outside.
        backend.send(Command::PreviewImport {
            folder: Some(old.clone()),
            paths: vec![],
        });
        let preview = wait_for(&backend, "the preview", |event| match event {
            Event::ImportPreview(result) => Some(result.expect("preview failed")),
            _ => None,
        });
        let planned: Vec<(&str, &str)> = preview
            .items
            .iter()
            .map(|i| (i.name.as_str(), i.destination.as_str()))
            .collect();
        assert_eq!(
            planned,
            [
                ("notes", "n/notes"),
                ("facebook/react", "r/react"),
                ("me/withtree", "w/withtree")
            ]
        );
        assert_eq!(preview.skipped.len(), 1, "the linked worktree is skipped");
        let moves: Vec<ImportMove> = preview
            .items
            .iter()
            .map(|i| ImportMove {
                source: i.source.clone(),
                destination: i.destination.clone(),
                rename_origin: i.can_rename_origin,
            })
            .collect();
        backend.send(Command::Import(moves));
        let run = wait_for(&backend, "the move run", |event| match event {
            Event::RunStarted { run, .. } => Some(run),
            _ => None,
        });
        assert_eq!(run.title, "Move 3 repos into place");
        wait_until("the moves to finish", || run.is_finished());
        assert_eq!(
            run.summary().succeeded,
            3,
            "{:?}",
            (0..3)
                .map(|i| run
                    .slot(i)
                    .output
                    .iter()
                    .map(|l| l.text.clone())
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
        let scan = wait_for(&backend, "the rescan", |event| match event {
            Event::Scan(Ok(scan)) => Some(scan),
            _ => None,
        });
        let in_root: Vec<&str> = scan.clones.iter().map(|c| c.rel_path.as_str()).collect();
        assert_eq!(in_root, ["n/notes", "r/react", "w/withtree", "x/serde"]);
        assert!(!old.join("react").exists() && !old.join("notes").exists());
        let react = scan
            .clones
            .iter()
            .find(|c| c.rel_path == "r/react")
            .unwrap();
        assert_eq!(react.remotes[0].name, "me", "origin was renamed");
        // The worktree outside still works: it was repaired to point at the new location.
        git(&old.join("withtree-wt"), &["status", "--short"]);

        // Fix the misplaced clone inside the root.
        backend.send(Command::PreviewImport {
            folder: None,
            paths: vec![root.join("x/serde")],
        });
        let preview = wait_for(&backend, "the second preview", |event| match event {
            Event::ImportPreview(result) => Some(result.expect("preview failed")),
            _ => None,
        });
        assert_eq!(preview.items[0].destination, "s/serde");
        backend.send(Command::Import(vec![ImportMove {
            source: root.join("x/serde"),
            destination: "s/serde".into(),
            rename_origin: false,
        }]));
        let scan = wait_for(&backend, "the rescan after fixing", |event| match event {
            Event::Scan(Ok(scan)) if scan.clones.iter().any(|c| c.rel_path == "s/serde") => {
                Some(scan)
            }
            _ => None,
        });
        let serde = scan
            .clones
            .iter()
            .find(|c| c.rel_path == "s/serde")
            .unwrap();
        assert_eq!(serde.id, serde_id, "the moved clone kept its database row");
        assert!(serde.problems.is_empty());

        drop(backend);
        let _ = fs::remove_dir_all(&base);
    }

    /// Tag and rule commands reach the database and come back as fresh tags; bad input is refused.
    #[test]
    fn tag_commands_end_to_end() {
        use crate::tags::RuleField;

        let base = std::env::temp_dir().join(format!("depository-tags-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("code");
        fs::create_dir_all(root.join("a/api")).unwrap();
        git(&root.join("a/api"), &["init", "-q"]);
        let paths = Paths {
            config_file: base.join("config.toml"),
            db_file: base.join("data/depository.db"),
            workspaces_dir: base.join("data/workspaces"),
        };
        let config = Config {
            clone_root: Some(root),
            ..Default::default()
        };
        let backend = Backend::start(egui::Context::default(), paths, config);
        let scan = wait_for(&backend, "the scan", |event| match event {
            Event::Scan(Ok(scan)) => Some(scan),
            _ => None,
        });
        let id = scan.clones[0].id;

        backend.send(Command::AddTag {
            clone_ids: vec![id],
            tag: " Work Stuff ".into(),
        });
        let store = wait_for(&backend, "the tag", |event| match event {
            Event::Tags(store) if !store.manual.is_empty() => Some(store),
            _ => None,
        });
        assert!(store.manual[&id].contains("work-stuff"));

        backend.send(Command::AddTag {
            clone_ids: vec![id],
            tag: "and".into(),
        });
        let error = wait_for(&backend, "the refusal", |event| match event {
            Event::TagActionFailed(e) => Some(e),
            _ => None,
        });
        assert!(error.contains("reserved"));

        backend.send(Command::SaveRule(Rule {
            pattern: "(".into(),
            tag_template: "x".into(),
            ..Rule::new(RuleField::Name)
        }));
        let error = wait_for(&backend, "the bad rule", |event| match event {
            Event::TagActionFailed(e) => Some(e),
            _ => None,
        });
        assert!(error.contains("regex"));

        backend.send(Command::SaveRule(Rule {
            pattern: "^a".into(),
            tag_template: "starts-with-a".into(),
            ..Rule::new(RuleField::Name)
        }));
        let store = wait_for(&backend, "the saved rule", |event| match event {
            Event::Tags(store) if !store.rules.is_empty() => Some(store),
            _ => None,
        });
        let rule_id = store.rules[0].id;
        backend.send(Command::SetRuleException {
            rule_id,
            clone_id: id,
            excepted: true,
        });
        let store = wait_for(&backend, "the exception", |event| match event {
            Event::Tags(store)
                if store
                    .rules
                    .first()
                    .is_some_and(|r| !r.exceptions.is_empty()) =>
            {
                Some(store)
            }
            _ => None,
        });
        assert!(store.rules[0].exceptions.contains(&id));

        drop(backend);
        let _ = fs::remove_dir_all(&base);
    }

    /// Opens repos with a fake "editor" script that records its arguments.
    #[test]
    fn launching_end_to_end() {
        use std::os::unix::fs::PermissionsExt;

        use crate::launch::{LauncherSpec, WorkspaceKind};

        let base =
            std::env::temp_dir().join(format!("depository-launch-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let log = base.join("calls.log");
        let editor = base.join("fake-editor");
        fs::write(
            &editor,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\n", log.display()),
        )
        .unwrap();
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).unwrap();
        let broken = base.join("broken-editor");
        fs::write(&broken, "#!/bin/sh\nexit 3\n").unwrap();
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o755)).unwrap();

        let spec = |name: &str, program: &PathBuf| LauncherSpec {
            name: name.into(),
            programs: vec![program.display().to_string()],
            flatpak: None,
            requires: Vec::new(),
            each: Some("--one {path}".into()),
            together: Some("--ws {workspace}".into()),
            workspace: WorkspaceKind::VsCode,
        };
        let paths = Paths {
            config_file: base.join("config.toml"),
            db_file: base.join("data/depository.db"),
            workspaces_dir: base.join("data/workspaces"),
        };
        let mut config = Config::default();
        config.launch.custom = vec![spec("Fake", &editor), spec("Broken", &broken)];
        let backend = Backend::start(egui::Context::default(), paths, config);
        let launchers = wait_for(&backend, "the launchers", |event| match event {
            Event::Launchers(list) => Some(list),
            _ => None,
        });
        assert_eq!(&launchers[0].name, "Fake");
        assert!(launchers[0].each && launchers[0].together);

        let targets = vec![
            ("me/api".to_owned(), PathBuf::from("/code/a/api")),
            ("me/web".to_owned(), PathBuf::from("/code/w/web")),
        ];
        let request = |launcher: &str, together: bool| {
            Command::Launch(LaunchRequest {
                launcher: launcher.into(),
                together,
                targets: targets.clone(),
                workspace_name: "lang:rust".into(),
                origin: Origin::Tags,
            })
        };
        backend.send(request("Fake", false));
        backend.send(request("Fake", true));
        wait_until("three launches", || {
            fs::read_to_string(&log).is_ok_and(|text| text.lines().count() == 3)
        });
        let mut calls: Vec<String> = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        calls.sort();
        let workspace = base.join("data/workspaces/lang-rust.code-workspace");
        assert_eq!(
            calls,
            [
                "--one /code/a/api".to_owned(),
                "--one /code/w/web".to_owned(),
                format!("--ws {}", workspace.display()),
            ]
        );
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&workspace).unwrap()).unwrap();
        assert_eq!(json["folders"][1]["name"], "me/web");

        backend.send(request("Broken", false));
        let failure = wait_for(&backend, "the failure", |event| match event {
            Event::LaunchFailed { origin, message } => Some((origin, message)),
            _ => None,
        });
        assert_eq!(failure.0, Origin::Tags);
        assert!(
            failure
                .1
                .contains("broken-editor stopped with exit status: 3"),
            "{}",
            failure.1
        );

        backend.send(request("Nope", false));
        // (The broken editor fails once per repo, so skip its second report.)
        let missing = wait_for(&backend, "the missing launcher", |event| match event {
            Event::LaunchFailed { message, .. } if message.contains("isn't installed") => {
                Some(message)
            }
            _ => None,
        });
        assert!(missing.contains("isn't installed"));

        drop(backend);
        let _ = fs::remove_dir_all(&base);
    }

    /// Scans a real clone root through the backend, runs `git status --short` in every clone,
    /// then starts a slow command everywhere and cancels it.
    #[test]
    fn scan_run_and_cancel_end_to_end() {
        let base = std::env::temp_dir().join(format!("depository-backend-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("code");
        for (rel, dirty) in [("a/alpha", true), ("b/beta", false), ("g/gamma", true)] {
            let dir = root.join(rel);
            fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q", "-b", "main"]);
            if dirty {
                fs::write(dir.join("depository-untracked-9d1e"), "x").unwrap();
            }
        }
        let paths = Paths {
            config_file: base.join("config.toml"),
            db_file: base.join("data/depository.db"),
            workspaces_dir: base.join("data/workspaces"),
        };
        let config = Config {
            clone_root: Some(root.clone()),
            runner: crate::config::Runner {
                shell: Some("/bin/sh".into()),
                ..Default::default()
            },
            ..Default::default()
        };

        let backend = Backend::start(egui::Context::default(), paths, config);
        let scan = wait_for(&backend, "the startup scan", |event| match event {
            Event::Scan(result) => Some(result.expect("scan failed")),
            _ => None,
        });
        assert_eq!(scan.clones.len(), 3);
        let targets: Vec<Target> = scan
            .clones
            .iter()
            .map(|c| Target {
                name: c.display_name(),
                path: c.abs_path.clone(),
                tags: Vec::new(),
                env: Vec::new(),
            })
            .collect();

        backend.send(Command::StartRun {
            request: 1,
            command: "git status --short".into(),
            targets: targets.clone(),
            parallel: 2,
            colors: false,
        });
        let run = wait_for(&backend, "the run to start", |event| match event {
            Event::RunStarted { run, .. } => Some(run),
            _ => None,
        });
        assert_eq!(run.shell, "/bin/sh");
        wait_until("the run to finish", || run.is_finished());
        let summary = run.summary();
        assert_eq!((summary.succeeded, summary.failed), (3, 0));
        let outputs: Vec<Vec<String>> = (0..3)
            .map(|i| run.slot(i).output.iter().map(|l| l.text.clone()).collect())
            .collect();
        assert_eq!(outputs[0], ["?? depository-untracked-9d1e"]);
        assert!(outputs[1].is_empty());
        assert_eq!(outputs[2], ["?? depository-untracked-9d1e"]);
        // Finishing a run refreshes the status column and records the command (in either order).
        let (mut rescanned, mut recorded) = (false, None);
        let history = wait_for(&backend, "the rescan and the history", |event| {
            match event {
                Event::Scan(_) => rescanned = true,
                Event::History(list) if !list.is_empty() => recorded = Some(list),
                _ => {}
            }
            if rescanned { recorded.clone() } else { None }
        });
        assert_eq!(history[0].command, "git status --short");
        assert_eq!((history[0].repo_count, history[0].ok_count), (3, 3));

        backend.send(Command::StartRun {
            request: 2,
            command: "sleep 30".into(),
            targets,
            parallel: 8,
            colors: false,
        });
        let slow = wait_for(&backend, "the slow run to start", |event| match event {
            Event::RunStarted { run, .. } => Some(run),
            _ => None,
        });
        wait_until("all three to be running", || slow.summary().running == 3);
        let cancelled_at = Instant::now();
        slow.cancel();
        wait_until("the slow run to stop", || slow.is_finished());
        assert!(cancelled_at.elapsed() < Duration::from_secs(5));
        assert!((0..3).all(|i| matches!(slow.status(i), TargetStatus::Cancelled { .. })));

        drop(backend);
        let _ = fs::remove_dir_all(&base);
    }
}
