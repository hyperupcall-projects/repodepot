//! Background work. The UI sends [`Command`]s; the backend answers with [`Event`]s and asks
//! egui to repaint. Slow work (network, keyring, database, processes) never runs on the UI thread.

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;

use eframe::egui;
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;

use crate::config::Paths;
use crate::db::Db;
use crate::github::{self, RateLimit, Viewer};
use crate::token::{self, TokenSource};

pub enum Command {
    CheckAuth,
    /// Check a pasted token with GitHub, then save it to the keyring if it works.
    SaveToken(String),
    ForgetToken,
}

pub enum Event {
    Db(Result<DbInfo, String>),
    Auth(AuthState),
    /// Outcome of saving or forgetting the token, for the Settings page.
    AccountMessage(Result<String, String>),
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
    pub fn start(ctx: egui::Context, paths: Paths) -> Self {
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
                runtime.block_on(run(command_rx, emitter, paths));
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

async fn run(mut commands: mpsc::UnboundedReceiver<Command>, emitter: Emitter, paths: Paths) {
    // Held for the backend's lifetime; later steps run their queries through it.
    let _db = open_db(&paths, &emitter).await;

    let http = github::http_client().map_err(|e| format!("{e:#}"));
    check_auth(&http, &emitter).await;

    while let Some(command) = commands.recv().await {
        match command {
            Command::CheckAuth => check_auth(&http, &emitter).await,
            Command::SaveToken(token) => save_token(&http, &emitter, token).await,
            Command::ForgetToken => forget_token(&http, &emitter).await,
        }
    }
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

async fn check_auth(http: &Result<reqwest::Client, String>, emitter: &Emitter) {
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
    let state = match (lookup.found, http) {
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

async fn save_token(http: &Result<reqwest::Client, String>, emitter: &Emitter, token: String) {
    let token = token.trim().to_owned();
    let client = match http {
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
    check_auth(http, emitter).await;
}

async fn forget_token(http: &Result<reqwest::Client, String>, emitter: &Emitter) {
    let message = match spawn_blocking(token::forget_from_keyring).await {
        Ok(Ok(())) => Ok("Saved token removed from the keyring.".to_owned()),
        Ok(Err(e)) => Err(format!("Couldn't remove the token: {e:#}")),
        Err(e) => Err(format!("Removing crashed: {e}")),
    };
    emitter.emit(Event::AccountMessage(message));
    check_auth(http, emitter).await;
}
