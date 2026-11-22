//! Manual tags, tag rules and rule exceptions.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result};
use rusqlite::params;

use super::Db;
use crate::tags::{Rule, RuleField};

/// What rules may look at for a repo. Flags are `None` until a sync has described the repo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoFacts {
    pub language: Option<String>,
    pub topics: Vec<String>,
    pub is_fork: Option<bool>,
    pub is_archived: Option<bool>,
    pub is_private: Option<bool>,
}

/// Everything needed to work out every clone's tags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagStore {
    /// Manual tags by clone ID.
    pub manual: HashMap<i64, BTreeSet<String>>,
    /// In display order.
    pub rules: Vec<Rule>,
    /// By repo ID.
    pub repo_facts: HashMap<i64, RepoFacts>,
}

impl Db {
    pub fn tag_store(&self) -> Result<TagStore> {
        let mut store = TagStore::default();

        let mut stmt = self.conn.prepare(
            "SELECT ct.clone_id, t.name FROM clone_tags ct JOIN tags t ON t.id = ct.tag_id",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (clone, tag) = row?;
            store.manual.entry(clone).or_default().insert(tag);
        }

        let mut exceptions: HashMap<i64, BTreeSet<i64>> = HashMap::new();
        let mut stmt = self
            .conn
            .prepare("SELECT rule_id, clone_id FROM rule_exceptions")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (rule, clone) = row?;
            exceptions.entry(rule).or_default().insert(clone);
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, field, pattern, tag_template, case_insensitive, enabled
             FROM rules ORDER BY position, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })?;
        for row in rows {
            let (id, field, pattern, tag_template, case_insensitive, enabled) = row?;
            let Some(field) = RuleField::parse(&field) else {
                tracing::warn!("rule {id} has an unknown field {field:?}; skipping it");
                continue;
            };
            store.rules.push(Rule {
                id,
                field,
                pattern,
                tag_template,
                case_insensitive,
                enabled,
                exceptions: exceptions.remove(&id).unwrap_or_default(),
            });
        }

        let mut stmt = self.conn.prepare(
            "SELECT id, language, topics, is_fork, is_archived, is_private,
                    github_node_id IS NOT NULL
             FROM repos",
        )?;
        let rows = stmt.query_map([], |r| {
            let synced: bool = r.get(6)?;
            let flag = |value: bool| synced.then_some(value);
            Ok((
                r.get::<_, i64>(0)?,
                RepoFacts {
                    language: r.get(1)?,
                    topics: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                    is_fork: flag(r.get(3)?),
                    is_archived: flag(r.get(4)?),
                    is_private: flag(r.get(5)?),
                },
            ))
        })?;
        for row in rows {
            let (id, facts) = row?;
            store.repo_facts.insert(id, facts);
        }
        Ok(store)
    }

    /// Adds a manual tag (already normalized) to clones.
    pub fn add_tag(&mut self, clone_ids: &[i64], tag: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO tags (name) VALUES (?1) ON CONFLICT (name) DO NOTHING",
            [tag],
        )?;
        let tag_id: i64 =
            tx.query_row("SELECT id FROM tags WHERE name = ?1", [tag], |r| r.get(0))?;
        for clone in clone_ids {
            tx.execute(
                "INSERT OR IGNORE INTO clone_tags (clone_id, tag_id) VALUES (?1, ?2)",
                params![clone, tag_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes a manual tag from clones; tags nothing uses any more are deleted.
    pub fn remove_tag(&mut self, clone_ids: &[i64], tag: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        for clone in clone_ids {
            tx.execute(
                "DELETE FROM clone_tags
                 WHERE clone_id = ?1 AND tag_id = (SELECT id FROM tags WHERE name = ?2)",
                params![clone, tag],
            )?;
        }
        tx.execute(
            "DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM clone_tags)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Inserts a new rule (id 0) at the end, or updates an existing one. Returns its ID.
    pub fn save_rule(&mut self, rule: &Rule) -> Result<i64> {
        if rule.id == 0 {
            self.conn.execute(
                "INSERT INTO rules (position, field, pattern, tag_template, case_insensitive, enabled)
                 VALUES ((SELECT coalesce(max(position), 0) + 1 FROM rules), ?1, ?2, ?3, ?4, ?5)",
                params![
                    rule.field.as_str(),
                    rule.pattern,
                    rule.tag_template.trim(),
                    rule.case_insensitive,
                    rule.enabled
                ],
            )?;
            return Ok(self.conn.last_insert_rowid());
        }
        let updated = self.conn.execute(
            "UPDATE rules SET field = ?2, pattern = ?3, tag_template = ?4, case_insensitive = ?5,
                              enabled = ?6
             WHERE id = ?1",
            params![
                rule.id,
                rule.field.as_str(),
                rule.pattern,
                rule.tag_template.trim(),
                rule.case_insensitive,
                rule.enabled
            ],
        )?;
        anyhow::ensure!(updated == 1, "that rule no longer exists");
        Ok(rule.id)
    }

    pub fn delete_rule(&mut self, rule_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM rules WHERE id = ?1", [rule_id])
            .context("couldn't delete the rule")?;
        Ok(())
    }

    pub fn set_rule_enabled(&mut self, rule_id: i64, enabled: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE rules SET enabled = ?2 WHERE id = ?1",
            params![rule_id, enabled],
        )?;
        Ok(())
    }

    /// Adds (or removes) an exception: the rule doesn't apply to this clone.
    pub fn set_rule_exception(
        &mut self,
        rule_id: i64,
        clone_id: i64,
        excepted: bool,
    ) -> Result<()> {
        if excepted {
            self.conn.execute(
                "INSERT OR IGNORE INTO rule_exceptions (rule_id, clone_id) VALUES (?1, ?2)",
                params![rule_id, clone_id],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM rule_exceptions WHERE rule_id = ?1 AND clone_id = ?2",
                params![rule_id, clone_id],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{FetchedRepo, RepoMeta, ScannedClone, ScannedRemote, Source};
    use crate::git::remote_url::{GITHUB, RemoteUrl};

    fn db_with_clones() -> (Db, Vec<i64>) {
        let mut db = Db::open_in_memory().unwrap();
        db.apply_sync(&[FetchedRepo {
            meta: RepoMeta {
                node_id: "A".into(),
                owner: "me".into(),
                name: "api".into(),
                language: Some("Rust".into()),
                topics: vec!["cli".into()],
                is_archived: true,
                ..Default::default()
            },
            parent: None,
            sources: [Source::Owned].into(),
        }])
        .unwrap();
        let api = RemoteUrl {
            host: GITHUB.into(),
            owner: "me".into(),
            name: "api".into(),
        };
        let local = RemoteUrl {
            host: GITHUB.into(),
            owner: "me".into(),
            name: "unsynced".into(),
        };
        let remote = |parsed| ScannedRemote {
            name: "me",
            url: "x",
            parsed: Some(parsed),
        };
        let ids = db
            .apply_scan(&[
                ScannedClone {
                    path: "a/api",
                    remotes: vec![remote(&api)],
                    main_remote: Some(0),
                },
                ScannedClone {
                    path: "u/unsynced",
                    remotes: vec![remote(&local)],
                    main_remote: Some(0),
                },
            ])
            .unwrap()
            .ids;
        (db, ids)
    }

    #[test]
    fn manual_tags_add_and_remove() {
        let (mut db, ids) = db_with_clones();
        db.add_tag(&ids, "work").unwrap();
        db.add_tag(&ids[..1], "work").unwrap();
        db.add_tag(&ids[..1], "rust").unwrap();
        let store = db.tag_store().unwrap();
        assert_eq!(
            store.manual[&ids[0]],
            BTreeSet::from(["rust".into(), "work".into()])
        );
        assert_eq!(store.manual[&ids[1]], BTreeSet::from(["work".into()]));

        db.remove_tag(&ids, "work").unwrap();
        db.remove_tag(&ids[..1], "rust").unwrap();
        assert!(db.tag_store().unwrap().manual.is_empty());
        let tags: i64 = db
            .conn
            .query_row("SELECT count(*) FROM tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tags, 0, "unused tags are cleaned up");
    }

    #[test]
    fn rules_and_exceptions_round_trip() {
        let (mut db, ids) = db_with_clones();
        let mut rule = Rule {
            pattern: "^rust$".into(),
            tag_template: " rust ".into(),
            ..Rule::new(RuleField::Language)
        };
        let id = db.save_rule(&rule).unwrap();
        let second = db.save_rule(&Rule {
            tag_template: "archived".into(),
            ..Rule::new(RuleField::Archived)
        });
        db.set_rule_exception(id, ids[0], true).unwrap();
        db.set_rule_enabled(second.unwrap(), false).unwrap();

        let store = db.tag_store().unwrap();
        assert_eq!(store.rules.len(), 2);
        assert_eq!(store.rules[0].id, id);
        assert_eq!(store.rules[0].tag_template, "rust", "trimmed");
        assert_eq!(store.rules[0].exceptions, BTreeSet::from([ids[0]]));
        assert!(!store.rules[1].enabled);

        rule.id = id;
        rule.pattern = "^go$".into();
        db.save_rule(&rule).unwrap();
        db.set_rule_exception(id, ids[0], false).unwrap();
        let store = db.tag_store().unwrap();
        assert_eq!(store.rules[0].pattern, "^go$");
        assert!(store.rules[0].exceptions.is_empty());

        db.delete_rule(id).unwrap();
        assert_eq!(db.tag_store().unwrap().rules.len(), 1);
        rule.id = 999;
        assert!(db.save_rule(&rule).is_err());
    }

    #[test]
    fn facts_know_which_flags_are_known() {
        let (db, _) = db_with_clones();
        let store = db.tag_store().unwrap();
        let synced = store
            .repo_facts
            .values()
            .find(|f| f.language.is_some())
            .unwrap();
        assert_eq!(synced.topics, ["cli"]);
        assert_eq!(synced.is_archived, Some(true));
        assert_eq!(synced.is_fork, Some(false));
        let unsynced = store
            .repo_facts
            .values()
            .find(|f| f.language.is_none())
            .unwrap();
        assert_eq!(unsynced.is_archived, None, "never synced, so unknown");
    }
}
