//! Finding the GitHub token: system keyring, then `GITHUB_TOKEN`, then `gh auth token`.
//! The token is never written anywhere else.
//!
//! Everything here blocks (D-Bus, child processes), so call it from `spawn_blocking`.

use std::process::{Command, Stdio};

use anyhow::Result;

const KEYRING_SERVICE: &str = "depository";
const KEYRING_USER: &str = "github-token";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    Keyring,
    EnvVar,
    GhCli,
}

impl TokenSource {
    pub fn describe(self) -> &'static str {
        match self {
            TokenSource::Keyring => "the system keyring",
            TokenSource::EnvVar => "GITHUB_TOKEN",
            TokenSource::GhCli => "the gh CLI",
        }
    }
}

pub struct Found {
    pub token: String,
    pub source: TokenSource,
}

pub struct Lookup {
    pub found: Option<Found>,
    /// Set when the keyring couldn't be read at all (as opposed to having no saved token).
    pub keyring_error: Option<String>,
}

fn entry() -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
}

pub fn find() -> Lookup {
    let mut keyring_error = None;
    match entry().and_then(|e| e.get_password()) {
        Ok(token) if !token.trim().is_empty() => {
            return Lookup {
                found: Some(Found {
                    token: token.trim().to_owned(),
                    source: TokenSource::Keyring,
                }),
                keyring_error: None,
            };
        }
        Ok(_) | Err(keyring::Error::NoEntry) => {}
        Err(e) => keyring_error = Some(format!("Couldn't read the system keyring: {e}")),
    }

    if let Ok(token) = std::env::var("GITHUB_TOKEN")
        && !token.trim().is_empty()
    {
        return Lookup {
            found: Some(Found {
                token: token.trim().to_owned(),
                source: TokenSource::EnvVar,
            }),
            keyring_error,
        };
    }

    let found = gh_cli_token().map(|token| Found {
        token,
        source: TokenSource::GhCli,
    });
    Lookup {
        found,
        keyring_error,
    }
}

fn gh_cli_token() -> Option<String> {
    let output = Command::new("gh")
        .args(["auth", "token", "--hostname", "github.com"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

pub fn save_to_keyring(token: &str) -> Result<()> {
    entry()?.set_password(token)?;
    Ok(())
}

pub fn forget_from_keyring() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
