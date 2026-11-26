//! `repodepot --check`: prints where things live and whether the database and GitHub
//! sign-in work, without opening a window.

use anyhow::Result;

use crate::config::{self, Paths};
use crate::db::Db;
use crate::github;
use crate::token;
use crate::ui::thousands;

pub fn run(paths: &Paths) -> Result<()> {
    println!("Config file:  {}", paths.config_file.display());
    let (config, error) = config::load(&paths.config_file);
    match error {
        Some(e) => println!("  problem: {e}"),
        None => match &config.clone_root {
            Some(root) => println!("  ok, clone root is {}", root.display()),
            None => println!("  ok, clone root isn't set yet"),
        },
    }

    println!("Database:     {}", paths.db_file.display());
    match Db::open(&paths.db_file).and_then(|db| db.schema_version()) {
        Ok(version) => println!("  ok, schema v{version}"),
        Err(e) => println!("  problem: {e:#}"),
    }

    let lookup = token::find();
    if let Some(e) = &lookup.keyring_error {
        println!("Keyring:      {e}");
    }
    match lookup.found {
        None => println!("GitHub:       no token found"),
        Some(found) => {
            println!("GitHub:       token from {}", found.source.describe());
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let result = runtime.block_on(async {
                let http = github::http_client()?;
                github::whoami(&http, &found.token).await
            });
            match result {
                Ok((viewer, rate)) => println!(
                    "  signed in as {}, {} of {} API points left (resets at {})",
                    viewer.login,
                    thousands(rate.remaining),
                    thousands(rate.limit),
                    rate.reset_at
                ),
                Err(e) => println!("  problem: {e:#}"),
            }
        }
    }
    Ok(())
}
