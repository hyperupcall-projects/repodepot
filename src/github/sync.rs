//! Fetching every repo that counts as relevant, from each source.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::task::JoinSet;

use super::graphql_with_warnings;
use crate::config::Sources;
use crate::db::{FetchedRepo, RepoMeta, Source};

const REPO_FRAGMENTS: &str = r#"
fragment RepoFields on Repository {
  id
  name
  owner { login }
  description
  isFork
  isArchived
  isPrivate
  pushedAt
  sshUrl
  stargazerCount
  primaryLanguage { name }
  repositoryTopics(first: 20) { nodes { topic { name } } }
}
fragment RepoWithParent on Repository {
  ...RepoFields
  parent { ...RepoFields }
}
"#;

const VIEWER_REPOS: &str = r#"
query ($affiliation: RepositoryAffiliation!, $cursor: String) {
  viewer {
    repositories(first: 100, after: $cursor, affiliations: [$affiliation],
                 ownerAffiliations: [OWNER, ORGANIZATION_MEMBER, COLLABORATOR]) {
      pageInfo { hasNextPage endCursor }
      nodes { ...RepoWithParent }
    }
  }
}
"#;

const CONTRIBUTION_YEARS: &str =
    "query { viewer { contributionsCollection { contributionYears } } }";

const CONTRIBUTIONS: &str = r#"
query ($from: DateTime!, $to: DateTime!) {
  viewer {
    contributionsCollection(from: $from, to: $to) {
      commitContributionsByRepository(maxRepositories: 100) { repository { ...RepoWithParent } }
      issueContributionsByRepository(maxRepositories: 100) { repository { ...RepoWithParent } }
      pullRequestContributionsByRepository(maxRepositories: 100) { repository { ...RepoWithParent } }
      pullRequestReviewContributionsByRepository(maxRepositories: 100) { repository { ...RepoWithParent } }
    }
  }
}
"#;

const OWNER_REPOS: &str = r#"
query ($login: String!, $cursor: String) {
  repositoryOwner(login: $login) {
    repositories(first: 100, after: $cursor, ownerAffiliations: [OWNER]) {
      pageInfo { hasNextPage endCursor }
      nodes { ...RepoWithParent }
    }
  }
}
"#;

const ONE_REPO: &str = r#"
query ($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) { ...RepoWithParent }
}
"#;

const OWNER_EXISTS: &str = "query ($login: String!) { repositoryOwner(login: $login) { login } }";

fn with_fragments(query: &str) -> String {
    format!("{REPO_FRAGMENTS}\n{query}")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRepo {
    id: String,
    name: String,
    owner: GhLogin,
    description: Option<String>,
    is_fork: bool,
    is_archived: bool,
    is_private: bool,
    pushed_at: Option<String>,
    ssh_url: Option<String>,
    #[serde(default)]
    stargazer_count: u32,
    primary_language: Option<GhName>,
    repository_topics: Option<GhTopics>,
    #[serde(default)]
    parent: Option<Box<GhRepo>>,
}

#[derive(Deserialize)]
struct GhLogin {
    login: String,
}

#[derive(Deserialize)]
struct GhName {
    name: String,
}

#[derive(Deserialize)]
struct GhTopics {
    nodes: Vec<Option<GhTopicNode>>,
}

#[derive(Deserialize)]
struct GhTopicNode {
    topic: GhName,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    page_info: PageInfo,
    nodes: Vec<Option<GhRepo>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

impl GhRepo {
    fn meta(&self) -> RepoMeta {
        RepoMeta {
            node_id: self.id.clone(),
            owner: self.owner.login.clone(),
            name: self.name.clone(),
            description: self.description.clone().filter(|d| !d.is_empty()),
            language: self.primary_language.as_ref().map(|l| l.name.clone()),
            topics: self
                .repository_topics
                .iter()
                .flat_map(|t| t.nodes.iter().flatten())
                .map(|n| n.topic.name.clone())
                .collect(),
            is_fork: self.is_fork,
            is_archived: self.is_archived,
            is_private: self.is_private,
            pushed_at: self.pushed_at.clone(),
            ssh_url: self.ssh_url.clone(),
            stars: self.stargazer_count,
        }
    }
}

/// Which sources to read, plus what was added by hand.
pub struct SyncRequest {
    pub sources: Sources,
    pub manual_repos: Vec<(String, String)>,
    pub manual_owners: Vec<String>,
}

pub struct Fetched {
    pub repos: Vec<FetchedRepo>,
    /// How many repos each source listed.
    pub counts: BTreeMap<Source, usize>,
    /// Problems that didn't stop the sync (SSO-protected orgs, repos added by hand that are gone).
    pub warnings: Vec<String>,
}

#[derive(Default)]
struct Collector {
    repos: HashMap<String, FetchedRepo>,
    counts: BTreeMap<Source, usize>,
    warnings: Vec<String>,
}

impl Collector {
    fn add(&mut self, repo: &GhRepo, source: Source) {
        let entry = self
            .repos
            .entry(repo.id.clone())
            .or_insert_with(|| FetchedRepo {
                meta: repo.meta(),
                parent: repo.parent.as_ref().map(|p| p.meta()),
                sources: BTreeSet::new(),
            });
        if entry.sources.insert(source) {
            *self.counts.entry(source).or_default() += 1;
        }
    }

    fn absorb(&mut self, part: Part, source: Source) {
        for repo in &part.repos {
            self.add(repo, source);
        }
        self.warn(part.warnings);
    }

    fn warn(&mut self, warnings: Vec<String>) {
        for warning in warnings {
            if !self.warnings.contains(&warning) {
                self.warnings.push(warning);
            }
        }
    }
}

/// Repos read from one source, before they're merged.
#[derive(Default)]
struct Part {
    repos: Vec<GhRepo>,
    warnings: Vec<String>,
    /// False when the connection's parent was null (e.g. an owner that doesn't exist).
    found: bool,
}

struct Client<'a> {
    http: &'a reqwest::Client,
    token: &'a str,
}

impl Client<'_> {
    async fn query(&self, query: &str, variables: Value) -> Result<(Value, Vec<String>)> {
        graphql_with_warnings(self.http, self.token, query, variables).await
    }

    /// Follows a paginated repository connection found at `page_path` in the response.
    async fn paginate(
        &self,
        query: &str,
        mut variables: Value,
        page_path: &str,
        progress: &(impl Fn(String) + Sync + ?Sized),
        what: &str,
    ) -> Result<Part> {
        let query = with_fragments(query);
        let mut part = Part::default();
        for _ in 0..100 {
            let (data, warnings) = self.query(&query, variables.clone()).await?;
            part.warnings.extend(warnings);
            let Some(page) = data.pointer(page_path).filter(|p| !p.is_null()) else {
                return Ok(part);
            };
            part.found = true;
            let page: Page = serde_json::from_value(page.clone())
                .context("GitHub sent a page of repos this app doesn't understand")?;
            part.repos.extend(page.nodes.into_iter().flatten());
            progress(format!("{what}: {} so far", part.repos.len()));
            if !page.page_info.has_next_page {
                break;
            }
            variables["cursor"] = json!(page.page_info.end_cursor);
        }
        Ok(part)
    }

    /// Every contribution year, queried at once (each year is a separate query).
    async fn contributions(&self, progress: &(impl Fn(String) + Sync + ?Sized)) -> Result<Part> {
        let (data, warnings) = self.query(CONTRIBUTION_YEARS, json!({})).await?;
        let mut part = Part {
            warnings,
            found: true,
            ..Default::default()
        };
        let years: Vec<i64> = serde_json::from_value(
            data.pointer("/viewer/contributionsCollection/contributionYears")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )?;
        let (this_year, now) = utc_now();
        let query = with_fragments(CONTRIBUTIONS);
        let mut tasks = JoinSet::new();
        for year in &years {
            let to = if *year >= this_year {
                now.clone()
            } else {
                format!("{year}-12-31T23:59:59Z")
            };
            let variables = json!({ "from": format!("{year}-01-01T00:00:00Z"), "to": to });
            let (http, token, query) = (self.http.clone(), self.token.to_owned(), query.clone());
            tasks.spawn(async move {
                graphql_with_warnings::<Value>(&http, &token, &query, variables).await
            });
        }
        let mut done = 0;
        while let Some(joined) = tasks.join_next().await {
            let (data, warnings) = joined.context("a contributions query crashed")??;
            part.warnings.extend(warnings);
            done += 1;
            progress(format!("Contributions: {done} of {} years", years.len()));
            let Some(collection) = data.pointer("/viewer/contributionsCollection") else {
                continue;
            };
            for kind in [
                "commitContributionsByRepository",
                "issueContributionsByRepository",
                "pullRequestContributionsByRepository",
                "pullRequestReviewContributionsByRepository",
            ] {
                for entry in collection[kind].as_array().into_iter().flatten() {
                    if let Ok(repo) = serde_json::from_value::<GhRepo>(entry["repository"].clone())
                    {
                        part.repos.push(repo);
                    }
                }
            }
        }
        Ok(part)
    }

    async fn one_repo(&self, owner: &str, name: &str) -> Result<(Option<GhRepo>, Vec<String>)> {
        let (data, warnings) = self
            .query(
                &with_fragments(ONE_REPO),
                json!({ "owner": owner, "name": name }),
            )
            .await?;
        let repo = match data.get("repository") {
            Some(Value::Null) | None => None,
            Some(repo) => Some(serde_json::from_value(repo.clone())?),
        };
        Ok((repo, warnings))
    }
}

/// Reads every enabled source. Fails only if GitHub can't be reached or rejects the token;
/// smaller problems become warnings.
pub async fn fetch_all(
    http: &reqwest::Client,
    token: &str,
    request: &SyncRequest,
    progress: &(impl Fn(String) + Sync + ?Sized),
) -> Result<Fetched> {
    let client = Client { http, token };
    let mut collector = Collector::default();

    // The sources are independent, so they're read at the same time.
    let viewer = |enabled: bool, affiliation: &'static str, what: &'static str| {
        let client = &client;
        async move {
            if !enabled {
                return Ok(Part::default());
            }
            client
                .paginate(
                    VIEWER_REPOS,
                    json!({ "affiliation": affiliation, "cursor": null }),
                    "/viewer/repositories",
                    progress,
                    what,
                )
                .await
        }
    };
    let sources = &request.sources;
    let (owned, org, collaborator, contributed) = tokio::join!(
        viewer(sources.owned, "OWNER", "Repos you own"),
        viewer(
            sources.org_member,
            "ORGANIZATION_MEMBER",
            "Repos of your orgs"
        ),
        viewer(
            sources.collaborator,
            "COLLABORATOR",
            "Repos you collaborate on"
        ),
        async {
            if sources.contributed {
                client.contributions(progress).await
            } else {
                Ok(Part::default())
            }
        },
    );
    collector.absorb(owned?, Source::Owned);
    collector.absorb(org?, Source::OrgMember);
    collector.absorb(collaborator?, Source::Collaborator);
    collector.absorb(contributed?, Source::Contributed);

    for login in &request.manual_owners {
        let what = format!("Repos of {login}");
        progress(what.clone());
        let part = client
            .paginate(
                OWNER_REPOS,
                json!({ "login": login, "cursor": null }),
                "/repositoryOwner/repositories",
                progress,
                &what,
            )
            .await?;
        if !part.found {
            collector.warn(vec![format!("{login}: no such user or org on GitHub")]);
        }
        collector.absorb(part, Source::ManualOwner);
    }

    for (owner, name) in &request.manual_repos {
        progress(format!("{owner}/{name}"));
        let (repo, warnings) = client.one_repo(owner, name).await?;
        collector.warn(warnings);
        match repo {
            Some(repo) => collector.add(&repo, Source::ManualRepo),
            None => collector.warn(vec![format!(
                "{owner}/{name} (added by hand): not found on GitHub"
            )]),
        }
    }

    let mut repos: Vec<FetchedRepo> = collector.repos.into_values().collect();
    repos.sort_by_key(|a| a.meta.full_name());
    Ok(Fetched {
        repos,
        counts: collector.counts,
        warnings: collector.warnings,
    })
}

/// Checks a repo or owner typed into "Add by hand".
pub enum Lookup {
    Repo(RepoMeta),
    Owner(String),
    NotFound,
}

/// `owner/name`, a GitHub URL, or a bare login.
pub async fn look_up(http: &reqwest::Client, token: &str, text: &str) -> Result<Lookup> {
    let client = Client { http, token };
    let text = text.trim();
    if let Some((owner, name)) = parse_repo_reference(text) {
        let (repo, _) = client.one_repo(&owner, &name).await?;
        return Ok(repo.map_or(Lookup::NotFound, |r| Lookup::Repo(r.meta())));
    }
    if text.is_empty() || text.contains(char::is_whitespace) {
        bail!("Type owner/name, a GitHub URL, or a user or org name.");
    }
    let (data, _) = client.query(OWNER_EXISTS, json!({ "login": text })).await?;
    Ok(
        match data
            .pointer("/repositoryOwner/login")
            .and_then(Value::as_str)
        {
            Some(login) => Lookup::Owner(login.to_owned()),
            None => Lookup::NotFound,
        },
    )
}

/// `owner/name` or any URL form `remote_url` understands, on GitHub.
pub fn parse_repo_reference(text: &str) -> Option<(String, String)> {
    if let Some(parsed) = crate::git::remote_url::parse(text, &[])
        && parsed.is_github()
    {
        return Some((parsed.owner, parsed.name));
    }
    let (owner, name) = text.trim_matches('/').split_once('/')?;
    let valid = |s: &str| !s.is_empty() && !s.contains(['/', ':', ' ', '@']);
    (valid(owner) && valid(name))
        .then(|| (owner.to_owned(), name.trim_end_matches(".git").to_owned()))
}

/// What the REST API says about a repo that no source lists.
pub enum RestCheck {
    Exists,
    Moved(RepoMeta),
    NotFound,
}

/// Uses the REST API because it follows renames and transfers (GraphQL doesn't say).
pub async fn check_repo(
    http: &reqwest::Client,
    token: &str,
    owner: &str,
    name: &str,
) -> Result<RestCheck> {
    #[derive(Deserialize)]
    struct RestRepo {
        node_id: String,
        name: String,
        full_name: String,
        owner: GhLogin,
        description: Option<String>,
        language: Option<String>,
        #[serde(default)]
        topics: Vec<String>,
        fork: bool,
        archived: bool,
        private: bool,
        pushed_at: Option<String>,
        ssh_url: Option<String>,
        #[serde(default)]
        stargazers_count: u32,
    }
    let response = http
        .get(format!("https://api.github.com/repos/{owner}/{name}"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .context("couldn't reach GitHub")?;
    match response.status() {
        reqwest::StatusCode::NOT_FOUND => return Ok(RestCheck::NotFound),
        status if !status.is_success() => bail!("GitHub returned {status} for {owner}/{name}"),
        _ => {}
    }
    let repo: RestRepo = response.json().await.context("unexpected REST response")?;
    if repo
        .full_name
        .eq_ignore_ascii_case(&format!("{owner}/{name}"))
    {
        return Ok(RestCheck::Exists);
    }
    Ok(RestCheck::Moved(RepoMeta {
        node_id: repo.node_id,
        owner: repo.owner.login,
        name: repo.name,
        description: repo.description,
        language: repo.language,
        topics: repo.topics,
        is_fork: repo.fork,
        is_archived: repo.archived,
        is_private: repo.private,
        pushed_at: repo.pushed_at,
        ssh_url: repo.ssh_url,
        stars: repo.stargazers_count,
    }))
}

/// The current UTC year and time as `YYYY-MM-DDTHH:MM:SSZ`.
fn utc_now() -> (i64, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rest = secs.rem_euclid(86_400);
    (
        year,
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            rest / 3600,
            rest % 3600 / 60,
            rest % 60
        ),
    )
}

/// Days since 1970-01-01 to (year, month, day). From Howard Hinnant's date algorithms.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(20_413), (2025, 11, 21));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        let (year, now) = utc_now();
        assert!(year >= 2025);
        assert_eq!(now.len(), "2026-01-01T00:00:00Z".len());
    }

    #[test]
    fn repo_references() {
        let pair = |o: &str, n: &str| Some((o.to_owned(), n.to_owned()));
        assert_eq!(
            parse_repo_reference("facebook/react"),
            pair("facebook", "react")
        );
        assert_eq!(
            parse_repo_reference("facebook/react.git"),
            pair("facebook", "react")
        );
        assert_eq!(
            parse_repo_reference("https://github.com/facebook/react"),
            pair("facebook", "react")
        );
        assert_eq!(
            parse_repo_reference("git@github.com:facebook/react.git"),
            pair("facebook", "react")
        );
        assert_eq!(parse_repo_reference("rust-lang"), None);
        assert_eq!(parse_repo_reference("a/b/c"), None);
        assert_eq!(parse_repo_reference("https://gitlab.com/a/b"), None);
    }

    #[test]
    fn parses_repos_with_parents_and_merges_sources() {
        let page: Page = serde_json::from_value(json!({
            "pageInfo": { "hasNextPage": false, "endCursor": null },
            "nodes": [
                {
                    "id": "R_1", "name": "react", "owner": { "login": "me" },
                    "description": "", "isFork": true, "isArchived": false, "isPrivate": false,
                    "pushedAt": "2026-10-01T12:00:00Z", "sshUrl": "git@github.com:me/react.git",
                    "stargazerCount": 42,
                    "primaryLanguage": { "name": "JavaScript" },
                    "repositoryTopics": { "nodes": [ { "topic": { "name": "ui" } } ] },
                    "parent": {
                        "id": "R_0", "name": "react", "owner": { "login": "facebook" },
                        "description": "The library", "isFork": false, "isArchived": false,
                        "isPrivate": false, "pushedAt": null, "sshUrl": "git@github.com:facebook/react.git",
                        "primaryLanguage": null, "repositoryTopics": { "nodes": [] }
                    }
                },
                null
            ]
        }))
        .unwrap();
        let mut collector = Collector::default();
        for repo in page.nodes.iter().flatten() {
            collector.add(repo, Source::Owned);
            collector.add(repo, Source::Contributed);
            collector.add(repo, Source::Owned);
        }
        let fetched = &collector.repos["R_1"];
        assert_eq!(
            fetched.meta.description, None,
            "empty descriptions become None"
        );
        assert_eq!(fetched.meta.language.as_deref(), Some("JavaScript"));
        assert_eq!(fetched.meta.topics, ["ui"]);
        assert_eq!(fetched.meta.stars, 42);
        assert_eq!(
            fetched.parent.as_ref().unwrap().full_name(),
            "facebook/react"
        );
        assert_eq!(
            fetched.sources,
            [Source::Owned, Source::Contributed].into_iter().collect()
        );
        assert_eq!(collector.counts[&Source::Owned], 1);
    }

    /// Talks to the real GitHub API with your token. Run it on purpose:
    /// `cargo test live_github_sync -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_github_sync() {
        let token = crate::token::find().found.expect("no token").token;
        let http = crate::github::http_client().unwrap();
        let request = SyncRequest {
            sources: Sources::default(),
            manual_repos: vec![("rust-lang".into(), "rustlings".into())],
            manual_owners: vec![],
        };
        let fetched = fetch_all(&http, &token, &request, &|msg| println!("progress: {msg}"))
            .await
            .unwrap();
        println!("repos: {}", fetched.repos.len());
        for (source, count) in &fetched.counts {
            println!("  {source:?}: {count}");
        }
        let forks = fetched.repos.iter().filter(|r| r.parent.is_some()).count();
        println!("forks with a visible parent: {forks}");
        for warning in &fetched.warnings {
            println!("warning: {warning}");
        }
        assert!(fetched.counts.get(&Source::ManualRepo) == Some(&1));
        match check_repo(&http, &token, "rust-lang", "rustlings")
            .await
            .unwrap()
        {
            RestCheck::Exists => println!("rest check: exists"),
            _ => panic!("rust-lang/rustlings should exist"),
        }
        assert!(matches!(
            check_repo(&http, &token, "rust-lang", "no-such-repo-c7f3")
                .await
                .unwrap(),
            RestCheck::NotFound
        ));
    }
}
