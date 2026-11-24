//! Tags: manual ones stored per clone or per repo, and automatic ones computed from rules
//! every time. A clone's manual tags are its own plus its repo's, so a repo tagged before it's
//! cloned keeps those tags once it is.

pub mod query;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use regex::{Regex, RegexBuilder};

use crate::db::{RepoFacts, RepoRow};
use crate::git::scan::CloneInfo;
use crate::layout;

/// What a rule looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleField {
    /// `owner/name` as one string, so one pattern can cover either part.
    FullName,
    Owner,
    Name,
    Language,
    /// Matches if any GitHub topic matches.
    Topic,
    Fork,
    Archived,
    Private,
}

impl RuleField {
    pub const ALL: [RuleField; 8] = [
        RuleField::FullName,
        RuleField::Owner,
        RuleField::Name,
        RuleField::Language,
        RuleField::Topic,
        RuleField::Fork,
        RuleField::Archived,
        RuleField::Private,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RuleField::FullName => "full_name",
            RuleField::Owner => "owner",
            RuleField::Name => "name",
            RuleField::Language => "language",
            RuleField::Topic => "topic",
            RuleField::Fork => "fork",
            RuleField::Archived => "archived",
            RuleField::Private => "private",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        RuleField::ALL.into_iter().find(|f| f.as_str() == text)
    }

    pub fn label(self) -> &'static str {
        match self {
            RuleField::FullName => "owner/name",
            RuleField::Owner => "owner",
            RuleField::Name => "name",
            RuleField::Language => "language",
            RuleField::Topic => "a topic",
            RuleField::Fork => "is a fork",
            RuleField::Archived => "is archived",
            RuleField::Private => "is private",
        }
    }

    /// Yes/no fields take `yes` or `no` instead of a regex.
    pub fn is_flag(self) -> bool {
        matches!(
            self,
            RuleField::Fork | RuleField::Archived | RuleField::Private
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// 0 for a rule that hasn't been saved yet.
    pub id: i64,
    pub field: RuleField,
    /// A regex, or `yes`/`no` for flag fields.
    pub pattern: String,
    /// The tag to add; may use capture groups like `$1` or `${name}`.
    pub tag_template: String,
    pub case_insensitive: bool,
    pub enabled: bool,
    /// Clones this rule doesn't apply to.
    pub exceptions: BTreeSet<i64>,
}

impl Rule {
    pub fn new(field: RuleField) -> Self {
        Self {
            id: 0,
            field,
            pattern: if field.is_flag() {
                "yes".to_owned()
            } else {
                String::new()
            },
            tag_template: String::new(),
            case_insensitive: true,
            enabled: true,
            exceptions: BTreeSet::new(),
        }
    }

    /// "language matches ^rust$: rust", "is archived: archived"
    pub fn describe(&self) -> String {
        let condition = match (self.field, self.pattern == "yes") {
            (RuleField::Fork, true) => "is a fork".to_owned(),
            (RuleField::Fork, false) => "isn't a fork".to_owned(),
            (RuleField::Archived, true) => "is archived".to_owned(),
            (RuleField::Archived, false) => "isn't archived".to_owned(),
            (RuleField::Private, true) => "is private".to_owned(),
            (RuleField::Private, false) => "isn't private".to_owned(),
            (field, _) => format!("{} matches {}", field.label(), self.pattern),
        };
        format!("{condition}: {}", self.tag_template)
    }
}

/// Cleans up a tag name: trimmed, lowercase, spaces become `-`. Names that would confuse
/// queries (empty, parentheses, quotes, `and`/`or`/`not`/`untagged`, `*`) are refused.
pub fn normalize_tag(raw: &str) -> Result<String, String> {
    let tag: String = raw
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    if tag.is_empty() {
        return Err("A tag needs a name.".to_owned());
    }
    if let Some(bad) = tag
        .chars()
        .find(|c| matches!(c, '(' | ')' | '"' | '\'' | '*' | ','))
    {
        return Err(format!("Tags can't contain '{bad}'."));
    }
    if matches!(tag.as_str(), "and" | "or" | "not" | "untagged") {
        return Err(format!("\"{tag}\" is reserved for queries."));
    }
    Ok(tag)
}

/// What rules can see about a clone, or about a repo that isn't cloned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CloneFacts {
    /// The clone's ID, or the repo's in an index of repos that aren't cloned.
    pub id: i64,
    /// `owner/name`, or the folder name for clones without a hosted remote.
    pub full_name: String,
    pub owner: Option<String>,
    pub name: String,
    pub language: Option<String>,
    pub topics: Vec<String>,
    /// `None` when unknown (not synced, or not on GitHub); flag rules never match then.
    pub is_fork: Option<bool>,
    pub is_archived: Option<bool>,
    pub is_private: Option<bool>,
}

impl CloneFacts {
    /// Facts for a scanned clone, with what the last sync said about its repo.
    pub fn from_clone(clone: &CloneInfo, repo: Option<&RepoFacts>) -> Self {
        let url = clone.repo();
        let (folder, _) = layout::split_suffix(clone.folder_name());
        Self {
            id: clone.id,
            full_name: clone.display_name(),
            owner: url.map(|u| u.owner.clone()),
            name: url.map_or_else(|| folder.to_owned(), |u| u.name.clone()),
            language: repo.and_then(|r| r.language.clone()),
            topics: repo.map(|r| r.topics.clone()).unwrap_or_default(),
            is_fork: repo.and_then(|r| r.is_fork),
            is_archived: repo.and_then(|r| r.is_archived),
            is_private: repo.and_then(|r| r.is_private),
        }
    }

    /// Facts for a repo that isn't cloned, as the last sync described it.
    pub fn from_repo(repo: &RepoRow) -> Self {
        Self {
            id: repo.id,
            full_name: repo.full_name(),
            owner: Some(repo.owner.clone()),
            name: repo.name.clone(),
            language: repo.language.clone(),
            topics: repo.topics.clone(),
            is_fork: Some(repo.is_fork),
            is_archived: Some(repo.is_archived),
            is_private: Some(repo.is_private),
        }
    }
}

/// Each clone's manual tags: its own plus its repo's, so tags given to a repo before it was
/// cloned stay with it. `clones` is (clone ID, repo ID).
pub fn clone_manual_tags(
    clone_tags: &HashMap<i64, BTreeSet<String>>,
    repo_tags: &HashMap<i64, BTreeSet<String>>,
    clones: impl IntoIterator<Item = (i64, Option<i64>)>,
) -> HashMap<i64, BTreeSet<String>> {
    let mut manual = clone_tags.clone();
    for (clone, repo) in clones {
        if let Some(tags) = repo.and_then(|id| repo_tags.get(&id)) {
            manual
                .entry(clone)
                .or_default()
                .extend(tags.iter().cloned());
        }
    }
    manual
}

/// A rule ready to apply.
#[derive(Debug)]
pub struct CompiledRule {
    pub id: i64,
    field: RuleField,
    regex: Option<Regex>,
    flag: bool,
    template: String,
    enabled: bool,
    exceptions: HashSet<i64>,
}

pub fn compile(rule: &Rule) -> Result<CompiledRule, String> {
    let template = rule.tag_template.trim();
    if template.is_empty() {
        return Err("Say which tag to add.".to_owned());
    }
    let (regex, flag) = if rule.field.is_flag() {
        match rule.pattern.as_str() {
            "yes" => (None, true),
            "no" => (None, false),
            _ => return Err("Choose yes or no.".to_owned()),
        }
    } else {
        if rule.pattern.trim().is_empty() {
            return Err("Write a pattern to match.".to_owned());
        }
        let regex = RegexBuilder::new(&rule.pattern)
            .case_insensitive(rule.case_insensitive)
            .size_limit(1 << 20)
            .build()
            .map_err(|e| format!("That pattern isn't a valid regex: {e}"))?;
        (Some(regex), false)
    };
    if regex.is_none() {
        normalize_tag(template)?;
    }
    Ok(CompiledRule {
        id: rule.id,
        field: rule.field,
        regex,
        flag,
        template: template.to_owned(),
        enabled: rule.enabled,
        exceptions: rule.exceptions.iter().copied().collect(),
    })
}

impl CompiledRule {
    /// The tags this rule gives a clone, ignoring exceptions and whether it's enabled.
    pub fn tags_for(&self, facts: &CloneFacts) -> Vec<String> {
        let flag = |value: Option<bool>| value == Some(self.flag);
        let values: Vec<&str> = match self.field {
            RuleField::FullName => vec![facts.full_name.as_str()],
            RuleField::Owner => facts.owner.as_deref().into_iter().collect(),
            RuleField::Name => vec![facts.name.as_str()],
            RuleField::Language => facts.language.as_deref().into_iter().collect(),
            RuleField::Topic => facts.topics.iter().map(String::as_str).collect(),
            RuleField::Fork | RuleField::Archived | RuleField::Private => {
                let value = match self.field {
                    RuleField::Fork => facts.is_fork,
                    RuleField::Archived => facts.is_archived,
                    _ => facts.is_private,
                };
                return if flag(value) {
                    normalize_tag(&self.template).into_iter().collect()
                } else {
                    Vec::new()
                };
            }
        };
        let regex = self.regex.as_ref().expect("text fields have a regex");
        let mut tags = Vec::new();
        for value in values {
            if let Some(captures) = regex.captures(value) {
                let mut tag = String::new();
                captures.expand(&self.template, &mut tag);
                if let Ok(tag) = normalize_tag(&tag)
                    && !tags.contains(&tag)
                {
                    tags.push(tag);
                }
            }
        }
        tags
    }

    pub fn applies_to(&self, clone_id: i64) -> bool {
        self.enabled && !self.exceptions.contains(&clone_id)
    }
}

/// One clone's (or repo's) tags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tagging {
    pub manual: BTreeSet<String>,
    /// Automatic tags, each with the IDs of the rules that gave it.
    pub auto: BTreeMap<String, Vec<i64>>,
}

impl Tagging {
    pub fn all(&self) -> BTreeSet<String> {
        self.manual
            .iter()
            .chain(self.auto.keys())
            .cloned()
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.manual.is_empty() && self.auto.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TagCount {
    /// Clones (or repos) with the tag at all.
    pub total: usize,
    pub manual: usize,
    pub auto: usize,
}

/// Adds one clone's (or repo's) tags to per-tag counts.
fn count_tagging(counts: &mut BTreeMap<String, TagCount>, tagging: &Tagging) {
    for tag in tagging.all() {
        let count = counts.entry(tag.clone()).or_default();
        count.total += 1;
        count.manual += usize::from(tagging.manual.contains(&tag));
        count.auto += usize::from(tagging.auto.contains_key(&tag));
    }
}

/// Every clone's tags, plus per-tag counts. Rebuilt whenever clones, tags or rules change.
/// The same type indexes repos that aren't cloned, keyed by repo ID.
#[derive(Default)]
pub struct TagIndex {
    /// By clone ID (or repo ID, in an index of repos).
    pub by_clone: HashMap<i64, Tagging>,
    pub counts: BTreeMap<String, TagCount>,
    /// Rules that don't compile (only possible if the database was edited by hand).
    pub rule_errors: HashMap<i64, String>,
}

impl TagIndex {
    /// Tags of scanned clones.
    pub fn build(
        clones: &[CloneFacts],
        manual: &HashMap<i64, BTreeSet<String>>,
        rules: &[Rule],
    ) -> Self {
        Self::build_with(clones, manual, rules, true)
    }

    /// Tags of repos that aren't cloned, by repo ID. Rules apply to them as they do to
    /// clones, but exceptions don't: those are made for one clone.
    pub fn build_for_repos(
        repos: &[CloneFacts],
        manual: &HashMap<i64, BTreeSet<String>>,
        rules: &[Rule],
    ) -> Self {
        Self::build_with(repos, manual, rules, false)
    }

    fn build_with(
        subjects: &[CloneFacts],
        manual: &HashMap<i64, BTreeSet<String>>,
        rules: &[Rule],
        exceptions: bool,
    ) -> Self {
        let mut index = TagIndex::default();
        let mut compiled = Vec::new();
        for rule in rules {
            match compile(rule) {
                Ok(rule) => compiled.push(rule),
                Err(e) => {
                    index.rule_errors.insert(rule.id, e);
                }
            }
        }
        for facts in subjects {
            let mut tagging = Tagging {
                manual: manual.get(&facts.id).cloned().unwrap_or_default(),
                auto: BTreeMap::new(),
            };
            let applies = |rule: &&CompiledRule| {
                if exceptions {
                    rule.applies_to(facts.id)
                } else {
                    rule.enabled
                }
            };
            for rule in compiled.iter().filter(applies) {
                for tag in rule.tags_for(facts) {
                    tagging.auto.entry(tag).or_default().push(rule.id);
                }
            }
            count_tagging(&mut index.counts, &tagging);
            index.by_clone.insert(facts.id, tagging);
        }
        index
    }

    /// Per-tag counts over some of the indexed IDs, added to `counts`.
    pub fn count_into(
        &self,
        ids: impl IntoIterator<Item = i64>,
        counts: &mut BTreeMap<String, TagCount>,
    ) {
        for id in ids {
            if let Some(tagging) = self.by_clone.get(&id) {
                count_tagging(counts, tagging);
            }
        }
    }

    pub fn is_untagged(&self, id: i64) -> bool {
        self.by_clone.get(&id).is_none_or(Tagging::is_empty)
    }

    pub fn tags_of(&self, clone_id: i64) -> BTreeSet<String> {
        self.by_clone
            .get(&clone_id)
            .map(Tagging::all)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(id: i64, full: &str, language: Option<&str>) -> CloneFacts {
        let (owner, name) = full.split_once('/').unwrap();
        CloneFacts {
            id,
            full_name: full.into(),
            owner: Some(owner.into()),
            name: name.into(),
            language: language.map(Into::into),
            topics: vec!["cli".into(), "Developer-Tools".into()],
            is_fork: Some(false),
            is_archived: Some(id == 3),
            is_private: None,
        }
    }

    fn rule(id: i64, field: RuleField, pattern: &str, tag: &str) -> Rule {
        Rule {
            id,
            pattern: pattern.into(),
            tag_template: tag.into(),
            ..Rule::new(field)
        }
    }

    #[test]
    fn descriptions() {
        assert_eq!(
            rule(1, RuleField::Language, "^rust$", "rust").describe(),
            "language matches ^rust$: rust"
        );
        assert_eq!(
            rule(1, RuleField::Archived, "no", "active").describe(),
            "isn't archived: active"
        );
    }

    #[test]
    fn tag_names_are_normalized() {
        assert_eq!(normalize_tag("  Work Stuff "), Ok("work-stuff".into()));
        assert_eq!(normalize_tag("lang:Rust"), Ok("lang:rust".into()));
        assert!(normalize_tag("").is_err());
        assert!(normalize_tag("a(b").is_err());
        assert!(normalize_tag("AND").is_err());
        assert!(normalize_tag("*").is_err());
        assert!(normalize_tag("Untagged").is_err());
    }

    #[test]
    fn text_rules_with_captures() {
        let org = compile(&rule(1, RuleField::FullName, "^([^/]+)/", "org:$1")).unwrap();
        assert_eq!(org.tags_for(&facts(1, "MyOrg/api", None)), ["org:myorg"]);

        let lang = compile(&rule(2, RuleField::Language, "^rust$", "rust")).unwrap();
        assert_eq!(lang.tags_for(&facts(1, "a/b", Some("Rust"))), ["rust"]);
        assert!(lang.tags_for(&facts(1, "a/b", Some("Rustic"))).is_empty());
        assert!(lang.tags_for(&facts(1, "a/b", None)).is_empty());

        let mut strict = rule(3, RuleField::Language, "^rust$", "rust");
        strict.case_insensitive = false;
        assert!(
            compile(&strict)
                .unwrap()
                .tags_for(&facts(1, "a/b", Some("Rust")))
                .is_empty()
        );

        let topics = compile(&rule(4, RuleField::Topic, "(.+)", "topic:$1")).unwrap();
        assert_eq!(
            topics.tags_for(&facts(1, "a/b", None)),
            ["topic:cli", "topic:developer-tools"]
        );
    }

    #[test]
    fn flag_rules_need_known_values() {
        let archived = compile(&rule(1, RuleField::Archived, "yes", "archived")).unwrap();
        assert_eq!(archived.tags_for(&facts(3, "a/b", None)), ["archived"]);
        assert!(archived.tags_for(&facts(1, "a/b", None)).is_empty());
        let public = compile(&rule(2, RuleField::Private, "no", "public")).unwrap();
        assert!(
            public.tags_for(&facts(1, "a/b", None)).is_empty(),
            "privacy unknown"
        );
    }

    #[test]
    fn bad_rules_are_explained() {
        assert!(
            compile(&rule(1, RuleField::Name, "(", "x"))
                .unwrap_err()
                .contains("regex")
        );
        assert!(compile(&rule(1, RuleField::Name, "", "x")).is_err());
        assert!(compile(&rule(1, RuleField::Name, "x", " ")).is_err());
        assert!(compile(&rule(1, RuleField::Fork, "maybe", "x")).is_err());
        assert!(compile(&rule(1, RuleField::Fork, "yes", "and")).is_err());
    }

    #[test]
    fn index_tracks_manual_auto_and_exceptions() {
        let clones = [
            facts(1, "me/api", Some("Rust")),
            facts(2, "me/web", Some("TypeScript")),
        ];
        let manual = HashMap::from([(1, BTreeSet::from(["rust".to_owned(), "work".to_owned()]))]);
        let mut lang = rule(10, RuleField::Language, "(.+)", "$1");
        let mine = rule(11, RuleField::Owner, "^me$", "mine");
        let index = TagIndex::build(&clones, &manual, &[lang.clone(), mine.clone()]);

        let api = &index.by_clone[&1];
        assert_eq!(api.manual, BTreeSet::from(["rust".into(), "work".into()]));
        assert_eq!(api.auto["rust"], [10]);
        assert_eq!(api.auto["mine"], [11]);
        assert_eq!(
            index.counts["rust"],
            TagCount {
                total: 1,
                manual: 1,
                auto: 1
            }
        );
        assert_eq!(index.counts["mine"].total, 2);
        assert_eq!(index.counts["typescript"].auto, 1);

        // An exception stops the rule for that clone only; disabling stops it everywhere.
        lang.exceptions.insert(1);
        let mut disabled = mine;
        disabled.enabled = false;
        let index = TagIndex::build(&clones, &manual, &[lang, disabled]);
        assert!(!index.by_clone[&1].auto.contains_key("rust"));
        assert!(
            index.by_clone[&1].manual.contains("rust"),
            "manual tag stays"
        );
        assert!(index.by_clone[&2].auto.contains_key("typescript"));
        assert!(!index.counts.contains_key("mine"));
    }

    #[test]
    fn clones_have_their_repos_tags_too() {
        let set = |tags: &[&str]| {
            tags.iter()
                .map(|t| (*t).to_owned())
                .collect::<BTreeSet<_>>()
        };
        let clone_tags = HashMap::from([(1, set(&["work"]))]);
        let repo_tags = HashMap::from([(10, set(&["later", "work"])), (11, set(&["x"]))]);
        let manual = clone_manual_tags(
            &clone_tags,
            &repo_tags,
            [(1, Some(10)), (2, Some(10)), (3, None), (4, Some(99))],
        );
        assert_eq!(manual[&1], set(&["later", "work"]));
        assert_eq!(manual[&2], set(&["later", "work"]));
        assert!(!manual.contains_key(&3) && !manual.contains_key(&4));
    }

    #[test]
    fn repo_index_applies_rules_but_not_clone_exceptions() {
        let repo = RepoRow {
            id: 1,
            owner: "me".into(),
            name: "api".into(),
            description: None,
            language: Some("Rust".into()),
            topics: Vec::new(),
            is_fork: false,
            is_archived: true,
            is_private: false,
            pushed_at: None,
            stars: None,
            sources: Vec::new(),
            fork_of: None,
            ignored: false,
        };
        let mut lang = rule(10, RuleField::Language, "(.+)", "lang:$1");
        lang.exceptions.insert(1); // clone 1, which isn't repo 1
        let archived = rule(11, RuleField::Archived, "yes", "archived");
        let manual = HashMap::from([(1, BTreeSet::from(["later".to_owned()]))]);
        let index = TagIndex::build_for_repos(
            &[
                CloneFacts::from_repo(&repo),
                CloneFacts {
                    id: 2,
                    ..Default::default()
                },
            ],
            &manual,
            &[lang, archived],
        );
        assert_eq!(
            index.tags_of(1),
            BTreeSet::from(["archived".into(), "lang:rust".into(), "later".into()])
        );
        assert!(!index.is_untagged(1));
        assert!(index.is_untagged(2));
        assert!(index.is_untagged(99), "unknown IDs have no tags");

        let mut counts = BTreeMap::new();
        index.count_into([1, 2], &mut counts);
        assert_eq!(counts["later"].manual, 1);
        assert_eq!(counts.len(), 3);
    }
}
