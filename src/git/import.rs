//! Import: finding git repos in a folder and planning where each goes in the layout.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::scan::{MainRemote, choose_main_remote, parse_remotes};
use crate::layout;

/// Folders that never hold repos worth importing, and can be huge.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "bower_components",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".tox",
];
const MAX_DEPTH: usize = 8;
const MAX_FOLDERS: usize = 50_000;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    pub repos: Vec<PathBuf>,
    /// Folders that look like repos but can't be moved this way, and why.
    pub skipped: Vec<(PathBuf, String)>,
    /// The search stopped early because the folder tree was too big.
    pub truncated: bool,
}

enum GitKind {
    /// A normal repo: `.git` is a folder.
    Repo,
    /// `.git` is a file: a linked worktree or a submodule checkout.
    Linked,
}

fn git_kind(dir: &Path) -> Option<GitKind> {
    let dot_git = dir.join(".git");
    let meta = fs::symlink_metadata(&dot_git).ok()?;
    Some(if meta.is_dir() {
        GitKind::Repo
    } else {
        GitKind::Linked
    })
}

/// Finds repos under `start` (or `start` itself). Doesn't look inside a repo once found,
/// doesn't follow symlinks, skips dependency folders, and only looks at hidden folders
/// that are themselves repos (like `~/.dotfiles`).
pub fn discover(start: &Path) -> Result<Discovery> {
    if !start.is_dir() {
        bail!("{} isn't a folder.", start.display());
    }
    let mut found = Discovery::default();
    let linked_reason = "a linked worktree or submodule (move it with git worktree move)";
    let mut stack = vec![(start.to_path_buf(), 0)];
    let mut visited = 0;
    while let Some((dir, depth)) = stack.pop() {
        visited += 1;
        if visited > MAX_FOLDERS {
            found.truncated = true;
            break;
        }
        match git_kind(&dir) {
            Some(GitKind::Repo) => {
                found.repos.push(dir);
                continue;
            }
            Some(GitKind::Linked) => {
                found.skipped.push((dir, linked_reason.to_owned()));
                continue;
            }
            None => {}
        }
        if depth >= MAX_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut children = Vec::new();
        for entry in entries.flatten() {
            // file_type() doesn't follow symlinks, so symlinked folders are skipped.
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            let path = entry.path();
            if name.starts_with('.') {
                if matches!(git_kind(&path), Some(GitKind::Repo)) {
                    found.repos.push(path);
                }
                continue;
            }
            children.push(path);
        }
        children.sort();
        stack.extend(children.into_iter().rev().map(|c| (c, depth + 1)));
    }
    found.repos.sort();
    Ok(found)
}

/// One repo ready to move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportItem {
    pub source: PathBuf,
    /// `owner/name` from the main remote, or the folder name.
    pub name: String,
    /// Relative to the clone root, e.g. `r/react~2`.
    pub destination: String,
    /// It has an `origin` remote and none with the default remote name.
    pub can_rename_origin: bool,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportPreview {
    /// The folder that was searched, if any.
    pub searched: Option<PathBuf>,
    pub items: Vec<ImportItem>,
    /// Repos already where the layout puts them.
    pub in_place: usize,
    pub skipped: Vec<(PathBuf, String)>,
    pub truncated: bool,
}

/// What planning needs to know about the clone root.
pub struct PlanContext {
    pub root: PathBuf,
    pub default_remote: String,
    pub github_aliases: Vec<String>,
    /// Paths of clones the database knows, relative to the root.
    pub existing_paths: Vec<String>,
    /// Clone paths by lowercase `host/owner/name` of their main repo.
    pub clones_by_repo: HashMap<String, Vec<String>>,
}

/// Reads each repo's remotes and assigns it a free destination.
pub async fn plan(
    discovery: Discovery,
    searched: Option<PathBuf>,
    cx: &PlanContext,
) -> ImportPreview {
    let mut preview = ImportPreview {
        searched,
        skipped: discovery.skipped,
        truncated: discovery.truncated,
        ..Default::default()
    };
    let mut taken: HashSet<String> = cx.existing_paths.iter().map(|p| p.to_lowercase()).collect();
    let root_device = fs::metadata(&cx.root).map(|m| m.dev()).ok();

    for source in discovery.repos {
        if source == cx.root {
            preview
                .skipped
                .push((source, "this is the clone root itself".to_owned()));
            continue;
        }
        let remotes = match super::run(&source, &["remote", "-v"]).await {
            Ok(output) => parse_remotes(&output, &cx.github_aliases),
            Err(e) => {
                preview
                    .skipped
                    .push((source, format!("git couldn't read it: {e:#}")));
                continue;
            }
        };
        let folder_name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let main = choose_main_remote(&remotes, &cx.default_remote, None);
        let repo = match main {
            MainRemote::Chosen { index, .. } => remotes[index].parsed.clone(),
            _ => None,
        };
        let mut notes = Vec::new();
        match (&main, &repo) {
            (MainRemote::NoRemotes, _) => notes.push("No remotes".to_owned()),
            (MainRemote::Undecided, _) => {
                notes.push(
                    "Several remotes and none is the main one; named after the folder".to_owned(),
                );
            }
            (MainRemote::Chosen { .. }, None) => {
                notes.push("Main remote isn't a hosted repo; named after the folder".to_owned());
            }
            (_, Some(repo)) if !repo.is_github() => notes.push(format!("On {}", repo.host)),
            _ => {}
        }
        let name = repo
            .as_ref()
            .map_or(folder_name.clone(), |r| r.name.clone());
        if name.is_empty() {
            preview
                .skipped
                .push((source, "couldn't work out a name".to_owned()));
            continue;
        }

        // Already in the layout?
        if let Ok(rel) = source.strip_prefix(&cx.root) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            if layout::is_in_place(&rel, &name) {
                preview.in_place += 1;
                continue;
            }
        }

        if let Some(repo) = &repo {
            let key = format!("{}/{}/{}", repo.host, repo.owner, repo.name).to_lowercase();
            let source_rel = source
                .strip_prefix(&cx.root)
                .map(|r| r.to_string_lossy().into_owned())
                .ok();
            let others: Vec<&String> = cx
                .clones_by_repo
                .get(&key)
                .into_iter()
                .flatten()
                .filter(|p| Some(p.as_str()) != source_rel.as_deref())
                .collect();
            if let Some(other) = others.first() {
                notes.push(format!("You already have a clone of it at {other}"));
            }
        }
        if fs::read_dir(source.join(".git/worktrees")).is_ok_and(|mut d| d.next().is_some()) {
            notes.push("Has linked worktrees; they'll be repaired after the move".to_owned());
        }
        let source_device = fs::metadata(&source).map(|m| m.dev()).ok();
        if root_device.is_some() && source_device != root_device {
            notes.push(
                "On another disk, so it will be copied and then the original removed".to_owned(),
            );
        }

        let destination = layout::assign_path(&name, |candidate| {
            taken.contains(&candidate.to_lowercase())
                || layout::exists_ignoring_case(&cx.root, candidate)
        });
        taken.insert(destination.to_lowercase());
        let has = |n: &str| remotes.iter().any(|r| r.name == n);
        preview.items.push(ImportItem {
            name: repo.as_ref().map_or(folder_name, |r| r.full_name()),
            destination,
            can_rename_origin: cx.default_remote != "origin"
                && has("origin")
                && !has(&cx.default_remote),
            notes,
            source,
        });
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp(test: &str) -> Dir {
        let dir =
            std::env::temp_dir().join(format!("repodepot-import-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    fn repo(path: &Path, remotes: &[(&str, &str)]) {
        fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q"]);
        for (name, url) in remotes {
            git(path, &["remote", "add", name, url]);
        }
    }

    #[test]
    fn discovery_finds_repos_and_skips_the_right_things() {
        let dir = temp("discover");
        let base = &dir.0;
        repo(&base.join("a/alpha"), &[]);
        repo(&base.join("a/alpha/nested"), &[]); // inside a repo: not searched
        repo(&base.join("deep/er/beta"), &[]);
        repo(&base.join("web/node_modules/pkg"), &[]); // dependency folder: skipped
        repo(&base.join(".dotfiles"), &[]); // hidden, but itself a repo
        repo(&base.join(".config/tool"), &[]); // inside a hidden folder: not searched
        fs::create_dir_all(base.join("wt")).unwrap();
        fs::write(base.join("wt/.git"), "gitdir: /elsewhere\n").unwrap();
        std::os::unix::fs::symlink(base.join("a"), base.join("link-to-a")).unwrap();

        let found = discover(base).unwrap();
        let rel: Vec<String> = found
            .repos
            .iter()
            .map(|p| p.strip_prefix(base).unwrap().display().to_string())
            .collect();
        assert_eq!(rel, [".dotfiles", "a/alpha", "deep/er/beta"]);
        assert_eq!(found.skipped.len(), 1);
        assert!(found.skipped[0].0.ends_with("wt"));
        assert!(!found.truncated);

        // Pointing at a repo finds just that repo.
        assert_eq!(
            discover(&base.join("a/alpha")).unwrap().repos,
            [base.join("a/alpha")]
        );
        assert!(discover(&base.join("missing")).is_err());
    }

    #[tokio::test]
    async fn planning_names_destinations_and_notes() {
        let dir = temp("plan");
        let root = dir.0.join("code");
        let old = dir.0.join("old");
        fs::create_dir_all(root.join("r/react")).unwrap(); // already taken on disk
        repo(
            &root.join("s/serde"),
            &[("me", "git@github.com:serde-rs/serde.git")],
        ); // in place
        repo(
            &root.join("misplaced"),
            &[("origin", "git@github.com:me/misplaced.git")],
        );
        repo(
            &old.join("react"),
            &[("origin", "https://github.com/facebook/react")],
        );
        repo(&old.join("notes"), &[]);
        repo(
            &old.join("lab"),
            &[("origin", "git@gitlab.com:group/lab.git")],
        );
        repo(
            &old.join("Two"),
            &[
                ("a", "git@github.com:x/two.git"),
                ("b", "git@github.com:y/two.git"),
            ],
        );
        repo(
            &old.join("dupe"),
            &[("origin", "git@github.com:serde-rs/serde.git")],
        );

        let cx = PlanContext {
            root: root.clone(),
            default_remote: "me".into(),
            github_aliases: Vec::new(),
            existing_paths: vec!["s/serde".into()],
            clones_by_repo: HashMap::from([(
                "github.com/serde-rs/serde".into(),
                vec!["s/serde".into()],
            )]),
        };
        let mut found = discover(&old).unwrap();
        found.repos.extend(discover(&root).unwrap().repos);
        let preview = plan(found, Some(old.clone()), &cx).await;

        assert_eq!(preview.in_place, 1, "s/serde is already in place");
        let item = |name: &str| preview.items.iter().find(|i| i.name == name).unwrap();
        assert_eq!(item("facebook/react").destination, "r/react~2");
        assert!(item("facebook/react").can_rename_origin);
        assert_eq!(item("me/misplaced").destination, "m/misplaced");
        assert_eq!(item("notes").destination, "n/notes");
        assert_eq!(item("notes").notes, ["No remotes"]);
        assert!(!item("notes").can_rename_origin);
        assert_eq!(item("group/lab").notes, ["On gitlab.com"]);
        assert_eq!(item("Two").destination, "t/Two");
        assert!(item("Two").notes[0].starts_with("Several remotes"));
        // A second clone of serde: placed at ~2, with a note about the first.
        assert_eq!(item("serde-rs/serde").destination, "s/serde~2");
        assert_eq!(
            item("serde-rs/serde").notes,
            ["You already have a clone of it at s/serde"]
        );
    }
}
