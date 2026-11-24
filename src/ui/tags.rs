//! The Tags tab: rules that tag clones and repos automatically. Tags are added by hand, and
//! groups picked by tag, in the Repositories tab.

use eframe::egui;

use crate::db::RepoRow;
use crate::tags::{self, CloneFacts, Rule, RuleField, TagIndex, Tagging};
use crate::ui::plural;

#[derive(Debug, PartialEq, Eq)]
pub enum TagsAction {
    /// Add a manual tag to clones and to repos that aren't cloned.
    AddTag {
        clone_ids: Vec<i64>,
        repo_ids: Vec<i64>,
        tag: String,
    },
    /// Remove a manual tag from clones (and their repos) and from repos.
    RemoveTag {
        clone_ids: Vec<i64>,
        repo_ids: Vec<i64>,
        tag: String,
    },
    SaveRule(Rule),
    DeleteRule(i64),
    SetRuleEnabled {
        rule_id: i64,
        enabled: bool,
    },
    SetRuleException {
        rule_id: i64,
        clone_id: i64,
        excepted: bool,
    },
}

pub struct TagsContext<'a> {
    /// Repos in the Remote List: listed, not cloned, not ignored.
    pub repos: &'a [RepoRow],
    pub facts: &'a [CloneFacts],
    pub index: &'a TagIndex,
    /// Tags of repos that aren't cloned, by repo ID.
    pub repo_index: &'a TagIndex,
    pub rules: &'a [Rule],
}

pub struct TagsPage {
    editor: Option<Rule>,
    error: Option<String>,
}

impl TagsPage {
    pub fn new() -> Self {
        Self {
            editor: None,
            error: None,
        }
    }

    pub fn action_failed(&mut self, message: String) {
        self.error = Some(message);
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: TagsContext<'_>) -> Option<TagsAction> {
        let mut action = None;
        if let Some(error) = &self.error {
            let mut dismiss = false;
            ui.horizontal(|ui| {
                ui.colored_label(ui.visuals().error_fg_color, error);
                dismiss = ui.small_button("Dismiss").clicked();
            });
            if dismiss {
                self.error = None;
            }
        }
        self.rules(ui, &cx, &mut action);
        if action.is_some() {
            self.error = None;
        }
        action
    }

    fn rules(&mut self, ui: &mut egui::Ui, cx: &TagsContext<'_>, action: &mut Option<TagsAction>) {
        ui.horizontal(|ui| {
            ui.heading(format!("Rules ({})", cx.rules.len()));
            if ui.button("New rule").clicked() {
                self.editor = Some(Rule::new(RuleField::FullName));
            }
        });
        ui.weak(
            "Rules add tags automatically and apply live: changing one updates every clone. \
             Add tags by hand, and pick repos by tag, in the Repositories tab.",
        );
        ui.add_space(4.0);
        if self.editor.is_some() {
            self.rule_editor(ui, cx, action);
            ui.add_space(8.0);
        }

        if cx.rules.is_empty() {
            ui.weak(
                "No rules yet. For example: language matches ^rust$ adds rust, or \
                 owner/name matches ^([^/]+)/ adds org:$1.",
            );
            return;
        }
        for rule in cx.rules {
            ui.horizontal(|ui| {
                let mut enabled = rule.enabled;
                if ui.checkbox(&mut enabled, rule.describe()).changed() {
                    *action = Some(TagsAction::SetRuleEnabled {
                        rule_id: rule.id,
                        enabled,
                    });
                }
                let tagged = cx
                    .index
                    .by_clone
                    .values()
                    .filter(|t| t.auto.values().any(|rules| rules.contains(&rule.id)))
                    .count();
                let repos_tagged = cx
                    .repos
                    .iter()
                    .filter_map(|r| cx.repo_index.by_clone.get(&r.id))
                    .filter(|t| t.auto.values().any(|rules| rules.contains(&rule.id)))
                    .count();
                if repos_tagged > 0 {
                    ui.weak(format!(
                        "· tags {} and {} not cloned",
                        plural(tagged, "clone", "clones"),
                        plural(repos_tagged, "repo", "repos")
                    ));
                } else {
                    ui.weak(format!("· tags {}", plural(tagged, "clone", "clones")));
                }
                if !rule.exceptions.is_empty() {
                    ui.weak(format!(
                        "· {}",
                        plural(rule.exceptions.len(), "exception", "exceptions")
                    ));
                }
                if let Some(error) = cx.index.rule_errors.get(&rule.id) {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if ui.small_button("Edit").clicked() {
                    self.editor = Some(rule.clone());
                }
                if ui.small_button("Delete").clicked() {
                    *action = Some(TagsAction::DeleteRule(rule.id));
                    if self.editor.as_ref().is_some_and(|e| e.id == rule.id) {
                        self.editor = None;
                    }
                }
            });
        }
    }

    fn rule_editor(
        &mut self,
        ui: &mut egui::Ui,
        cx: &TagsContext<'_>,
        action: &mut Option<TagsAction>,
    ) {
        let Some(rule) = &mut self.editor else {
            return;
        };
        let mut close = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong(if rule.id == 0 {
                "New rule"
            } else {
                "Edit rule"
            });
            egui::Grid::new("rule_editor")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    let label = ui.label("When");
                    let before = rule.field;
                    egui::ComboBox::from_id_salt("rule_field")
                        .selected_text(rule.field.label())
                        .show_ui(ui, |ui| {
                            for field in RuleField::ALL {
                                ui.selectable_value(&mut rule.field, field, field.label());
                            }
                        })
                        .response
                        .labelled_by(label.id);
                    if rule.field.is_flag() != before.is_flag() {
                        rule.pattern = if rule.field.is_flag() {
                            "yes".into()
                        } else {
                            String::new()
                        };
                    }
                    ui.end_row();

                    if rule.field.is_flag() {
                        ui.label("Is");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut rule.pattern, "yes".to_owned(), "Yes");
                            ui.radio_value(&mut rule.pattern, "no".to_owned(), "No");
                        });
                    } else {
                        let label = ui.label("Matches");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut rule.pattern)
                                    .font(egui::TextStyle::Monospace)
                                    .hint_text("^rust$")
                                    .desired_width(260.0),
                            )
                            .labelled_by(label.id);
                            ui.checkbox(&mut rule.case_insensitive, "Ignore case");
                        });
                    }
                    ui.end_row();

                    let label = ui.label("Add tag");
                    ui.add(
                        egui::TextEdit::singleline(&mut rule.tag_template)
                            .hint_text(if rule.field.is_flag() {
                                "archived"
                            } else {
                                "rust, or org:$1 to use a capture group"
                            })
                            .desired_width(260.0),
                    )
                    .labelled_by(label.id);
                    ui.end_row();
                });

            let compiled = tags::compile(rule);
            match &compiled {
                Err(error) => {
                    ui.colored_label(ui.visuals().warn_fg_color, error);
                }
                Ok(compiled) => {
                    let hits: Vec<(&CloneFacts, Vec<String>)> = cx
                        .facts
                        .iter()
                        .filter_map(|f| {
                            let tags = compiled.tags_for(f);
                            (!tags.is_empty()).then_some((f, tags))
                        })
                        .collect();
                    ui.label(format!(
                        "Tags {} of {}",
                        plural(hits.len(), "clone", "clones"),
                        cx.facts.len()
                    ));
                    egui::ScrollArea::vertical()
                        .id_salt("rule_preview")
                        .max_height(220.0)
                        .show(ui, |ui| {
                            for (facts, tags) in hits {
                                ui.horizontal(|ui| {
                                    let excepted = rule.exceptions.contains(&facts.id);
                                    let text = format!("{}: {}", facts.full_name, tags.join(", "));
                                    if excepted {
                                        ui.label(egui::RichText::new(text).strikethrough().weak())
                                            .on_hover_text(
                                                "Excepted: this rule doesn't apply here",
                                            );
                                    } else {
                                        ui.label(text);
                                    }
                                    if rule.id == 0 {
                                        return;
                                    }
                                    let (button, excepted_now) = if excepted {
                                        ("Remove exception", false)
                                    } else {
                                        ("Add exception", true)
                                    };
                                    if ui.small_button(button).clicked() {
                                        if excepted_now {
                                            rule.exceptions.insert(facts.id);
                                        } else {
                                            rule.exceptions.remove(&facts.id);
                                        }
                                        *action = Some(TagsAction::SetRuleException {
                                            rule_id: rule.id,
                                            clone_id: facts.id,
                                            excepted: excepted_now,
                                        });
                                    }
                                });
                            }
                        });
                }
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(compiled.is_ok(), egui::Button::new("Save rule"))
                    .clicked()
                {
                    *action = Some(TagsAction::SaveRule(rule.clone()));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.editor = None;
        }
    }
}

/// Manual tags in plain text, automatic ones in italics.
pub fn tag_summary(ui: &mut egui::Ui, tagging: &Tagging) {
    for tag in &tagging.manual {
        ui.label(tag);
    }
    for tag in tagging.auto.keys().filter(|t| !tagging.manual.contains(*t)) {
        ui.label(egui::RichText::new(tag).italics().weak())
            .on_hover_text("Added by a rule");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::path::PathBuf;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    struct Fixture {
        repos: Vec<RepoRow>,
        facts: Vec<CloneFacts>,
        rules: Vec<Rule>,
        index: TagIndex,
        repo_index: TagIndex,
    }

    /// Repos in the Remote List: one tagged `work` and `someday` (and lang:rust by the rule),
    /// one with no tags at all.
    fn uncloned() -> (Vec<RepoRow>, HashMap<i64, BTreeSet<String>>) {
        let repo = |id: i64, owner: &str, name: &str, language: Option<&str>| RepoRow {
            id,
            owner: owner.into(),
            name: name.into(),
            description: None,
            language: language.map(Into::into),
            topics: Vec::new(),
            is_fork: false,
            is_archived: false,
            is_private: false,
            pushed_at: None,
            stars: None,
            sources: Vec::new(),
            fork_of: None,
            ignored: false,
        };
        (
            vec![
                repo(20, "me", "side-project", Some("Rust")),
                repo(21, "org", "empty", None),
            ],
            HashMap::from([(
                20,
                BTreeSet::from(["work".to_owned(), "someday".to_owned()]),
            )]),
        )
    }

    fn fixture() -> Fixture {
        let repos = [
            (1, "me/api", Some("Rust")),
            (2, "me/web", Some("TypeScript")),
            (3, "org/cli", Some("Rust")),
            (4, "me/notes", None),
        ];
        let facts: Vec<CloneFacts> = repos
            .iter()
            .map(|(id, full, language)| {
                let (owner, name) = full.split_once('/').unwrap();
                CloneFacts {
                    id: *id,
                    full_name: (*full).into(),
                    owner: Some(owner.into()),
                    name: name.into(),
                    language: language.map(Into::into),
                    ..Default::default()
                }
            })
            .collect();
        let rules = vec![
            Rule {
                id: 10,
                pattern: "(.+)".into(),
                tag_template: "lang:$1".into(),
                ..Rule::new(RuleField::Language)
            },
            Rule {
                id: 11,
                pattern: "^notes$".into(),
                tag_template: "personal".into(),
                ..Rule::new(RuleField::Name)
            },
        ];
        let manual = HashMap::from([
            (1, BTreeSet::from(["work".to_owned()])),
            (2, BTreeSet::from(["work".to_owned()])),
        ]);
        let index = TagIndex::build(&facts, &manual, &rules);
        let (repos, repo_manual) = uncloned();
        let repo_facts: Vec<CloneFacts> = repos.iter().map(CloneFacts::from_repo).collect();
        let repo_index = TagIndex::build_for_repos(&repo_facts, &repo_manual, &rules);
        Fixture {
            repos,
            facts,
            rules,
            index,
            repo_index,
        }
    }

    type State = (TagsPage, Fixture, Vec<TagsAction>);

    fn harness<'a>(builder: egui_kittest::HarnessBuilder<State>) -> Harness<'a, State> {
        builder.with_size([1200.0, 600.0]).build_ui_state(
            |ui, (page, fixture, actions): &mut State| {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    let cx = TagsContext {
                        repos: &fixture.repos,
                        facts: &fixture.facts,
                        index: &fixture.index,
                        repo_index: &fixture.repo_index,
                        rules: &fixture.rules,
                    };
                    if let Some(action) = page.show(ui, cx) {
                        actions.push(action);
                    }
                });
            },
            (TagsPage::new(), fixture(), Vec::new()),
        )
    }

    #[test]
    fn rule_editor_previews_and_saves() {
        let mut h = harness(Harness::builder());
        h.run();
        h.get_by_label("New rule").click();
        h.run();
        {
            let editor = h.state_mut().0.editor.as_mut().unwrap();
            editor.pattern = "(".into();
            editor.tag_template = "org".into();
        }
        h.run();
        assert!(h.query_by_label_contains("isn't a valid regex").is_some());

        h.state_mut().0.editor.as_mut().unwrap().pattern = "^org/".into();
        h.run();
        assert!(h.query_by_label("Tags 1 clone of 4").is_some());
        assert!(h.query_by_label("org/cli: org").is_some());
        assert!(
            h.query_by_label("Add exception").is_none(),
            "unsaved rules can't have exceptions"
        );
        h.get_by_label("Save rule").click();
        h.run();
        let Some(TagsAction::SaveRule(rule)) = h.state().2.first() else {
            panic!("expected a saved rule");
        };
        assert_eq!((rule.id, rule.pattern.as_str()), (0, "^org/"));
        assert!(h.state().0.editor.is_none());
    }

    #[test]
    fn exceptions_toggles_and_deletes() {
        let mut h = harness(Harness::builder());
        h.run();
        assert!(h.query_by_label_contains("tags 3 clones").is_some());
        h.get_by_label("language matches (.+): lang:$1").click();
        h.run();
        h.get_all_by_label("Edit").next().unwrap().click();
        h.run();
        assert!(h.query_by_label("Tags 3 clones of 4").is_some());
        h.get_all_by_label("Add exception").next().unwrap().click();
        h.run();
        assert!(h.query_by_label("Remove exception").is_some());
        h.get_all_by_label("Delete").nth(1).unwrap().click();
        h.run();
        assert_eq!(
            h.state().2,
            vec![
                TagsAction::SetRuleEnabled {
                    rule_id: 10,
                    enabled: false
                },
                TagsAction::SetRuleException {
                    rule_id: 10,
                    clone_id: 1,
                    excepted: true
                },
                TagsAction::DeleteRule(11),
            ]
        );
    }

    /// Renders the Tags tab when `DEPOSITORY_UI_SHOTS` names a folder.
    #[test]
    fn ui_shots() {
        let Some(dir) = std::env::var_os("DEPOSITORY_UI_SHOTS").map(PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = harness(Harness::builder().wgpu());
        h.run();
        h.get_all_by_label("Edit").next().unwrap().click();
        h.run();
        h.render()
            .unwrap()
            .save(dir.join("tags-rules.png"))
            .unwrap();
    }
}
