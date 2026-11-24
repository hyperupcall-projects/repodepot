//! What GitHub sync stores, and the lists built from it: Inbox, Ignored and fork families.

use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::Db;

/// Why a repo is in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Owned,
    OrgMember,
    Collaborator,
    Contributed,
    ManualRepo,
    ManualOwner,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Owned => "owned",
            Source::OrgMember => "org_member",
            Source::Collaborator => "collaborator",
            Source::Contributed => "contributed",
            Source::ManualRepo => "manual_repo",
            Source::ManualOwner => "manual_owner",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "owned" => Source::Owned,
            "org_member" => Source::OrgMember,
            "collaborator" => Source::Collaborator,
            "contributed" => Source::Contributed,
            "manual_repo" => Source::ManualRepo,
            "manual_owner" => Source::ManualOwner,
            _ => return None,
        })
    }

    /// Short badge text.
    pub fn label(self) -> &'static str {
        match self {
            Source::Owned => "owned",
            Source::OrgMember => "org",
            Source::Collaborator => "collaborator",
            Source::Contributed => "contributed",
            Source::ManualRepo => "added",
            Source::ManualOwner => "added owner",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkDecision {
    /// Fork for contributing: one clone with the parent as an upstream remote.
    Linked,
    /// Hard fork: fork and parent are handled independently.
    Separate,
}

impl ForkDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            ForkDecision::Linked => "linked",
            ForkDecision::Separate => "separate",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "separate" => ForkDecision::Separate,
            _ => ForkDecision::Linked,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ForkDecision::Linked => "Linked",
            ForkDecision::Separate => "Separate",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            ForkDecision::Linked => ForkDecision::Separate,
            ForkDecision::Separate => ForkDecision::Linked,
        }
    }
}

/// Gives every fork not decided by hand its default: linked, unless the fork has a clone
/// and none of its clones has a remote for the parent (it's set up as a separate repo).
/// Runs after every sync and scan, since both change what that depends on.
pub(super) fn apply_fork_defaults(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE fork_links SET decision = CASE
             WHEN EXISTS (SELECT 1 FROM clones c
                          WHERE c.repo_id = fork_links.fork_repo_id AND c.missing_since IS NULL)
              AND NOT EXISTS (SELECT 1 FROM clones c JOIN clone_remotes r ON r.clone_id = c.id
                              WHERE c.repo_id = fork_links.fork_repo_id
                                AND c.missing_since IS NULL
                                AND r.repo_id = fork_links.parent_repo_id)
             THEN 'separate' ELSE 'linked' END
         WHERE NOT chosen",
        [],
    )?;
    Ok(())
}

/// A repo as GitHub describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoMeta {
    pub node_id: String,
    pub owner: String,
    pub name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub topics: Vec<String>,
    pub is_fork: bool,
    pub is_archived: bool,
    pub is_private: bool,
    pub pushed_at: Option<String>,
    pub ssh_url: Option<String>,
    pub stars: u32,
}

impl RepoMeta {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// One repo found by a sync, with every source that listed it.
#[derive(Clone, Debug)]
pub struct FetchedRepo {
    pub meta: RepoMeta,
    pub parent: Option<RepoMeta>,
    pub sources: BTreeSet<Source>,
}

pub struct SyncApplied {
    /// Repos with at least one source after the sync.
    pub relevant: usize,
    /// Fork/parent pairs seen for the first time.
    pub new_forks: usize,
}

/// What a sync learned about a GitHub repo, for judging the clones that point at it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoState {
    /// Some source lists it.
    pub listed: bool,
    pub not_found: bool,
    /// (full name, SSH URL) of where it moved.
    pub renamed_to: Option<(String, String)>,
    /// (full name, SSH URL) of its parent, when it's a linked fork.
    pub linked_upstream: Option<(String, String)>,
}

/// A repo for the Remote List and Ignored lists.
#[derive(Clone, Debug, PartialEq)]
pub struct RepoRow {
    pub id: i64,
    pub owner: String,
    pub name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub topics: Vec<String>,
    pub is_fork: bool,
    pub is_archived: bool,
    pub is_private: bool,
    pub pushed_at: Option<String>,
    /// `None` for repos not synced since stars were added.
    pub stars: Option<u32>,
    pub sources: Vec<Source>,
    /// Parent's full name and the decision for this fork.
    pub fork_of: Option<(String, ForkDecision)>,
    pub ignored: bool,
}

impl RepoRow {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// One fork and its parent, for the Forks table.
#[derive(Clone, Debug, PartialEq)]
pub struct ForkRow {
    pub fork_id: i64,
    pub fork_name: String,
    pub parent_id: i64,
    pub parent_name: String,
    pub decision: ForkDecision,
    /// Decided by hand; otherwise it's the default.
    pub chosen: bool,
    pub fork_cloned: bool,
    pub parent_cloned: bool,
    /// The parent has a source of its own.
    pub parent_listed: bool,
    /// A clone of the fork has a remote for the parent.
    pub has_upstream_remote: bool,
}

/// What the Repositories filters need to know about any repo, cloned or not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoSummary {
    pub description: Option<String>,
    pub language: Option<String>,
    pub is_archived: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    /// UTC `YYYY-MM-DD HH:MM:SS`, or `None` before the first sync.
    pub last_sync: Option<String>,
    pub relevant: usize,
    /// Listed repos that aren't cloned or ignored, shown as the Remote List.
    pub inbox: Vec<RepoRow>,
    /// Every ignored repo, shown as the Ignored list.
    pub ignored: Vec<RepoRow>,
    /// The ignored repos that would otherwise be on the Remote List; it shows them when
    /// "Hide ignored" is off.
    pub ignored_remote: Vec<RepoRow>,
    pub forks: Vec<ForkRow>,
    pub manual_owners: Vec<String>,
    /// Every GitHub repo by ID, so clones and forks can be filtered like the lists.
    pub summaries: HashMap<i64, RepoSummary>,
}

/// What's needed to clone one repo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneItem {
    pub repo_id: i64,
    pub full_name: String,
    pub name: String,
    pub ssh_url: String,
    /// The parent's SSH URL, when this is a linked fork.
    pub upstream_url: Option<String>,
}

fn ssh_url_or_default(url: Option<String>, owner: &str, name: &str) -> String {
    url.unwrap_or_else(|| format!("git@github.com:{owner}/{name}.git"))
}

impl Db {
    pub fn last_sync(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'last_sync'", [], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Stores a sync's results. Sources are replaced wholesale, except repos added by hand.
    pub fn apply_sync(&mut self, fetched: &[FetchedRepo]) -> Result<SyncApplied> {
        let tx = self.conn.transaction()?;
        let mut sources: Vec<(i64, Source)> = Vec::new();
        let mut new_forks = 0;
        for repo in fetched {
            let id = upsert_github(&tx, &repo.meta)?;
            if let Some(parent) = &repo.parent {
                let parent_id = upsert_github(&tx, parent)?;
                tx.execute(
                    "UPDATE repos SET parent_repo_id = ?2 WHERE id = ?1",
                    params![id, parent_id],
                )?;
                new_forks += tx.execute(
                    "INSERT INTO fork_links (fork_repo_id, parent_repo_id) VALUES (?1, ?2)
                     ON CONFLICT DO NOTHING",
                    params![id, parent_id],
                )?;
            }
            sources.extend(
                repo.sources
                    .iter()
                    .filter(|s| **s != Source::ManualRepo)
                    .map(|s| (id, *s)),
            );
            if !repo.sources.is_empty() {
                tx.execute(
                    "UPDATE repos SET last_listed_at = datetime('now') WHERE id = ?1",
                    [id],
                )?;
            }
        }

        tx.execute("DELETE FROM repo_sources WHERE source <> 'manual_repo'", [])?;
        {
            let mut insert =
                tx.prepare("INSERT OR IGNORE INTO repo_sources (repo_id, source) VALUES (?1, ?2)")?;
            for (id, source) in sources {
                insert.execute(params![id, source.as_str()])?;
            }
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('last_sync', datetime('now'))
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        apply_fork_defaults(&tx)?;
        let relevant: i64 = tx.query_row(
            "SELECT count(DISTINCT repo_id) FROM repo_sources",
            [],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(SyncApplied {
            relevant: relevant as usize,
            new_forks,
        })
    }

    /// GitHub repos that clones point at but no source lists: (id, owner, name).
    pub fn unlisted_clone_repos(&self) -> Result<Vec<(i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT r.id, r.owner, r.name
             FROM clones c JOIN repos r ON r.id = c.repo_id
             WHERE c.missing_since IS NULL AND r.host = 'github.com'
               AND NOT EXISTS (SELECT 1 FROM repo_sources s WHERE s.repo_id = r.id)",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mark_found(&mut self, repo_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE repos SET not_found_at = NULL, renamed_to_id = NULL WHERE id = ?1",
            [repo_id],
        )?;
        Ok(())
    }

    pub fn mark_not_found(&mut self, repo_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE repos SET not_found_at = COALESCE(not_found_at, datetime('now')),
                              renamed_to_id = NULL
             WHERE id = ?1",
            [repo_id],
        )?;
        Ok(())
    }

    /// Records that the repo at `old_id` now lives at `new`.
    pub fn mark_renamed(&mut self, old_id: i64, new: &RepoMeta) -> Result<()> {
        let tx = self.conn.transaction()?;
        // The old row stands for the old name now; its node ID belongs to the new one.
        tx.execute(
            "UPDATE repos SET github_node_id = NULL WHERE id = ?1",
            [old_id],
        )?;
        let new_id = upsert_github(&tx, new)?;
        tx.execute(
            "UPDATE repos SET renamed_to_id = ?2, not_found_at = NULL WHERE id = ?1",
            params![old_id, new_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Per-repo facts from the last sync, or `None` if there hasn't been one.
    pub fn repo_states(&self) -> Result<Option<HashMap<i64, RepoState>>> {
        if self.last_sync()?.is_none() {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare(
            "SELECT r.id,
                    EXISTS (SELECT 1 FROM repo_sources s WHERE s.repo_id = r.id),
                    r.not_found_at IS NOT NULL,
                    n.owner, n.name, n.ssh_url,
                    p.owner, p.name, p.ssh_url
             FROM repos r
             LEFT JOIN repos n ON n.id = r.renamed_to_id
             LEFT JOIN fork_links f ON f.fork_repo_id = r.id AND f.decision = 'linked'
             LEFT JOIN repos p ON p.id = f.parent_repo_id
             WHERE r.host = 'github.com'",
        )?;
        let rows = stmt.query_map([], |r| {
            let named = |owner: Option<String>, name: Option<String>, url: Option<String>| {
                owner.zip(name).map(|(owner, name)| {
                    let url = ssh_url_or_default(url, &owner, &name);
                    (format!("{owner}/{name}"), url)
                })
            };
            Ok((
                r.get::<_, i64>(0)?,
                RepoState {
                    listed: r.get(1)?,
                    not_found: r.get(2)?,
                    renamed_to: named(r.get(3)?, r.get(4)?, r.get(5)?),
                    linked_upstream: named(r.get(6)?, r.get(7)?, r.get(8)?),
                },
            ))
        })?;
        Ok(Some(rows.collect::<rusqlite::Result<_>>()?))
    }

    pub fn set_ignored(&mut self, repo_ids: &[i64], ignored: bool) -> Result<()> {
        let tx = self.conn.transaction()?;
        for id in repo_ids {
            tx.execute(
                "UPDATE repos SET ignored = ?2 WHERE id = ?1",
                params![id, ignored],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Decides a fork by hand; the default no longer applies to it.
    pub fn set_fork_decision(
        &mut self,
        fork_id: i64,
        parent_id: i64,
        decision: ForkDecision,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE fork_links SET decision = ?3, chosen = 1
             WHERE fork_repo_id = ?1 AND parent_repo_id = ?2",
            params![fork_id, parent_id, decision.as_str()],
        )?;
        Ok(())
    }

    /// Adds one repo by hand (already checked with GitHub).
    pub fn add_manual_repo(&mut self, meta: &RepoMeta) -> Result<()> {
        let tx = self.conn.transaction()?;
        let id = upsert_github(&tx, meta)?;
        tx.execute(
            "INSERT OR IGNORE INTO repo_sources (repo_id, source) VALUES (?1, 'manual_repo')",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// (owner, name) of every repo added by hand.
    pub fn manual_repos(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.owner, r.name FROM repos r JOIN repo_sources s ON s.repo_id = r.id
             WHERE s.source = 'manual_repo' ORDER BY r.owner, r.name",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn manual_owners(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT login FROM manual_owners ORDER BY login COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_manual_owner(&mut self, login: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO manual_owners (login) VALUES (?1)",
            [login],
        )?;
        Ok(())
    }

    pub fn remove_manual_owner(&mut self, login: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM manual_owners WHERE login = ?1", [login])?;
        tx.execute(
            "DELETE FROM repo_sources WHERE source = 'manual_owner'
               AND repo_id IN (SELECT id FROM repos WHERE owner = ?1)",
            [login],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Paths of present clones, by the lowercase `host/owner/name` of their main repo.
    pub fn clones_by_repo(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut stmt = self.conn.prepare(
            "SELECT lower(r.host || '/' || r.owner || '/' || r.name), c.path
             FROM clones c JOIN repos r ON r.id = c.repo_id
             WHERE c.missing_since IS NULL",
        )?;
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (key, path) = row?;
            map.entry(key).or_default().push(path);
        }
        Ok(map)
    }

    /// Every clone path in the database, including missing ones.
    pub fn clone_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM clones")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn clone_items(&self, repo_ids: &[i64]) -> Result<Vec<CloneItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.owner, r.name, r.ssh_url, p.owner, p.name, p.ssh_url
             FROM repos r
             LEFT JOIN fork_links f ON f.fork_repo_id = r.id AND f.decision = 'linked'
             LEFT JOIN repos p ON p.id = f.parent_repo_id
             WHERE r.id = ?1",
        )?;
        let mut items = Vec::new();
        for id in repo_ids {
            let item = stmt
                .query_row([id], |r| {
                    let owner: String = r.get(1)?;
                    let name: String = r.get(2)?;
                    let parent: Option<(String, String)> =
                        r.get::<_, Option<String>>(4)?.zip(r.get(5)?);
                    Ok(CloneItem {
                        repo_id: r.get(0)?,
                        full_name: format!("{owner}/{name}"),
                        ssh_url: ssh_url_or_default(r.get(3)?, &owner, &name),
                        upstream_url: match parent {
                            Some((p_owner, p_name)) => {
                                Some(ssh_url_or_default(r.get(6)?, &p_owner, &p_name))
                            }
                            None => None,
                        },
                        name,
                    })
                })
                .optional()?;
            items.extend(item);
        }
        Ok(items)
    }

    /// Builds the Remote List, Ignored and Forks lists.
    pub fn catalog(&self) -> Result<Catalog> {
        let mut sources: HashMap<i64, Vec<Source>> = HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT repo_id, source FROM repo_sources")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (id, source) = row?;
                if let Some(source) = Source::parse(&source) {
                    sources.entry(id).or_default().push(source);
                }
            }
        }
        for list in sources.values_mut() {
            list.sort();
        }
        let listed = |id: &i64| sources.contains_key(id);

        let cloned: HashSet<i64> = {
            let mut stmt = self.conn.prepare(
                "SELECT repo_id FROM clones WHERE missing_since IS NULL AND repo_id IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        // (main repo, remote's repo) for each present clone, to tell whether a fork's clone
        // has a remote for the parent.
        let clone_remotes: HashSet<(i64, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT c.repo_id, cr.repo_id FROM clone_remotes cr
                 JOIN clones c ON c.id = cr.clone_id
                 WHERE c.repo_id IS NOT NULL AND cr.repo_id IS NOT NULL
                   AND c.missing_since IS NULL",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let links: Vec<(i64, i64, ForkDecision, bool)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT fork_repo_id, parent_repo_id, decision, chosen FROM fork_links")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    ForkDecision::parse(&r.get::<_, String>(2)?),
                    r.get(3)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        // Parents of linked forks are covered by the fork's clone.
        let mut covered: HashSet<i64> = HashSet::new();
        let mut cloned_through_fork: HashSet<i64> = HashSet::new();
        for (fork, parent, decision, _) in &links {
            if *decision == ForkDecision::Linked && listed(fork) {
                covered.insert(*parent);
                if cloned.contains(fork) {
                    cloned_through_fork.insert(*parent);
                }
            }
        }
        let is_cloned = |id: &i64| cloned.contains(id) || cloned_through_fork.contains(id);

        struct Base {
            row: RepoRow,
        }
        let mut repos: HashMap<i64, Base> = HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT r.id, r.owner, r.name, r.description, r.language, r.topics, r.is_fork,
                        r.is_archived, r.is_private, r.pushed_at, r.ignored,
                        p.owner, p.name, f.decision, r.stars
                 FROM repos r
                 LEFT JOIN fork_links f ON f.fork_repo_id = r.id
                 LEFT JOIN repos p ON p.id = f.parent_repo_id
                 WHERE r.host = 'github.com'",
            )?;
            let rows = stmt.query_map([], |r| {
                let id: i64 = r.get(0)?;
                let topics: String = r.get(5)?;
                let parent: Option<(String, String)> =
                    r.get::<_, Option<String>>(11)?.zip(r.get(12)?);
                let decision: Option<String> = r.get(13)?;
                Ok(Base {
                    row: RepoRow {
                        id,
                        owner: r.get(1)?,
                        name: r.get(2)?,
                        description: r.get(3)?,
                        language: r.get(4)?,
                        topics: serde_json::from_str(&topics).unwrap_or_default(),
                        is_fork: r.get(6)?,
                        is_archived: r.get(7)?,
                        is_private: r.get(8)?,
                        pushed_at: r.get(9)?,
                        stars: r.get(14)?,
                        sources: Vec::new(),
                        fork_of: parent.map(|(o, n)| {
                            (
                                format!("{o}/{n}"),
                                ForkDecision::parse(decision.as_deref().unwrap_or("")),
                            )
                        }),
                        ignored: r.get(10)?,
                    },
                })
            })?;
            for base in rows {
                let mut base = base?;
                base.row.sources = sources.get(&base.row.id).cloned().unwrap_or_default();
                repos.insert(base.row.id, base);
            }
        }

        let mut inbox = Vec::new();
        let mut ignored = Vec::new();
        let mut ignored_remote = Vec::new();
        for (id, base) in &repos {
            let remote = listed(id) && !is_cloned(id) && !covered.contains(id);
            if base.row.ignored {
                ignored.push(base.row.clone());
                if remote {
                    ignored_remote.push(base.row.clone());
                }
            } else if remote {
                inbox.push(base.row.clone());
            }
        }
        let by_name = |a: &RepoRow, b: &RepoRow| {
            a.full_name()
                .to_lowercase()
                .cmp(&b.full_name().to_lowercase())
        };
        inbox.sort_by(by_name);
        ignored.sort_by(by_name);
        ignored_remote.sort_by(by_name);

        let mut forks = Vec::new();
        for (fork, parent, decision, chosen) in &links {
            if !listed(fork) {
                continue;
            }
            let (Some(fork_repo), Some(parent_repo)) = (repos.get(fork), repos.get(parent)) else {
                continue;
            };
            forks.push(ForkRow {
                fork_id: *fork,
                fork_name: fork_repo.row.full_name(),
                parent_id: *parent,
                parent_name: parent_repo.row.full_name(),
                decision: *decision,
                chosen: *chosen,
                fork_cloned: cloned.contains(fork),
                parent_cloned: cloned.contains(parent),
                parent_listed: listed(parent),
                has_upstream_remote: clone_remotes.contains(&(*fork, *parent)),
            });
        }
        forks.sort_by_key(|f| (f.fork_name.to_lowercase(), f.parent_name.to_lowercase()));

        let summaries = repos
            .iter()
            .map(|(id, base)| {
                (
                    *id,
                    RepoSummary {
                        description: base.row.description.clone(),
                        language: base.row.language.clone(),
                        is_archived: base.row.is_archived,
                    },
                )
            })
            .collect();

        Ok(Catalog {
            last_sync: self.last_sync()?,
            relevant: sources.len(),
            inbox,
            ignored,
            ignored_remote,
            forks,
            manual_owners: self.manual_owners()?,
            summaries,
        })
    }
}

/// Finds a GitHub repo's row by node ID, then by name, and updates it; inserts if neither
/// matches. If the node ID and the name point at two different rows, they're merged.
pub(super) fn upsert_github(tx: &Transaction<'_>, meta: &RepoMeta) -> Result<i64> {
    let by_node: Option<i64> = tx
        .query_row(
            "SELECT id FROM repos WHERE github_node_id = ?1",
            [&meta.node_id],
            |r| r.get(0),
        )
        .optional()?;
    let by_name: Option<i64> = tx
        .query_row(
            "SELECT id FROM repos WHERE host = 'github.com' AND owner = ?1 AND name = ?2",
            params![meta.owner, meta.name],
            |r| r.get(0),
        )
        .optional()?;
    let id = match (by_node, by_name) {
        (Some(node), Some(name)) if node != name => {
            merge_repo(tx, name, node)?;
            node
        }
        (Some(id), _) | (None, Some(id)) => id,
        (None, None) => {
            tx.execute(
                "INSERT INTO repos (host, owner, name) VALUES ('github.com', ?1, ?2)",
                params![meta.owner, meta.name],
            )?;
            tx.last_insert_rowid()
        }
    };
    tx.execute(
        "UPDATE repos SET owner = ?2, name = ?3, github_node_id = ?4, description = ?5,
                language = ?6, topics = ?7, is_fork = ?8, is_archived = ?9, is_private = ?10,
                pushed_at = ?11, ssh_url = ?12, stars = ?13, not_found_at = NULL,
                renamed_to_id = NULL
         WHERE id = ?1",
        params![
            id,
            meta.owner,
            meta.name,
            meta.node_id,
            meta.description,
            meta.language,
            serde_json::to_string(&meta.topics)?,
            meta.is_fork,
            meta.is_archived,
            meta.is_private,
            meta.pushed_at,
            meta.ssh_url,
            meta.stars,
        ],
    )?;
    Ok(id)
}

/// Moves everything that points at repo `from` over to `into`, then deletes `from`.
fn merge_repo(tx: &Transaction<'_>, from: i64, into: i64) -> Result<()> {
    for sql in [
        "UPDATE clones SET repo_id = ?2 WHERE repo_id = ?1",
        "UPDATE clone_remotes SET repo_id = ?2 WHERE repo_id = ?1",
        "INSERT OR IGNORE INTO repo_sources (repo_id, source)
             SELECT ?2, source FROM repo_sources WHERE repo_id = ?1",
        "UPDATE OR IGNORE fork_links SET fork_repo_id = ?2 WHERE fork_repo_id = ?1",
        "UPDATE OR IGNORE fork_links SET parent_repo_id = ?2 WHERE parent_repo_id = ?1",
        "INSERT OR IGNORE INTO repo_tags (repo_id, tag_id)
             SELECT ?2, tag_id FROM repo_tags WHERE repo_id = ?1",
        "UPDATE repos SET parent_repo_id = ?2 WHERE parent_repo_id = ?1",
        "UPDATE repos SET renamed_to_id = ?2 WHERE renamed_to_id = ?1",
        "UPDATE repos SET ignored = max(ignored, (SELECT ignored FROM repos WHERE id = ?1))
             WHERE id = ?2",
    ] {
        tx.execute(sql, params![from, into])?;
    }
    tx.execute("DELETE FROM repos WHERE id = ?1", [from])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ScannedClone, ScannedRemote};
    use crate::git::remote_url::{GITHUB, RemoteUrl};

    fn meta(node: &str, owner: &str, name: &str) -> RepoMeta {
        RepoMeta {
            node_id: node.into(),
            owner: owner.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    fn fetched(meta: RepoMeta, sources: &[Source]) -> FetchedRepo {
        FetchedRepo {
            meta,
            parent: None,
            sources: sources.iter().copied().collect(),
        }
    }

    fn fork(fork: RepoMeta, parent: RepoMeta, sources: &[Source]) -> FetchedRepo {
        FetchedRepo {
            meta: RepoMeta {
                is_fork: true,
                ..fork
            },
            parent: Some(parent),
            sources: sources.iter().copied().collect(),
        }
    }

    fn url(owner: &str, name: &str) -> RemoteUrl {
        RemoteUrl {
            host: GITHUB.into(),
            owner: owner.into(),
            name: name.into(),
        }
    }

    /// Records clones as a scan would: each with a `me` remote, plus an optional second remote.
    fn scan(db: &mut Db, clones: &[(&str, &RemoteUrl, Option<&RemoteUrl>)]) {
        let scanned: Vec<ScannedClone<'_>> = clones
            .iter()
            .map(|(path, main, extra)| {
                let mut remotes = vec![ScannedRemote {
                    name: "me",
                    url: "x",
                    parsed: Some(main),
                }];
                if let Some(extra) = extra {
                    remotes.push(ScannedRemote {
                        name: "upstream",
                        url: "y",
                        parsed: Some(extra),
                    });
                }
                ScannedClone {
                    path,
                    remotes,
                    main_remote: Some(0),
                }
            })
            .collect();
        db.apply_scan(&scanned).unwrap();
    }

    fn names(rows: &[RepoRow]) -> Vec<String> {
        rows.iter().map(RepoRow::full_name).collect()
    }

    #[test]
    fn inbox_holds_listed_uncloned_unignored_repos() {
        let mut db = Db::open_in_memory().unwrap();
        assert_eq!(db.catalog().unwrap().last_sync, None);
        db.apply_sync(&[
            fetched(meta("A", "me", "alpha"), &[Source::Owned]),
            fetched(
                RepoMeta {
                    stars: 1234,
                    ..meta("B", "org", "beta")
                },
                &[Source::OrgMember, Source::Contributed],
            ),
            fetched(meta("C", "me", "gamma"), &[Source::Owned]),
        ])
        .unwrap();
        scan(&mut db, &[("a/alpha", &url("me", "alpha"), None)]);
        let gamma = db
            .catalog()
            .unwrap()
            .inbox
            .iter()
            .find(|r| r.name == "gamma")
            .unwrap()
            .id;
        db.set_ignored(&[gamma], true).unwrap();

        let catalog = db.catalog().unwrap();
        assert!(catalog.last_sync.is_some());
        assert_eq!(catalog.relevant, 3);
        assert_eq!(names(&catalog.inbox), ["org/beta"]);
        assert_eq!(
            catalog.inbox[0].sources,
            [Source::OrgMember, Source::Contributed]
        );
        assert_eq!(catalog.inbox[0].stars, Some(1234));
        assert_eq!(names(&catalog.ignored), ["me/gamma"]);
    }

    #[test]
    fn ignored_repos_that_would_be_listed_are_kept_for_the_remote_list() {
        let mut db = Db::open_in_memory().unwrap();
        db.apply_sync(&[
            fetched(meta("A", "me", "alpha"), &[Source::Owned]),
            fetched(meta("B", "me", "beta"), &[Source::Owned]),
            fetched(meta("C", "me", "gamma"), &[Source::Owned]),
        ])
        .unwrap();
        scan(&mut db, &[("a/alpha", &url("me", "alpha"), None)]);
        let id = |db: &Db, name: &str| -> i64 {
            db.conn
                .query_row("SELECT id FROM repos WHERE name = ?1", [name], |r| r.get(0))
                .unwrap()
        };
        let ignore = [id(&db, "alpha"), id(&db, "gamma")];
        db.set_ignored(&ignore, true).unwrap();

        let catalog = db.catalog().unwrap();
        assert_eq!(names(&catalog.inbox), ["me/beta"]);
        assert!(!catalog.inbox[0].ignored);
        assert_eq!(names(&catalog.ignored), ["me/alpha", "me/gamma"]);
        // alpha is cloned, so it wouldn't be on the Remote List anyway.
        assert_eq!(names(&catalog.ignored_remote), ["me/gamma"]);
        assert!(catalog.ignored_remote[0].ignored);
    }

    #[test]
    fn sync_replaces_sources_but_keeps_manual_repos() {
        let mut db = Db::open_in_memory().unwrap();
        db.add_manual_repo(&meta("M", "someone", "tool")).unwrap();
        db.apply_sync(&[fetched(meta("A", "me", "alpha"), &[Source::Owned])])
            .unwrap();
        // Next sync: alpha isn't listed any more; the manual repo stays.
        let applied = db
            .apply_sync(&[fetched(meta("M", "someone", "tool"), &[Source::ManualRepo])])
            .unwrap();
        assert_eq!(applied.relevant, 1);
        assert_eq!(names(&db.catalog().unwrap().inbox), ["someone/tool"]);
        assert_eq!(
            db.manual_repos().unwrap(),
            [("someone".into(), "tool".into())]
        );
    }

    #[test]
    fn renames_follow_the_node_id_and_merge_rows() {
        let mut db = Db::open_in_memory().unwrap();
        db.apply_sync(&[fetched(meta("A", "me", "old-name"), &[Source::Owned])])
            .unwrap();
        // A local clone already points at the new name, creating a second row for it.
        scan(&mut db, &[("n/new-name", &url("me", "new-name"), None)]);
        let scanned_row: i64 = db
            .conn
            .query_row("SELECT id FROM repos WHERE name = 'new-name'", [], |r| {
                r.get(0)
            })
            .unwrap();
        db.add_tag(&[], &[scanned_row], "kept").unwrap();
        db.apply_sync(&[fetched(meta("A", "me", "new-name"), &[Source::Owned])])
            .unwrap();

        let rows: i64 = db
            .conn
            .query_row("SELECT count(*) FROM repos", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the two rows were merged");
        let store = db.tag_store().unwrap();
        assert_eq!(
            store.repo_manual.values().collect::<Vec<_>>(),
            [&BTreeSet::from(["kept".to_owned()])],
            "repo tags move to the surviving row"
        );
        let catalog = db.catalog().unwrap();
        assert!(catalog.inbox.is_empty(), "the merged repo is cloned");
        assert!(db.unlisted_clone_repos().unwrap().is_empty());
    }

    #[test]
    fn forks_default_to_linked_which_covers_the_parent() {
        let mut db = Db::open_in_memory().unwrap();
        let parent = meta("P", "facebook", "react");
        db.apply_sync(&[
            fork(meta("F", "me", "react"), parent.clone(), &[Source::Owned]),
            fetched(parent.clone(), &[Source::Contributed]),
        ])
        .unwrap();
        let catalog = db.catalog().unwrap();
        assert_eq!(names(&catalog.inbox), ["me/react"], "the parent is covered");
        let row = &catalog.forks[0];
        assert_eq!(
            (row.fork_name.as_str(), row.parent_name.as_str()),
            ("me/react", "facebook/react")
        );
        assert!(row.parent_listed);
        assert_eq!(row.decision, ForkDecision::Linked);
        assert!(!row.chosen);
        assert_eq!(
            catalog.inbox[0].fork_of,
            Some(("facebook/react".into(), ForkDecision::Linked))
        );

        let (fork_id, parent_id) = (row.fork_id, row.parent_id);
        db.set_fork_decision(fork_id, parent_id, ForkDecision::Separate)
            .unwrap();
        let catalog = db.catalog().unwrap();
        assert_eq!(names(&catalog.inbox), ["facebook/react", "me/react"]);
        assert!(catalog.forks[0].chosen);

        db.set_fork_decision(fork_id, parent_id, ForkDecision::Linked)
            .unwrap();
        assert_eq!(names(&db.catalog().unwrap().inbox), ["me/react"]);
    }

    #[test]
    fn clone_items_include_upstream_for_linked_forks() {
        let mut db = Db::open_in_memory().unwrap();
        let parent = RepoMeta {
            ssh_url: Some("git@github.com:facebook/react.git".into()),
            ..meta("P", "facebook", "react")
        };
        db.apply_sync(&[fork(meta("F", "me", "react"), parent, &[Source::Owned])])
            .unwrap();
        let row = db.catalog().unwrap().forks[0].clone();

        let items = db.clone_items(&[row.fork_id]).unwrap();
        assert_eq!(items[0].ssh_url, "git@github.com:me/react.git");
        assert_eq!(
            items[0].upstream_url.as_deref(),
            Some("git@github.com:facebook/react.git")
        );
        assert_eq!(items[0].name, "react");

        db.set_fork_decision(row.fork_id, row.parent_id, ForkDecision::Separate)
            .unwrap();
        let items = db.clone_items(&[row.fork_id]).unwrap();
        assert_eq!(items[0].upstream_url, None);
    }

    #[test]
    fn a_clone_without_the_parent_remote_makes_the_default_separate() {
        let mut db = Db::open_in_memory().unwrap();
        let (me, parent) = (url("me", "react"), url("facebook", "react"));
        // Cloned before the first sync: the sync already sees the clone.
        scan(&mut db, &[("r/react", &me, None)]);
        db.apply_sync(&[fork(
            meta("F", "me", "react"),
            meta("P", "facebook", "react"),
            &[Source::Owned],
        )])
        .unwrap();
        let decision = |db: &Db| {
            let row = db.catalog().unwrap().forks[0].clone();
            (row.decision, row.chosen, row.has_upstream_remote)
        };
        assert_eq!(decision(&db), (ForkDecision::Separate, false, false));
        assert!(
            db.repo_states()
                .unwrap()
                .unwrap()
                .values()
                .all(|s| s.linked_upstream.is_none())
        );

        // Adding the parent as a remote follows on the next scan.
        scan(&mut db, &[("r/react", &me, Some(&parent))]);
        assert_eq!(decision(&db), (ForkDecision::Linked, false, true));

        // A second clone set up on its own doesn't undo that.
        scan(
            &mut db,
            &[("r/react", &me, Some(&parent)), ("r/react~2", &me, None)],
        );
        assert_eq!(decision(&db).0, ForkDecision::Linked);

        // Only that one clone, with no parent remote: back to separate.
        scan(&mut db, &[("r/react~2", &me, None)]);
        assert_eq!(decision(&db).0, ForkDecision::Separate);

        // Decided by hand, it stays put.
        let row = db.catalog().unwrap().forks[0].clone();
        db.set_fork_decision(row.fork_id, row.parent_id, ForkDecision::Linked)
            .unwrap();
        scan(&mut db, &[("r/react~2", &me, None)]);
        db.apply_sync(&[fork(
            meta("F", "me", "react"),
            meta("P", "facebook", "react"),
            &[Source::Owned],
        )])
        .unwrap();
        assert_eq!(decision(&db), (ForkDecision::Linked, true, false));
    }

    #[test]
    fn a_fork_cloned_after_the_sync_gets_its_default_on_the_scan() {
        let mut db = Db::open_in_memory().unwrap();
        db.apply_sync(&[fork(
            meta("F", "me", "react"),
            meta("P", "facebook", "react"),
            &[Source::Owned],
        )])
        .unwrap();
        assert_eq!(
            db.catalog().unwrap().forks[0].decision,
            ForkDecision::Linked
        );
        scan(&mut db, &[("r/react", &url("me", "react"), None)]);
        let row = db.catalog().unwrap().forks[0].clone();
        assert_eq!(row.decision, ForkDecision::Separate);
        assert!(row.fork_cloned);
        assert!(!row.parent_cloned);
    }

    #[test]
    fn repo_states_report_unlisted_missing_and_renamed() {
        let mut db = Db::open_in_memory().unwrap();
        scan(
            &mut db,
            &[
                ("g/gone", &url("me", "gone"), None),
                ("o/old", &url("me", "old"), None),
            ],
        );
        assert_eq!(db.repo_states().unwrap(), None, "no sync yet");
        db.apply_sync(&[]).unwrap();
        let unlisted = db.unlisted_clone_repos().unwrap();
        assert_eq!(unlisted.len(), 2);
        let id = |name: &str| unlisted.iter().find(|r| r.2 == name).unwrap().0;

        db.mark_not_found(id("gone")).unwrap();
        db.mark_renamed(
            id("old"),
            &RepoMeta {
                ssh_url: Some("git@github.com:me/new.git".into()),
                ..meta("N", "me", "new")
            },
        )
        .unwrap();
        let states = db.repo_states().unwrap().unwrap();
        assert!(states[&id("gone")].not_found);
        assert!(!states[&id("gone")].listed);
        assert_eq!(
            states[&id("old")].renamed_to,
            Some(("me/new".into(), "git@github.com:me/new.git".into()))
        );

        db.mark_found(id("gone")).unwrap();
        assert!(!db.repo_states().unwrap().unwrap()[&id("gone")].not_found);
    }

    #[test]
    fn manual_owners_add_and_remove() {
        let mut db = Db::open_in_memory().unwrap();
        db.add_manual_owner("rust-lang").unwrap();
        db.add_manual_owner("rust-lang").unwrap();
        db.apply_sync(&[fetched(
            meta("R", "rust-lang", "rust"),
            &[Source::ManualOwner],
        )])
        .unwrap();
        assert_eq!(db.catalog().unwrap().manual_owners, ["rust-lang"]);
        assert_eq!(db.catalog().unwrap().inbox.len(), 1);

        db.remove_manual_owner("rust-lang").unwrap();
        let catalog = db.catalog().unwrap();
        assert!(catalog.manual_owners.is_empty());
        assert!(catalog.inbox.is_empty());
    }
}
