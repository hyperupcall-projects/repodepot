//! Background work. The UI sends [`Command`]s; the backend answers with [`Event`]s and asks
//! egui to repaint. Slow work (network, keyring, database, processes) never runs on the UI thread.

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard};

use eframe::egui;
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;

use crate::config::{Config, Paths};
use crate::db::Db;
use crate::git::scan::{self, ScanResult, ScanSettings};
use crate::github::{self, RateLimit, Viewer};
use crate::token::{self, TokenSource};

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
    let db = open_db(&paths, &emitter).await.map(|db| Arc::new(Mutex::new(db)));
    let shared = Arc::new(Shared {
        http: github::http_client().map_err(|e| format!("{e:#}")),
        db,
        config: Mutex::new(config),
        emitter,
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
            }
        }
    });

    let _ = auth_tx.send(AuthJob::Check);
    if lock(&shared.config).clone_root.is_some() {
        let _ = scan_tx.send(());
    }

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
                let rescan = {
                    let mut config = lock(&shared.config);
                    let changed = scan_inputs_differ(&config, &new);
                    *config = new;
                    changed
                };
                if rescan {
                    let _ = scan_tx.send(());
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
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn scan_inputs_differ(old: &Config, new: &Config) -> bool {
    old.clone_root != new.clone_root
        || old.remotes.default_name != new.remotes.default_name
        || old.github.ssh_host_aliases != new.github.ssh_host_aliases
}

/// Runs a database write off the async threads. Reports failures to the UI.
async fn write_db(
    shared: &Shared,
    write: impl FnOnce(&mut Db) -> anyhow::Result<()> + Send + 'static,
) -> bool {
    let Some(db) = shared.db.clone() else {
        shared.emitter.emit(Event::RepoActionFailed(
            "The database isn't available.".to_owned(),
        ));
        return false;
    };
    match spawn_blocking(move || write(&mut lock(&db))).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            shared
                .emitter
                .emit(Event::RepoActionFailed(format!("{e:#}")));
            false
        }
        Err(e) => {
            shared
                .emitter
                .emit(Event::RepoActionFailed(format!("crashed: {e}")));
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
