//! The SQLite database in `~/.local/share/depository/depository.db`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use rusqlite_migration::{M, Migrations};

use crate::git::remote_url::RemoteUrl;

/// Schema changes, oldest first. Never edit one that has shipped; add a new one instead.
const MIGRATIONS: &[M<'static>] = &[
    M::up(include_str!("migrations/001_initial.sql")),
    M::up(include_str!("migrations/002_clone_scan.sql")),
];

pub fn migrations() -> Migrations<'static> {
    Migrations::from_slice(MIGRATIONS)
}

pub struct Db {
    conn: Connection,
}

/// One clone found by a scan, as the database needs it.
pub struct ScannedClone<'a> {
    /// Relative to the clone root, e.g. `r/react~2`.
    pub path: &'a str,
    pub remotes: Vec<ScannedRemote<'a>>,
    /// Index into `remotes`.
    pub main_remote: Option<usize>,
}

pub struct ScannedRemote<'a> {
    pub name: &'a str,
    pub url: &'a str,
    pub parsed: Option<&'a RemoteUrl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingClone {
    pub id: i64,
    pub path: String,
    /// `owner/name` of its main remote's repo, if it had one.
    pub name: Option<String>,
    /// UTC, `YYYY-MM-DD HH:MM:SS`.
    pub since: String,
}

pub struct ScanApplied {
    /// Clone IDs, in the same order as the scanned clones.
    pub ids: Vec<i64>,
    pub missing: Vec<MissingClone>,
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

    /// Main remotes picked by hand, keyed by clone path.
    pub fn main_remote_picks(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, main_remote FROM clones WHERE main_remote IS NOT NULL")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records what a scan found:
    /// - new folders get a row; known ones get their remotes refreshed
    /// - a new folder whose main repo matches exactly one vanished row is treated as that
    ///   row moved, so its tags and choices follow it
    /// - rows whose folder wasn't found are marked missing (never deleted here)
    pub fn apply_scan(&mut self, scanned: &[ScannedClone<'_>]) -> Result<ScanApplied> {
        let tx = self.conn.transaction()?;

        let mut remote_repo_ids: Vec<Vec<Option<i64>>> = Vec::with_capacity(scanned.len());
        for clone in scanned {
            let mut ids = Vec::with_capacity(clone.remotes.len());
            for remote in &clone.remotes {
                ids.push(match remote.parsed {
                    Some(parsed) => Some(upsert_repo(&tx, parsed)?),
                    None => None,
                });
            }
            remote_repo_ids.push(ids);
        }
        let main_repo_ids: Vec<Option<i64>> = scanned
            .iter()
            .zip(&remote_repo_ids)
            .map(|(clone, ids)| clone.main_remote.and_then(|i| ids[i]))
            .collect();

        let existing: Vec<(i64, String, Option<i64>)> = {
            let mut stmt = tx.prepare("SELECT id, path, repo_id FROM clones")?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let id_by_path: HashMap<&str, i64> =
            existing.iter().map(|(id, path, _)| (path.as_str(), *id)).collect();
        let found_paths: HashSet<&str> = scanned.iter().map(|c| c.path).collect();
        let mut vanished: Vec<(i64, Option<i64>)> = existing
            .iter()
            .filter(|(_, path, _)| !found_paths.contains(path.as_str()))
            .map(|(id, _, repo_id)| (*id, *repo_id))
            .collect();

        // How many new folders claim each repo; a move is only assumed when exactly one does.
        let mut new_folders_per_repo: HashMap<i64, usize> = HashMap::new();
        for (clone, main_repo) in scanned.iter().zip(&main_repo_ids) {
            if !id_by_path.contains_key(clone.path)
                && let Some(repo) = main_repo
            {
                *new_folders_per_repo.entry(*repo).or_default() += 1;
            }
        }

        let mut ids = Vec::with_capacity(scanned.len());
        for (i, clone) in scanned.iter().enumerate() {
            let main_repo = main_repo_ids[i];
            let id = if let Some(&id) = id_by_path.get(clone.path) {
                id
            } else if let Some(position) = main_repo
                .filter(|repo| new_folders_per_repo.get(repo) == Some(&1))
                .and_then(|repo| unique_position(&vanished, |(_, r)| *r == Some(repo)))
            {
                let (id, _) = vanished.remove(position);
                tx.execute(
                    "UPDATE clones SET path = ?1 WHERE id = ?2",
                    params![clone.path, id],
                )?;
                id
            } else {
                tx.execute("INSERT INTO clones (path) VALUES (?1)", params![clone.path])?;
                tx.last_insert_rowid()
            };

            tx.execute(
                "UPDATE clones SET repo_id = ?1, missing_since = NULL WHERE id = ?2",
                params![main_repo, id],
            )?;
            tx.execute("DELETE FROM clone_remotes WHERE clone_id = ?1", params![id])?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO clone_remotes (clone_id, remote_name, url, repo_id) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (remote, repo_id) in clone.remotes.iter().zip(&remote_repo_ids[i]) {
                insert.execute(params![id, remote.name, remote.url, repo_id])?;
            }
            ids.push(id);
        }

        for (id, _) in &vanished {
            tx.execute(
                "UPDATE clones SET missing_since = COALESCE(missing_since, datetime('now')) WHERE id = ?1",
                params![id],
            )?;
        }
        let missing = missing_clones(&tx)?;
        tx.commit()?;
        Ok(ScanApplied { ids, missing })
    }

    /// Sets (or with `None`, clears) the main remote picked by hand.
    pub fn set_main_remote(&mut self, clone_id: i64, remote: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE clones SET main_remote = ?1 WHERE id = ?2",
            params![remote, clone_id],
        )?;
        Ok(())
    }

    /// Removes a missing clone's row, along with its tags and rule exceptions.
    /// Refuses rows whose folder is still there.
    pub fn forget_missing_clone(&mut self, clone_id: i64) -> Result<()> {
        let deleted = self.conn.execute(
            "DELETE FROM clones WHERE id = ?1 AND missing_since IS NOT NULL",
            params![clone_id],
        )?;
        anyhow::ensure!(deleted == 1, "that clone isn't marked missing");
        Ok(())
    }
}

fn upsert_repo(tx: &Transaction<'_>, repo: &RemoteUrl) -> Result<i64> {
    tx.prepare_cached(
        "INSERT INTO repos (host, owner, name) VALUES (?1, ?2, ?3)
         ON CONFLICT (host, owner, name) DO NOTHING",
    )?
    .execute(params![repo.host, repo.owner, repo.name])?;
    let id = tx
        .prepare_cached("SELECT id FROM repos WHERE host = ?1 AND owner = ?2 AND name = ?3")?
        .query_row(params![repo.host, repo.owner, repo.name], |row| row.get(0))
        .optional()?
        .context("a repo row vanished right after it was written")?;
    Ok(id)
}

fn missing_clones(tx: &Transaction<'_>) -> Result<Vec<MissingClone>> {
    let mut stmt = tx.prepare(
        "SELECT c.id, c.path, r.owner, r.name, c.missing_since
         FROM clones c LEFT JOIN repos r ON r.id = c.repo_id
         WHERE c.missing_since IS NOT NULL
         ORDER BY c.path",
    )?;
    let rows = stmt.query_map([], |row| {
        let owner: Option<String> = row.get(2)?;
        let name: Option<String> = row.get(3)?;
        Ok(MissingClone {
            id: row.get(0)?,
            path: row.get(1)?,
            name: owner.zip(name).map(|(o, n)| format!("{o}/{n}")),
            since: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn unique_position<T>(items: &[T], predicate: impl Fn(&T) -> bool) -> Option<usize> {
    let mut found = None;
    for (i, item) in items.iter().enumerate() {
        if predicate(item) {
            if found.is_some() {
                return None;
            }
            found = Some(i);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::remote_url::GITHUB;

    fn repo(owner: &str, name: &str) -> RemoteUrl {
        RemoteUrl {
            host: GITHUB.into(),
            owner: owner.into(),
            name: name.into(),
        }
    }

    fn clone<'a>(path: &'a str, main: Option<&'a RemoteUrl>) -> ScannedClone<'a> {
        ScannedClone {
            path,
            remotes: main
                .map(|parsed| ScannedRemote {
                    name: "me",
                    url: "git@github.com:x/y.git",
                    parsed: Some(parsed),
                })
                .into_iter()
                .collect(),
            main_remote: main.map(|_| 0),
        }
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn migrations_are_valid() {
        migrations().validate().unwrap();
    }

    #[test]
    fn fresh_database_gets_latest_schema() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
        assert_eq!(count(&db, "SELECT count(*) FROM snippets"), 4);
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

    #[test]
    fn scan_inserts_clones_remotes_and_repos() {
        let mut db = Db::open_in_memory().unwrap();
        let react = repo("facebook", "react");
        let applied = db
            .apply_scan(&[clone("r/react", Some(&react)), clone("l/local", None)])
            .unwrap();
        assert_eq!(applied.ids.len(), 2);
        assert!(applied.missing.is_empty());
        assert_eq!(count(&db, "SELECT count(*) FROM clones"), 2);
        assert_eq!(count(&db, "SELECT count(*) FROM clone_remotes"), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM repos"), 1);
        assert_eq!(
            count(&db, "SELECT count(*) FROM clones WHERE repo_id IS NOT NULL"),
            1
        );

        // A second scan keeps the same IDs and doesn't duplicate anything.
        let again = db
            .apply_scan(&[clone("r/react", Some(&react)), clone("l/local", None)])
            .unwrap();
        assert_eq!(again.ids, applied.ids);
        assert_eq!(count(&db, "SELECT count(*) FROM clone_remotes"), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM repos"), 1);
    }

    #[test]
    fn vanished_folders_are_marked_missing_then_forgotten() {
        let mut db = Db::open_in_memory().unwrap();
        let react = repo("facebook", "react");
        let first = db.apply_scan(&[clone("r/react", Some(&react))]).unwrap();
        let applied = db.apply_scan(&[]).unwrap();
        assert_eq!(applied.missing.len(), 1);
        assert_eq!(applied.missing[0].id, first.ids[0]);
        assert_eq!(applied.missing[0].name.as_deref(), Some("facebook/react"));

        // It comes back: no longer missing, same row.
        let back = db.apply_scan(&[clone("r/react", Some(&react))]).unwrap();
        assert!(back.missing.is_empty());
        assert_eq!(back.ids, first.ids);

        // Forgetting only works while missing.
        assert!(db.forget_missing_clone(first.ids[0]).is_err());
        db.apply_scan(&[]).unwrap();
        db.forget_missing_clone(first.ids[0]).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM clones"), 0);
    }

    #[test]
    fn moved_folder_keeps_its_row_and_tags() {
        let mut db = Db::open_in_memory().unwrap();
        let react = repo("facebook", "react");
        let first = db.apply_scan(&[clone("x/react-old", Some(&react))]).unwrap();
        db.conn
            .execute("INSERT INTO tags (id, name) VALUES (1, 'work')", [])
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO clone_tags (clone_id, tag_id) VALUES (?1, 1)",
                [first.ids[0]],
            )
            .unwrap();

        let moved = db.apply_scan(&[clone("r/react", Some(&react))]).unwrap();
        assert_eq!(moved.ids, first.ids);
        assert!(moved.missing.is_empty());
        assert_eq!(count(&db, "SELECT count(*) FROM clone_tags"), 1);
    }

    #[test]
    fn ambiguous_moves_are_not_guessed() {
        let mut db = Db::open_in_memory().unwrap();
        let react = repo("facebook", "react");
        db.apply_scan(&[clone("x/react-old", Some(&react))]).unwrap();
        // Two new folders for the same repo: can't tell which one is the moved one.
        let applied = db
            .apply_scan(&[clone("r/react", Some(&react)), clone("r/react~2", Some(&react))])
            .unwrap();
        assert_eq!(applied.missing.len(), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM clones"), 3);
    }

    #[test]
    fn main_remote_picks_round_trip() {
        let mut db = Db::open_in_memory().unwrap();
        let ids = db.apply_scan(&[clone("l/local", None)]).unwrap().ids;
        db.set_main_remote(ids[0], Some("fork")).unwrap();
        assert_eq!(
            db.main_remote_picks().unwrap().get("l/local").map(String::as_str),
            Some("fork")
        );
        db.set_main_remote(ids[0], None).unwrap();
        assert!(db.main_remote_picks().unwrap().is_empty());
    }
}
