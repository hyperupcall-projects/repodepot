//! Scanning the clone root: which folders are clones, what their remotes and status are,
//! and what's wrong with them. The folders on disk decide what's cloned.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::Semaphore;
use tokio::task::{JoinSet, spawn_blocking};

use super::remote_url::{self, RemoteUrl};
use super::status::{self, Status};
use crate::config::Config;
use crate::db::{self, Db, MissingClone, RepoState};
use crate::layout;

/// How many repos are inspected at once.
const PARALLEL: usize = 16;

#[derive(Clone, Debug)]
pub struct ScanSettings {
    pub root: Option<PathBuf>,
    pub default_remote: String,
    pub github_aliases: Vec<String>,
}

impl ScanSettings {
    pub fn from_config(config: &Config) -> Self {
        Self {
            root: config.clone_root.clone(),
            default_remote: config.remotes.default_name.clone(),
            github_aliases: config.github.ssh_host_aliases.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub parsed: Option<RemoteUrl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// Directly under the clone root, not in a letter folder.
    Root,
    /// Inside the letter folder with this name.
    Letter(char),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainRemote {
    Chosen {
        index: usize,
        picked_by_hand: bool,
    },
    NoRemotes,
    /// Several remotes, none matching the rule, and none picked.
    Undecided,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    NoRemotes,
    NoMainRemote,
    /// The main remote is a local path or a URL that can't be read.
    NotHosted {
        url: String,
    },
    OtherHost {
        host: String,
    },
    /// Not at `<letter>/<repo name>`.
    OutOfLayout {
        expected: String,
    },
    GitError {
        message: String,
    },
    /// A GitHub repo no source lists (judged after a sync).
    NotListed,
    /// GitHub says the repo doesn't exist, or the token can't see it.
    NotOnGitHub,
    /// GitHub says the repo moved; `url` is its new SSH URL.
    Renamed {
        name: String,
        url: String,
    },
    /// A linked fork's clone has no remote for the parent.
    MissingUpstream {
        name: String,
        url: String,
    },
}

#[derive(Clone, Debug)]
pub struct CloneInfo {
    pub id: i64,
    /// The main remote's repo in the database.
    pub repo_id: Option<i64>,
    /// Relative to the clone root, with `/` separators.
    pub rel_path: String,
    pub abs_path: PathBuf,
    pub remotes: Vec<Remote>,
    pub main: MainRemote,
    pub status: Result<Status, String>,
    pub problems: Vec<Problem>,
}

impl CloneInfo {
    pub fn main_remote(&self) -> Option<&Remote> {
        match self.main {
            MainRemote::Chosen { index, .. } => self.remotes.get(index),
            _ => None,
        }
    }

    /// The repo the folder is, according to its main remote.
    pub fn repo(&self) -> Option<&RemoteUrl> {
        self.main_remote().and_then(|r| r.parsed.as_ref())
    }

    pub fn folder_name(&self) -> &str {
        self.rel_path.rsplit('/').next().unwrap_or(&self.rel_path)
    }

    /// `owner/name` when known, otherwise the folder name.
    pub fn display_name(&self) -> String {
        match self.repo() {
            Some(repo) => repo.full_name(),
            None => self.folder_name().to_owned(),
        }
    }
}

#[derive(Debug)]
pub struct ScanResult {
    pub root: PathBuf,
    /// Sorted by path.
    pub clones: Vec<CloneInfo>,
    /// Folders under the clone root that aren't git repos.
    pub strays: Vec<String>,
    /// Clones from earlier scans whose folder is gone.
    pub missing: Vec<MissingClone>,
    pub took: Duration,
}

impl ScanResult {
    pub fn problem_count(&self) -> usize {
        self.clones.iter().map(|c| c.problems.len()).sum::<usize>()
            + self.strays.len()
            + self.missing.len()
    }
}

/// Picks the remote that says which repo a folder is: one picked by hand, else the default
/// remote name, else `origin`, else the only remote.
pub fn choose_main_remote(
    remotes: &[Remote],
    default_name: &str,
    picked: Option<&str>,
) -> MainRemote {
    if remotes.is_empty() {
        return MainRemote::NoRemotes;
    }
    let position = |name: &str| remotes.iter().position(|r| r.name == name);
    if let Some(index) = picked.and_then(position) {
        return MainRemote::Chosen {
            index,
            picked_by_hand: true,
        };
    }
    if let Some(index) = position(default_name).or_else(|| position("origin")) {
        return MainRemote::Chosen {
            index,
            picked_by_hand: false,
        };
    }
    if remotes.len() == 1 {
        return MainRemote::Chosen {
            index: 0,
            picked_by_hand: false,
        };
    }
    MainRemote::Undecided
}

/// What's wrong with a clone, judged without the network.
pub fn find_problems(
    rel_path: &str,
    location: Location,
    remotes: &[Remote],
    main: MainRemote,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut repo = None;
    match main {
        MainRemote::NoRemotes => problems.push(Problem::NoRemotes),
        MainRemote::Undecided => problems.push(Problem::NoMainRemote),
        MainRemote::Chosen { index, .. } => {
            let remote = &remotes[index];
            match &remote.parsed {
                None => problems.push(Problem::NotHosted {
                    url: remote.url.clone(),
                }),
                Some(parsed) => {
                    if !parsed.is_github() {
                        problems.push(Problem::OtherHost {
                            host: parsed.host.clone(),
                        });
                    }
                    repo = Some(parsed);
                }
            }
        }
    }

    let folder = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let (base, _) = layout::split_suffix(folder);
    let name = repo.map_or(base, |r| r.name.as_str());
    let in_place = match location {
        Location::Root => false,
        Location::Letter(letter) => {
            letter == layout::letter_for(name) && base.eq_ignore_ascii_case(name)
        }
    };
    if !in_place {
        problems.push(Problem::OutOfLayout {
            expected: layout::expected_path(name),
        });
    }
    problems
}

/// Problems only a sync can reveal, for a clone whose main remote is `state`'s repo.
pub fn sync_problems(clone: &CloneInfo, state: &RepoState) -> Vec<Problem> {
    let mut problems = Vec::new();
    if !clone.repo().is_some_and(RemoteUrl::is_github) {
        return problems;
    }
    if let Some((name, url)) = &state.renamed_to {
        problems.push(Problem::Renamed {
            name: name.clone(),
            url: url.clone(),
        });
    } else if state.not_found {
        problems.push(Problem::NotOnGitHub);
    } else if !state.listed {
        problems.push(Problem::NotListed);
    }
    if let Some((name, url)) = &state.linked_upstream {
        let has_upstream = clone.remotes.iter().any(|r| {
            r.parsed
                .as_ref()
                .is_some_and(|p| p.is_github() && p.full_name().eq_ignore_ascii_case(name))
        });
        if !has_upstream {
            problems.push(Problem::MissingUpstream {
                name: name.clone(),
                url: url.clone(),
            });
        }
    }
    problems
}

/// Scans the clone root and records the result in the database.
pub async fn scan(
    settings: &ScanSettings,
    db: Arc<Mutex<Db>>,
    progress: impl Fn(usize, usize),
) -> Result<ScanResult> {
    let started = Instant::now();
    let root = settings
        .root
        .clone()
        .ok_or_else(|| anyhow!("No clone root is set. Pick one in Settings."))?;

    let walked = {
        let root = root.clone();
        spawn_blocking(move || walk(&root)).await??
    };
    let picks = {
        let db = db.clone();
        spawn_blocking(move || lock(&db).main_remote_picks()).await??
    };

    let total = walked.repos.len();
    progress(0, total);
    let aliases: Arc<[String]> = settings.github_aliases.clone().into();
    let limit = Arc::new(Semaphore::new(PARALLEL));
    let mut tasks = JoinSet::new();
    for (index, found) in walked.repos.iter().enumerate() {
        let path = found.abs_path.clone();
        let aliases = aliases.clone();
        let limit = limit.clone();
        tasks.spawn(async move {
            let _permit = limit.acquire_owned().await;
            (index, inspect(&path, &aliases).await)
        });
    }
    let mut inspected: Vec<Option<Inspected>> = (0..total).map(|_| None).collect();
    let mut done = 0;
    while let Some(joined) = tasks.join_next().await {
        let (index, result) = joined.context("inspecting a repo crashed")?;
        inspected[index] = Some(result);
        done += 1;
        progress(done, total);
    }

    let mut clones: Vec<CloneInfo> = walked
        .repos
        .into_iter()
        .zip(inspected)
        .map(|(found, inspected)| {
            let Inspected { remotes, status } = inspected.expect("every repo was inspected");
            let (remotes, git_error) = match remotes {
                Ok(remotes) => (remotes, None),
                Err(e) => (Vec::new(), Some(e)),
            };
            let main = choose_main_remote(
                &remotes,
                &settings.default_remote,
                picks.get(&found.rel_path).map(String::as_str),
            );
            let problems = match git_error {
                Some(message) => vec![Problem::GitError { message }],
                None => find_problems(&found.rel_path, found.location, &remotes, main),
            };
            CloneInfo {
                id: 0,
                repo_id: None,
                rel_path: found.rel_path,
                abs_path: found.abs_path,
                remotes,
                main,
                status,
                problems,
            }
        })
        .collect();

    let (clones_back, applied) = spawn_blocking(move || {
        let scanned: Vec<db::ScannedClone<'_>> = clones
            .iter()
            .map(|clone| db::ScannedClone {
                path: &clone.rel_path,
                remotes: clone
                    .remotes
                    .iter()
                    .map(|r| db::ScannedRemote {
                        name: &r.name,
                        url: &r.url,
                        parsed: r.parsed.as_ref(),
                    })
                    .collect(),
                main_remote: match clone.main {
                    MainRemote::Chosen { index, .. } => Some(index),
                    _ => None,
                },
            })
            .collect();
        let mut db = lock(&db);
        let applied = db.apply_scan(&scanned)?;
        let states = db.repo_states()?;
        drop(scanned);
        anyhow::Ok((clones, (applied, states)))
    })
    .await??;
    let (applied, states) = applied;
    clones = clones_back;
    for ((clone, id), repo_id) in clones.iter_mut().zip(applied.ids).zip(applied.repo_ids) {
        clone.id = id;
        clone.repo_id = repo_id;
        if let (Some(states), Some(repo_id)) = (&states, repo_id)
            && let Some(state) = states.get(&repo_id)
        {
            clone.problems.extend(sync_problems(clone, state));
        }
    }

    Ok(ScanResult {
        root,
        clones,
        strays: walked.strays,
        missing: applied.missing,
        took: started.elapsed(),
    })
}

fn lock(db: &Mutex<Db>) -> std::sync::MutexGuard<'_, Db> {
    db.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Found {
    rel_path: String,
    abs_path: PathBuf,
    location: Location,
}

#[derive(Default)]
struct Walked {
    repos: Vec<Found>,
    strays: Vec<String>,
}

/// Looks two levels deep: letter folders, then the folders inside them.
/// Hidden entries at the top level and plain files are skipped.
fn walk(root: &Path) -> Result<Walked> {
    if !root.is_dir() {
        bail!(
            "The clone root {} doesn't exist or isn't a folder.",
            root.display()
        );
    }
    let mut walked = Walked::default();
    for (name, path) in subfolders(root)? {
        if name.starts_with('.') {
            continue;
        }
        if layout::is_letter_folder(&name) {
            let letter = name.chars().next().expect("letter folders have one char");
            let children = match subfolders(&path) {
                Ok(children) => children,
                Err(e) => {
                    tracing::warn!("{e:#}");
                    walked.strays.push(name);
                    continue;
                }
            };
            for (child, child_path) in children {
                let rel_path = format!("{name}/{child}");
                if is_git_repo(&child_path) {
                    walked.repos.push(Found {
                        rel_path,
                        abs_path: child_path,
                        location: Location::Letter(letter),
                    });
                } else {
                    walked.strays.push(rel_path);
                }
            }
        } else if is_git_repo(&path) {
            walked.repos.push(Found {
                rel_path: name,
                abs_path: path,
                location: Location::Root,
            });
        } else {
            walked.strays.push(name);
        }
    }
    Ok(walked)
}

/// Folder names (following symlinks) and paths, sorted by name.
fn subfolders(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let entries = fs::read_dir(dir).with_context(|| format!("couldn't read {}", dir.display()))?;
    let mut folders = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("couldn't read {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            folders.push((entry.file_name().to_string_lossy().into_owned(), path));
        }
    }
    folders.sort();
    Ok(folders)
}

fn is_git_repo(path: &Path) -> bool {
    path.join(".git").exists()
}

struct Inspected {
    remotes: Result<Vec<Remote>, String>,
    status: Result<Status, String>,
}

async fn inspect(path: &Path, github_aliases: &[String]) -> Inspected {
    let remotes = super::run(path, &["remote", "-v"])
        .await
        .map(|out| parse_remotes(&out, github_aliases))
        .map_err(|e| format!("{e:#}"));
    let status = super::run(
        path,
        &["status", "--porcelain=v2", "--branch", "--show-stash"],
    )
    .await
    .map(|out| status::parse(&out))
    .map_err(|e| format!("{e:#}"));
    Inspected { remotes, status }
}

/// Reads `git remote -v` (which already applies `insteadOf` rewrites). Uses fetch URLs.
pub fn parse_remotes(output: &str, github_aliases: &[String]) -> Vec<Remote> {
    let mut remotes: Vec<Remote> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    for line in output.lines() {
        let Some((name, rest)) = line.split_once('\t') else {
            continue;
        };
        let Some(url) = rest.strip_suffix(" (fetch)") else {
            continue;
        };
        if seen.insert(name.to_owned(), ()).is_some() {
            continue;
        }
        remotes.push(Remote {
            name: name.to_owned(),
            url: url.to_owned(),
            parsed: remote_url::parse(url, github_aliases),
        });
    }
    remotes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(name: &str, url: &str) -> Remote {
        Remote {
            name: name.into(),
            url: url.into(),
            parsed: remote_url::parse(url, &[]),
        }
    }

    #[test]
    fn main_remote_rules() {
        let me = remote("me", "git@github.com:me/thing.git");
        let origin = remote("origin", "git@github.com:org/thing.git");
        let upstream = remote("upstream", "git@github.com:up/thing.git");
        let fork = remote("fork", "git@github.com:other/thing.git");

        assert_eq!(choose_main_remote(&[], "me", None), MainRemote::NoRemotes);
        let chosen = |index| MainRemote::Chosen {
            index,
            picked_by_hand: false,
        };
        assert_eq!(
            choose_main_remote(&[upstream.clone(), me.clone()], "me", None),
            chosen(1)
        );
        assert_eq!(
            choose_main_remote(&[upstream.clone(), origin.clone()], "me", None),
            chosen(1)
        );
        assert_eq!(
            choose_main_remote(std::slice::from_ref(&fork), "me", None),
            chosen(0)
        );
        assert_eq!(
            choose_main_remote(&[fork.clone(), upstream.clone()], "me", None),
            MainRemote::Undecided
        );
        assert_eq!(
            choose_main_remote(&[fork.clone(), upstream.clone()], "me", Some("upstream")),
            MainRemote::Chosen {
                index: 1,
                picked_by_hand: true
            }
        );
        // A pick for a remote that no longer exists falls back to the rule.
        assert_eq!(
            choose_main_remote(&[me.clone(), fork], "me", Some("gone")),
            chosen(0)
        );
    }

    #[test]
    fn problems_for_well_placed_github_clone() {
        let remotes = [remote("me", "git@github.com:facebook/react.git")];
        let main = choose_main_remote(&remotes, "me", None);
        assert!(find_problems("r/react", Location::Letter('r'), &remotes, main).is_empty());
        assert!(find_problems("r/react~2", Location::Letter('r'), &remotes, main).is_empty());
        assert!(find_problems("r/React", Location::Letter('r'), &remotes, main).is_empty());
    }

    #[test]
    fn layout_problems() {
        let remotes = [remote("me", "git@github.com:facebook/react.git")];
        let main = choose_main_remote(&remotes, "me", None);
        let expected = vec![Problem::OutOfLayout {
            expected: "r/react".into(),
        }];
        assert_eq!(
            find_problems("react", Location::Root, &remotes, main),
            expected
        );
        assert_eq!(
            find_problems("x/react", Location::Letter('x'), &remotes, main),
            expected
        );
        assert_eq!(
            find_problems("R/react", Location::Letter('R'), &remotes, main),
            expected
        );
        assert_eq!(
            find_problems("r/react-old", Location::Letter('r'), &remotes, main),
            expected
        );
    }

    #[test]
    fn remote_problems() {
        assert_eq!(
            find_problems("t/thing", Location::Letter('t'), &[], MainRemote::NoRemotes),
            vec![Problem::NoRemotes]
        );

        let gitlab = [remote("origin", "git@gitlab.com:group/thing.git")];
        assert_eq!(
            find_problems(
                "t/thing",
                Location::Letter('t'),
                &gitlab,
                choose_main_remote(&gitlab, "me", None)
            ),
            vec![Problem::OtherHost {
                host: "gitlab.com".into()
            }]
        );

        let local = [remote("origin", "/srv/git/thing.git")];
        assert_eq!(
            find_problems(
                "t/thing",
                Location::Letter('t'),
                &local,
                choose_main_remote(&local, "me", None)
            ),
            vec![Problem::NotHosted {
                url: "/srv/git/thing.git".into()
            }]
        );

        let two = [
            remote("a", "git@github.com:x/thing.git"),
            remote("b", "git@github.com:y/thing.git"),
        ];
        assert_eq!(
            find_problems(
                "t/thing",
                Location::Letter('t'),
                &two,
                MainRemote::Undecided
            ),
            vec![Problem::NoMainRemote]
        );
    }

    #[test]
    fn problems_from_the_last_sync() {
        let remotes = vec![remote("me", "git@github.com:me/react.git")];
        let clone = CloneInfo {
            id: 1,
            repo_id: Some(1),
            rel_path: "r/react".into(),
            abs_path: "/code/r/react".into(),
            main: choose_main_remote(&remotes, "me", None),
            remotes,
            status: Ok(Status::default()),
            problems: vec![],
        };
        let listed = RepoState {
            listed: true,
            ..Default::default()
        };
        assert!(sync_problems(&clone, &listed).is_empty());
        assert_eq!(
            sync_problems(&clone, &RepoState::default()),
            [Problem::NotListed]
        );
        assert_eq!(
            sync_problems(
                &clone,
                &RepoState {
                    not_found: true,
                    ..Default::default()
                }
            ),
            [Problem::NotOnGitHub]
        );
        let up = (
            "facebook/react".to_owned(),
            "git@github.com:facebook/react.git".to_owned(),
        );
        assert_eq!(
            sync_problems(
                &clone,
                &RepoState {
                    listed: true,
                    linked_upstream: Some(up.clone()),
                    ..Default::default()
                }
            ),
            [Problem::MissingUpstream {
                name: up.0.clone(),
                url: up.1.clone()
            }]
        );
        let mut with_upstream = clone.clone();
        with_upstream
            .remotes
            .push(remote("upstream", "https://github.com/Facebook/React"));
        assert!(
            sync_problems(
                &with_upstream,
                &RepoState {
                    listed: true,
                    linked_upstream: Some(up),
                    ..Default::default()
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn remote_list_parsing() {
        let out = "me\tgit@github.com:me/x.git (fetch)\nme\tgit@github.com:me/x.git (push)\n\
                   up\thttps://github.com/up/x (fetch)\nup\tno_push (push)\n";
        let remotes = parse_remotes(out, &[]);
        assert_eq!(remotes.len(), 2);
        assert_eq!(remotes[0].name, "me");
        assert_eq!(remotes[1].url, "https://github.com/up/x");
        assert_eq!(remotes[1].parsed.as_ref().unwrap().owner, "up");
    }

    /// Builds a real clone root with git and scans it.
    #[tokio::test]
    async fn scans_a_real_folder() {
        let root = std::env::temp_dir().join(format!("repodepot-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let git = |dir: &Path, args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(status.status.success(), "{status:?}");
        };
        let make_repo = |rel: &str, remotes: &[(&str, &str)]| {
            let dir = root.join(rel);
            fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q", "-b", "main"]);
            for (name, url) in remotes {
                git(&dir, &["remote", "add", name, url]);
            }
            dir
        };

        make_repo("r/react", &[("me", "git@github.com:facebook/react.git")]);
        let dirty = make_repo("n/notes", &[]);
        // An unusual name, so no global gitignore hides it.
        fs::write(dirty.join("repodepot-untracked-c7f3"), "x").unwrap();
        make_repo(
            "t/thing",
            &[
                ("a", "git@github.com:x/thing.git"),
                ("b", "git@github.com:y/thing.git"),
            ],
        );
        make_repo(
            "misplaced",
            &[("origin", "https://github.com/someone/misplaced")],
        );
        make_repo("_/.github", &[("me", "git@github.com:someone/.github.git")]);
        fs::create_dir_all(root.join("p/photos")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join("r/.directory"), "").unwrap();

        let db = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        let settings = ScanSettings {
            root: Some(root.clone()),
            default_remote: "me".into(),
            github_aliases: Vec::new(),
        };
        let result = scan(&settings, db.clone(), |_, _| {}).await.unwrap();
        fs::remove_dir_all(&root).unwrap();

        let paths: Vec<&str> = result.clones.iter().map(|c| c.rel_path.as_str()).collect();
        assert_eq!(
            paths,
            ["_/.github", "misplaced", "n/notes", "r/react", "t/thing"]
        );
        assert_eq!(result.strays, ["p/photos"]);
        assert!(result.missing.is_empty());

        let by_path = |p: &str| result.clones.iter().find(|c| c.rel_path == p).unwrap();
        assert!(by_path("r/react").problems.is_empty());
        assert_eq!(by_path("r/react").display_name(), "facebook/react");
        assert!(by_path("_/.github").problems.is_empty());
        assert_eq!(
            by_path("misplaced").problems,
            vec![Problem::OutOfLayout {
                expected: "m/misplaced".into()
            }]
        );
        assert_eq!(by_path("t/thing").problems, vec![Problem::NoMainRemote]);
        let notes = by_path("n/notes");
        assert_eq!(notes.problems, vec![Problem::NoRemotes]);
        let status = notes.status.as_ref().unwrap();
        assert_eq!(status.untracked, 1);
        assert_eq!(status.head, None);
        assert!(result.clones.iter().all(|c| c.id > 0));
    }
}
