//! The SQLite database in `~/.local/share/depository/depository.db`.

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

/// Schema changes, oldest first. Never edit one that has shipped; add a new one instead.
const MIGRATIONS: &[M<'static>] = &[M::up(include_str!("migrations/001_initial.sql"))];

pub fn migrations() -> Migrations<'static> {
    Migrations::from_slice(MIGRATIONS)
}

pub struct Db {
    conn: Connection,
}

impl Db {
    /// Opens (creating if needed) the database and brings its schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("couldn't open {}", path.display()))?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
            .context("couldn't switch the database to WAL mode")?;
        Self::prepare(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        migrations()
            .to_latest(&mut conn)
            .context("couldn't update the database schema")?;
        Ok(Self { conn })
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_valid() {
        migrations().validate().unwrap();
    }

    #[test]
    fn fresh_database_gets_latest_schema() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
        let snippets: i64 = db
            .conn
            .query_row("SELECT count(*) FROM snippets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(snippets, 4);
    }

    #[test]
    fn repo_identity_ignores_case() {
        let db = Db::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO repos (host, owner, name) VALUES ('github.com', 'Facebook', 'React')",
                [],
            )
            .unwrap();
        let duplicate = db.conn.execute(
            "INSERT INTO repos (host, owner, name) VALUES ('github.com', 'facebook', 'react')",
            [],
        );
        assert!(duplicate.is_err());
    }
}
