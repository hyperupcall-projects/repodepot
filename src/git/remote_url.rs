//! Turning remote URLs into host + owner + name.

pub const GITHUB: &str = "github.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    /// Lowercased. SSH aliases for GitHub are mapped to `github.com`.
    pub host: String,
    /// For hosts with nested groups (GitLab), everything before the last segment.
    pub owner: String,
    pub name: String,
}

impl RemoteUrl {
    pub fn is_github(&self) -> bool {
        self.host == GITHUB
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Understands `git@host:o/r(.git)`, `ssh://[user@]host[:port]/o/r`, `https://host/o/r(.git)`,
/// `git://…`, and `git+ssh://…`. Local paths and `file://` URLs give `None`.
/// `github_aliases` are SSH host aliases (from `~/.ssh/config`) that mean github.com.
pub fn parse(url: &str, github_aliases: &[String]) -> Option<RemoteUrl> {
    let (host, path) = split_host_and_path(url.trim())?;
    let host = normalize_host(host, github_aliases);

    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    let (owner, name) = path.rsplit_once('/')?;
    if owner.is_empty() || name.is_empty() || (host == GITHUB && owner.contains('/')) {
        return None;
    }
    Some(RemoteUrl {
        host,
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}

fn split_host_and_path(url: &str) -> Option<(&str, &str)> {
    if let Some((scheme, rest)) = url.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(
            scheme.as_str(),
            "ssh" | "git+ssh" | "ssh+git" | "https" | "http" | "git"
        ) {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host_and_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = match host_and_port.rsplit_once(':') {
            Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
            _ => host_and_port,
        };
        return (!host.is_empty()).then_some((host, path));
    }

    // scp-like syntax: [user@]host:path. A '/' before the ':' means it's a local path.
    let (before, path) = url.split_once(':')?;
    if before.is_empty() || before.contains('/') {
        return None;
    }
    let host = before.rsplit_once('@').map_or(before, |(_, h)| h);
    (!host.is_empty()).then_some((host, path))
}

fn normalize_host(host: &str, github_aliases: &[String]) -> String {
    let lower = host.to_ascii_lowercase();
    let is_github = matches!(
        lower.as_str(),
        "github.com" | "www.github.com" | "ssh.github.com"
    ) || github_aliases.iter().any(|a| a.eq_ignore_ascii_case(host));
    if is_github { GITHUB.to_owned() } else { lower }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh(owner: &str, name: &str) -> Option<RemoteUrl> {
        Some(RemoteUrl {
            host: GITHUB.into(),
            owner: owner.into(),
            name: name.into(),
        })
    }

    fn p(url: &str) -> Option<RemoteUrl> {
        parse(url, &["github-work".to_owned()])
    }

    #[test]
    fn github_forms() {
        assert_eq!(
            p("git@github.com:facebook/react.git"),
            gh("facebook", "react")
        );
        assert_eq!(p("git@github.com:facebook/react"), gh("facebook", "react"));
        assert_eq!(
            p("git@github.com:/facebook/react.git/"),
            gh("facebook", "react")
        );
        assert_eq!(
            p("https://github.com/facebook/react"),
            gh("facebook", "react")
        );
        assert_eq!(
            p("https://github.com/facebook/react.git"),
            gh("facebook", "react")
        );
        assert_eq!(
            p("https://github.com/facebook/react/"),
            gh("facebook", "react")
        );
        assert_eq!(
            p("http://www.github.com/facebook/react"),
            gh("facebook", "react")
        );
        assert_eq!(
            p("https://user:token@github.com/facebook/react.git"),
            gh("facebook", "react")
        );
        assert_eq!(
            p("ssh://git@github.com/facebook/react.git"),
            gh("facebook", "react")
        );
        assert_eq!(
            p("ssh://git@ssh.github.com:443/facebook/react.git"),
            gh("facebook", "react")
        );
        assert_eq!(
            p("git+ssh://git@github.com/facebook/react"),
            gh("facebook", "react")
        );
        assert_eq!(
            p("git://github.com/facebook/react.git"),
            gh("facebook", "react")
        );
        assert_eq!(
            p("  GIT@GitHub.com:Facebook/React.git\n"),
            gh("Facebook", "React")
        );
    }

    #[test]
    fn dotted_names_keep_their_dots() {
        assert_eq!(
            p("git@github.com:someone/.github.git"),
            gh("someone", ".github")
        );
        assert_eq!(
            p("git@github.com:someone/site.io"),
            gh("someone", "site.io")
        );
    }

    #[test]
    fn github_aliases() {
        assert_eq!(p("git@github-work:corp/api.git"), gh("corp", "api"));
        assert_eq!(p("ssh://git@GITHUB-WORK/corp/api"), gh("corp", "api"));
        assert_eq!(
            parse("git@github-work:corp/api.git", &[]).unwrap().host,
            "github-work"
        );
    }

    #[test]
    fn other_hosts() {
        let gitlab = p("git@gitlab.com:group/sub/project.git").unwrap();
        assert_eq!(gitlab.host, "gitlab.com");
        assert_eq!(gitlab.owner, "group/sub");
        assert_eq!(gitlab.name, "project");
        assert!(!gitlab.is_github());

        let codeberg = p("https://codeberg.org/someone/thing").unwrap();
        assert_eq!(codeberg.full_name(), "someone/thing");
    }

    #[test]
    fn not_hosted() {
        assert_eq!(p("/home/me/repos/thing"), None);
        assert_eq!(p("../thing"), None);
        assert_eq!(p("./a:b"), None);
        assert_eq!(p("file:///home/me/thing.git"), None);
        assert_eq!(p("git@github.com:just-one-segment"), None);
        assert_eq!(p("https://github.com/a/b/c"), None);
        assert_eq!(p(""), None);
    }
}
