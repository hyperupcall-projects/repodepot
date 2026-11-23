//! The SQLite database in `~/.local/share/depository/depository.db`.

pub mod github;
mod tags;

pub use github::{Catalog, FetchedRepo, ForkDecision, RepoMeta, RepoRow, RepoState, Source};
pub use tags::{RepoFacts, TagStore};

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
    M::up(include_str!("migrations/003_github_sync.sql")),
    M::up(include_str!("migrations/004_repo_stars.sql")),
    M::up(include_str!("migrations/005_repo_tags.sql")),
    M::up(include_str!("migrations/006_fork_defaults.sql")),
];

/// Stamped into every database this app creates (SQLite's `application_id`), so a file
/// written by some other program (even one also called depository) is never mistaken for ours.
const APPLICATION_ID: i32 = 0x4445_504F; // "DEPO"

/// Tables and columns each migration creates; a database without a stamp must have all of
/// them (up to its version) to count as ours.
const EXPECTED: &[(i64, &str, &[&str])] = &[
    (
        1,
        "repos",
        &[
            "host",
            "owner",
            "name",
            "github_node_id",
            "description",
            "topics",
            "ignored",
        ],
    ),
    (1, "clones", &["id", "path", "repo_id"]),
    (
        1,
        "clone_remotes",
        &["clone_id", "remote_name", "url", "repo_id"],
    ),
    (1, "tags", &["id", "name"]),
    (1, "rules", &["field", "pattern", "tag_template"]),
    (1, "snippets", &["name", "command"]),
    (2, "clones", &["main_remote", "missing_since"]),
    (3, "repos", &["not_found_at", "renamed_to_id"]),
    (3, "meta", &["key", "value"]),
    (4, "repos", &["stars"]),
    (5, "repo_tags", &["repo_id", "tag_id"]),
    (6, "fork_links", &["chosen"]),
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub id: i64,
    pub name: String,
    pub command: String,
}

/// A command run from the Runner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub command: String,
    /// UTC `YYYY-MM-DD HH:MM:SS`.
    pub started_at: String,
    pub repo_count: usize,
    pub ok_count: usize,
    pub failed_count: usize,
}

pub struct ScanApplied {
    /// Clone IDs, in the same order as the scanned clones.
    pub ids: Vec<i64>,
    /// The main remote's repo ID for each clone, in the same order.
    pub repo_ids: Vec<Option<i64>>,
    pub missing: Vec<MissingClone>,
}

impl Db {
    /// Opens (creating if needed) the database and brings its schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("couldn't create {}", dir.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("couldn't open {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        // Before anything writes to the file: make sure it's ours.
        check_owner(&conn).map_err(|problem| {
            anyhow::anyhow!(
                "{} {problem} depository hasn't changed it. Move it somewhere else (depository \
                 will start a new one), or give the other program its own folder.",
                path.display()
            )
        })?;
        let db = Self::prepare(conn)?;
        db.conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
            .context("couldn't switch the database to WAL mode")?;
        Ok(db)
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
        conn.pragma_update(None, "application_id", APPLICATION_ID)?;
        Ok(Self { conn })
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    pub fn snippets(&self) -> Result<Vec<Snippet>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, command FROM snippets ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(Snippet {
                id: row.get(0)?,
                name: row.get(1)?,
                command: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_snippet(&mut self, name: &str, command: &str) -> Result<()> {
        anyhow::ensure!(!name.trim().is_empty(), "a snippet needs a name");
        anyhow::ensure!(!command.trim().is_empty(), "a snippet needs a command");
        self.conn.execute(
            "INSERT INTO snippets (name, command) VALUES (?1, ?2)",
            params![name.trim(), command.trim()],
        )?;
        Ok(())
    }

    pub fn delete_snippet(&mut self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM snippets WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Records a finished command. Only the newest 500 are kept.
    pub fn add_history(
        &mut self,
        command: &str,
        took: Duration,
        repos: usize,
        ok: usize,
        failed: usize,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO run_history (command, started_at, repo_count, ok_count, failed_count)
             VALUES (?1, datetime('now', ?2), ?3, ?4, ?5)",
            params![
                command,
                format!("-{} seconds", took.as_secs()),
                repos as i64,
                ok as i64,
                failed as i64
            ],
        )?;
        tx.execute(
            "DELETE FROM run_history WHERE id NOT IN
               (SELECT id FROM run_history ORDER BY id DESC LIMIT 500)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The newest `limit` commands, newest first.
    pub fn history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT command, started_at, repo_count, ok_count, failed_count
             FROM run_history ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(HistoryEntry {
                command: r.get(0)?,
                started_at: r.get(1)?,
                repo_count: r.get::<_, i64>(2)? as usize,
                ok_count: r.get::<_, i64>(3)? as usize,
                failed_count: r.get::<_, i64>(4)? as usize,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
        let id_by_path: HashMap<&str, i64> = existing
            .iter()
            .map(|(id, path, _)| (path.as_str(), *id))
            .collect();
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
        // Which forks have a clone, and with which remotes, may have changed.
        github::apply_fork_defaults(&tx)?;
        tx.commit()?;
        Ok(ScanApplied {
            ids,
            repo_ids: main_repo_ids,
            missing,
        })
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

/// Checks that a database is this app's (or new). On failure, says what's wrong with it.
fn check_owner(conn: &Connection) -> std::result::Result<(), String> {
    let query = |sql: &str| -> std::result::Result<i64, String> {
        conn.query_row(sql, [], |r| r.get(0))
            .map_err(|e| format!("couldn't be read: {e}."))
    };
    let application_id = query("PRAGMA application_id")?;
    if application_id == i64::from(APPLICATION_ID) {
        return Ok(());
    }
    if application_id != 0 {
        return Err(
            "belongs to a different program (its SQLite application ID isn't depository's).".into(),
        );
    }
    let version = query("PRAGMA user_version")?;
    let tables = query("SELECT count(*) FROM sqlite_master WHERE type = 'table'")?;
    if version == 0 && tables == 0 {
        return Ok(()); // brand new
    }
    if version > MIGRATIONS.len() as i64 {
        return Err(format!(
            "is at schema version {version}, newer than this build of depository knows \
             (it knows up to {}).",
            MIGRATIONS.len()
        ));
    }
    // Made before databases were stamped: accept it only if it has every table and column
    // our migrations up to its version create.
    for (since, table, columns) in EXPECTED {
        if *since > version {
            continue;
        }
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .map_err(|e| format!("couldn't be read: {e}."))?;
        let present: Vec<String> = stmt
            .query_map([table], |r| r.get(0))
            .and_then(|rows| rows.collect())
            .map_err(|e| format!("couldn't be read: {e}."))?;
        if let Some(missing) = columns.iter().find(|c| !present.iter().any(|p| p == *c)) {
            return Err(format!(
                "was written by a different program, maybe another app also called depository: \
                 its {table} table has no {missing} column, so its layout isn't this app's."
            ));
        }
    }
    Ok(())
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
        let snippets = db.snippets().unwrap();
        assert_eq!(snippets.len(), 4);
        assert_eq!(snippets[2].command, "git status --short");
    }

    fn temp_db(test: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("depository-owner-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("depository.db")
    }

    #[test]
    fn new_databases_are_stamped_and_reopen() {
        let path = temp_db("new");
        drop(Db::open(&path).unwrap());
        let db = Db::open(&path).unwrap();
        let id: i32 = db
            .conn
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .unwrap();
        assert_eq!(id, APPLICATION_ID);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn unstamped_databases_of_ours_are_accepted() {
        // Like a database from before stamping: our migrations, no application ID.
        let path = temp_db("legacy");
        {
            let mut conn = Connection::open(&path).unwrap();
            Migrations::from_slice(&MIGRATIONS[..1])
                .to_latest(&mut conn)
                .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(
            db.schema_version().unwrap(),
            MIGRATIONS.len() as i64,
            "migrated forward"
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn undecided_forks_get_their_default_when_migrated() {
        let mut conn = Connection::open_in_memory().unwrap();
        Migrations::from_slice(&MIGRATIONS[..5])
            .to_latest(&mut conn)
            .unwrap();
        conn.execute_batch(
            "INSERT INTO repos (id, host, owner, name) VALUES
                 (1, 'github.com', 'up', 'a'), (2, 'github.com', 'me', 'a'),
                 (3, 'github.com', 'me', 'b'), (4, 'github.com', 'me', 'c'),
                 (5, 'github.com', 'me', 'd');
             INSERT INTO fork_links (fork_repo_id, parent_repo_id, decision) VALUES
                 (2, 1, 'undecided'), (3, 1, 'undecided'), (4, 1, 'separate'), (5, 1, 'linked');
             -- me/a is cloned with no remote for its parent; me/b has one.
             INSERT INTO clones (id, path, repo_id) VALUES (1, 'a/a', 2), (2, 'b/b', 3);
             INSERT INTO clone_remotes (clone_id, remote_name, url, repo_id) VALUES
                 (1, 'origin', 'x', 2), (2, 'origin', 'y', 3), (2, 'upstream', 'z', 1);",
        )
        .unwrap();
        let db = Db::prepare(conn).unwrap();
        let mut stmt = db
            .conn
            .prepare("SELECT fork_repo_id, decision, chosen FROM fork_links ORDER BY 1")
            .unwrap();
        let rows: Vec<(i64, String, bool)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            [
                (2, "separate".into(), false),
                (3, "linked".into(), false),
                (4, "separate".into(), true),
                (5, "linked".into(), true),
            ]
        );
    }

    #[test]
    fn other_programs_databases_are_refused_untouched() {
        // Shaped like the other depository's: same version number, different columns.
        let path = temp_db("foreign");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE clones(id INTEGER PRIMARY KEY, root TEXT, path TEXT, repo_id INTEGER,
                                     payload TEXT NOT NULL);
                 CREATE TABLE repos(id INTEGER PRIMARY KEY, host TEXT, owner TEXT, name TEXT,
                                    github_node_id TEXT, payload TEXT NOT NULL);
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        }
        let before = fs::read(&path).unwrap();
        let error = Db::open(&path).err().unwrap().to_string();
        assert!(error.contains("different program"), "{error}");
        assert!(error.contains("hasn't changed it"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), before, "the file wasn't touched");

        // A stamp from another program, or a version from the future, is refused too.
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "application_id", 1234).unwrap();
        drop(conn);
        assert!(
            Db::open(&path)
                .err()
                .unwrap()
                .to_string()
                .contains("application ID")
        );
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "application_id", 0).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        assert!(Db::open(&path).err().unwrap().to_string().contains("newer"));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn snippets_and_history() {
        let mut db = Db::open_in_memory().unwrap();
        db.save_snippet(" Build ", " cargo build ").unwrap();
        assert!(db.save_snippet("", "x").is_err());
        let snippets = db.snippets().unwrap();
        assert_eq!(snippets.len(), 5);
        assert_eq!(
            (snippets[4].name.as_str(), snippets[4].command.as_str()),
            ("Build", "cargo build")
        );
        db.delete_snippet(snippets[4].id).unwrap();
        assert_eq!(db.snippets().unwrap().len(), 4);

        for i in 0..3 {
            db.add_history(&format!("cmd {i}"), Duration::from_secs(2), 5, 4, 1)
                .unwrap();
        }
        let history = db.history(2).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].command, "cmd 2");
        assert_eq!(
            (
                history[0].repo_count,
                history[0].ok_count,
                history[0].failed_count
            ),
            (5, 4, 1)
        );
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
        let first = db
            .apply_scan(&[clone("x/react-old", Some(&react))])
            .unwrap();
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
        db.apply_scan(&[clone("x/react-old", Some(&react))])
            .unwrap();
        // Two new folders for the same repo: can't tell which one is the moved one.
        let applied = db
            .apply_scan(&[
                clone("r/react", Some(&react)),
                clone("r/react~2", Some(&react)),
            ])
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
            db.main_remote_picks()
                .unwrap()
                .get("l/local")
                .map(String::as_str),
            Some("fork")
        );
        db.set_main_remote(ids[0], None).unwrap();
        assert!(db.main_remote_picks().unwrap().is_empty());
    }
}
