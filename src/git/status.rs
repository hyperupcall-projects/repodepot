//! Reading `git status --porcelain=v2 --branch --show-stash`.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// `None` on a detached HEAD.
    pub branch: Option<String>,
    /// `None` when the branch has no commits yet.
    pub head: Option<String>,
    pub upstream: Option<String>,
    /// Commits (ahead, behind) the upstream, as of the last fetch.
    /// `None` when there's no upstream or it no longer exists.
    pub ahead_behind: Option<(u32, u32)>,
    /// Changed tracked files (staged or not).
    pub changed: u32,
    pub untracked: u32,
    pub conflicts: u32,
    pub stashes: u32,
}

impl Status {
    pub fn total_changes(&self) -> u32 {
        self.changed + self.untracked + self.conflicts
    }

    /// An upstream is set, but the branch it points at is gone.
    pub fn upstream_gone(&self) -> bool {
        self.upstream.is_some() && self.ahead_behind.is_none()
    }

    pub fn out_of_sync(&self) -> bool {
        self.upstream_gone() || self.ahead_behind.is_some_and(|(a, b)| a > 0 || b > 0)
    }
}

pub fn parse(text: &str) -> Status {
    let mut status = Status::default();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" => status.head = (value != "(initial)").then(|| value.to_owned()),
                "branch.head" => status.branch = (value != "(detached)").then(|| value.to_owned()),
                "branch.upstream" => status.upstream = Some(value.to_owned()),
                "branch.ab" => status.ahead_behind = parse_ahead_behind(value),
                "stash" => status.stashes = value.parse().unwrap_or(0),
                _ => {}
            }
        } else {
            match line.as_bytes().first() {
                Some(b'1' | b'2') => status.changed += 1,
                Some(b'u') => status.conflicts += 1,
                Some(b'?') => status.untracked += 1,
                _ => {}
            }
        }
    }
    status
}

fn parse_ahead_behind(value: &str) -> Option<(u32, u32)> {
    let (ahead, behind) = value.split_once(' ')?;
    Some((
        ahead.strip_prefix('+')?.parse().ok()?,
        behind.strip_prefix('-')?.parse().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_branch_in_sync() {
        let s = parse(
            "# branch.oid 1234abcd\n# branch.head main\n# branch.upstream me/main\n# branch.ab +0 -0\n",
        );
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.head.as_deref(), Some("1234abcd"));
        assert_eq!(s.upstream.as_deref(), Some("me/main"));
        assert_eq!(s.ahead_behind, Some((0, 0)));
        assert_eq!(s.total_changes(), 0);
        assert!(!s.out_of_sync());
    }

    #[test]
    fn dirty_and_out_of_sync() {
        let s = parse(concat!(
            "# branch.oid 1234abcd\n",
            "# branch.head feature\n",
            "# branch.upstream me/feature\n",
            "# branch.ab +2 -5\n",
            "# stash 3\n",
            "1 .M N... 100644 100644 100644 aaa bbb src/main.rs\n",
            "1 M. N... 100644 100644 100644 aaa bbb Cargo.toml\n",
            "2 R. N... 100644 100644 100644 aaa bbb R100 new.rs\told.rs\n",
            "u UU N... 100644 100644 100644 100644 aaa bbb ccc conflict.rs\n",
            "? notes.txt\n",
            "? scratch/\n",
        ));
        assert_eq!(s.ahead_behind, Some((2, 5)));
        assert_eq!(s.changed, 3);
        assert_eq!(s.conflicts, 1);
        assert_eq!(s.untracked, 2);
        assert_eq!(s.stashes, 3);
        assert!(s.out_of_sync());
    }

    #[test]
    fn detached_head_without_upstream() {
        let s = parse("# branch.oid 1234abcd\n# branch.head (detached)\n");
        assert_eq!(s.branch, None);
        assert_eq!(s.upstream, None);
        assert_eq!(s.ahead_behind, None);
        assert!(!s.upstream_gone());
    }

    #[test]
    fn new_repo_without_commits() {
        let s = parse("# branch.oid (initial)\n# branch.head main\n? README.md\n");
        assert_eq!(s.head, None);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.untracked, 1);
    }

    #[test]
    fn upstream_that_was_deleted() {
        let s = parse("# branch.oid 1234abcd\n# branch.head old\n# branch.upstream me/old\n");
        assert!(s.upstream_gone());
        assert!(s.out_of_sync());
    }
}
