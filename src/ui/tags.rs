//! The Tags tab: groups (tag sidebar, query, working set) and rules.

use std::collections::HashSet;

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher};

use crate::git::scan::CloneInfo;
use crate::tags::query::{self, Query};
use crate::tags::{self, CloneFacts, Rule, RuleField, TagIndex, Tagging};

#[derive(Debug, PartialEq, Eq)]
pub enum TagsAction {
    AddTag {
        clone_ids: Vec<i64>,
        tag: String,
    },
    RemoveTag {
        clone_ids: Vec<i64>,
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
    /// Open the Runner with these clones as the targets.
    RunOn {
        label: String,
        clone_ids: Vec<i64>,
    },
}

pub struct TagsContext<'a> {
    pub clones: &'a [CloneInfo],
    pub facts: &'a [CloneFacts],
    pub index: &'a TagIndex,
    pub rules: &'a [Rule],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Groups,
    Rules,
}

pub struct TagsPage {
    view: View,
    query: String,
    /// The last parsed query, keyed by its text.
    parsed: Option<(String, Result<Query, String>)>,
    /// Query matches unticked from the working set.
    excluded: HashSet<i64>,
    /// Clones added to the working set by hand.
    extra: Vec<i64>,
    picker: String,
    tag_name: String,
    editor: Option<Rule>,
    error: Option<String>,
}

impl TagsPage {
    pub fn new() -> Self {
        Self {
            view: View::Groups,
            query: String::new(),
            parsed: None,
            excluded: HashSet::new(),
            extra: Vec::new(),
            picker: String::new(),
            tag_name: String::new(),
            editor: None,
            error: None,
        }
    }

    pub fn action_failed(&mut self, message: String) {
        self.error = Some(message);
    }

    fn set_query(&mut self, query: &str) {
        self.query = query.to_owned();
        self.excluded.clear();
        self.extra.clear();
    }

    fn parsed_query(&mut self) -> Result<Query, String> {
        let text = self.query.trim().to_owned();
        if self
            .parsed
            .as_ref()
            .is_none_or(|(cached, _)| *cached != text)
        {
            self.parsed = Some((text.clone(), query::parse(&text)));
        }
        self.parsed.as_ref().expect("just set").1.clone()
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: TagsContext<'_>) -> Option<TagsAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, View::Groups, "Groups");
            ui.selectable_value(
                &mut self.view,
                View::Rules,
                format!("Rules ({})", cx.rules.len()),
            );
        });
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
        ui.separator();
        match self.view {
            View::Groups => self.groups(ui, &cx, &mut action),
            View::Rules => self.rules(ui, &cx, &mut action),
        }
        if action.is_some() {
            self.error = None;
        }
        action
    }

    fn groups(&mut self, ui: &mut egui::Ui, cx: &TagsContext<'_>, action: &mut Option<TagsAction>) {
        egui::Panel::left("tag_sidebar")
            .resizable(true)
            .default_size(220.0)
            .min_size(160.0)
            .show(ui, |ui| self.sidebar(ui, cx));

        ui.horizontal(|ui| {
            let label = ui.label("Query");
            let edited = ui
                .add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("rust AND NOT archived")
                        .desired_width(320.0),
                )
                .labelled_by(label.id)
                .changed();
            if edited {
                self.excluded.clear();
                self.extra.clear();
            }
        });

        let parsed = self.parsed_query();
        let matched: Vec<&CloneInfo> = match &parsed {
            Ok(query) => cx
                .clones
                .iter()
                .filter(|c| query.matches(&cx.index.tags_of(c.id)))
                .collect(),
            Err(_) => Vec::new(),
        };
        match &parsed {
            Err(_) if self.query.trim().is_empty() => {
                ui.weak("Pick a tag on the left, or type a query. * means every clone.");
            }
            Err(error) => {
                ui.colored_label(ui.visuals().warn_fg_color, error);
            }
            Ok(query) => {
                let unknown: Vec<&str> = query
                    .tags()
                    .into_iter()
                    .filter(|t| !cx.index.counts.contains_key(*t))
                    .collect();
                ui.horizontal(|ui| {
                    ui.weak(format!(
                        "{} match",
                        plural(matched.len(), "clone", "clones")
                    ));
                    if !unknown.is_empty() {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!("· no clone has the tag {}", unknown.join(", ")),
                        );
                    }
                });
            }
        }

        let matched_ids: HashSet<i64> = matched.iter().map(|c| c.id).collect();
        self.extra.retain(|id| !matched_ids.contains(id));
        let mut rows: Vec<&CloneInfo> = matched.clone();
        rows.extend(
            self.extra
                .iter()
                .filter_map(|id| cx.clones.iter().find(|c| c.id == *id)),
        );
        let in_set = |id: i64, excluded: &HashSet<i64>, extra: &[i64]| {
            (matched_ids.contains(&id) && !excluded.contains(&id)) || extra.contains(&id)
        };
        let set: Vec<i64> = rows
            .iter()
            .map(|c| c.id)
            .filter(|id| in_set(*id, &self.excluded, &self.extra))
            .collect();

        ui.add_space(4.0);
        self.set_actions(ui, &set, action);
        self.picker(ui, cx, &rows);
        ui.add_space(4.0);

        if rows.is_empty() {
            return;
        }
        ui.style_mut().interaction.selectable_labels = false;
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        TableBuilder::new(ui)
            .id_salt("working_set")
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(26.0))
            .column(
                Column::initial(240.0)
                    .at_least(120.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(
                Column::initial(160.0)
                    .at_least(80.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(Column::remainder().at_least(120.0).clip(true))
            .header(22.0, |mut header| {
                for title in ["", "Repository", "Folder", "Tags"] {
                    header.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(row_height, rows.len(), |mut row| {
                    let clone = rows[row.index()];
                    let member = in_set(clone.id, &self.excluded, &self.extra);
                    row.set_selected(member);
                    row.col(|ui| {
                        if member {
                            ui.label("✔");
                        }
                    });
                    row.col(|ui| {
                        ui.label(clone.display_name());
                        if !matched_ids.contains(&clone.id) {
                            ui.weak("added");
                        }
                    });
                    row.col(|ui| {
                        ui.weak(&clone.rel_path);
                    });
                    row.col(|ui| {
                        if let Some(tagging) = cx.index.by_clone.get(&clone.id) {
                            tag_summary(ui, tagging);
                        }
                    });
                    if row.response().clicked() {
                        if !matched_ids.contains(&clone.id) {
                            self.extra.retain(|id| *id != clone.id);
                        } else if !self.excluded.remove(&clone.id) {
                            self.excluded.insert(clone.id);
                        }
                    }
                });
            });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, cx: &TagsContext<'_>) {
        ui.add_space(4.0);
        if ui
            .selectable_label(
                self.query.trim() == "*",
                format!("All clones ({})", cx.clones.len()),
            )
            .clicked()
        {
            self.set_query("*");
        }
        ui.separator();
        if cx.index.counts.is_empty() {
            ui.weak(
                "No tags yet. Add one from a clone's details on the Repositories tab, or make a rule.",
            );
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("tag_sidebar_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                for (tag, count) in &cx.index.counts {
                    let clicked = ui
                        .selectable_label(
                            self.query.trim() == tag,
                            format!("{tag}  ({})", count.total),
                        )
                        .on_hover_text(format!("{} manual, {} automatic", count.manual, count.auto))
                        .clicked();
                    if clicked {
                        self.set_query(tag);
                    }
                }
            });
    }

    fn set_actions(&mut self, ui: &mut egui::Ui, set: &[i64], action: &mut Option<TagsAction>) {
        ui.horizontal(|ui| {
            ui.label(format!("{} in the working set", set.len()));
            if ui
                .add_enabled(!set.is_empty(), egui::Button::new("Run command…"))
                .on_hover_text("Open the Runner with these clones")
                .clicked()
            {
                let label = match self.query.trim() {
                    "" | "*" => "working set".to_owned(),
                    query => query.to_owned(),
                };
                *action = Some(TagsAction::RunOn {
                    label,
                    clone_ids: set.to_vec(),
                });
            }
            ui.separator();
            let label = ui.label("Tag");
            ui.add(
                egui::TextEdit::singleline(&mut self.tag_name)
                    .hint_text("tag name")
                    .desired_width(140.0),
            )
            .labelled_by(label.id);
            let valid = tags::normalize_tag(&self.tag_name);
            let enabled = valid.is_ok() && !set.is_empty();
            let hover = valid.as_ref().err().cloned().unwrap_or_default();
            if ui
                .add_enabled(enabled, egui::Button::new("Add to all"))
                .on_disabled_hover_text(&hover)
                .clicked()
            {
                *action = Some(TagsAction::AddTag {
                    clone_ids: set.to_vec(),
                    tag: std::mem::take(&mut self.tag_name),
                });
            }
            if ui
                .add_enabled(enabled, egui::Button::new("Remove from all"))
                .on_hover_text("Removes manual tags only; automatic ones need an exception")
                .clicked()
            {
                *action = Some(TagsAction::RemoveTag {
                    clone_ids: set.to_vec(),
                    tag: std::mem::take(&mut self.tag_name),
                });
            }
        });
    }

    /// Fuzzy search for clones to add to the working set.
    fn picker(&mut self, ui: &mut egui::Ui, cx: &TagsContext<'_>, rows: &[&CloneInfo]) {
        ui.horizontal(|ui| {
            let label = ui.label("Add a repo");
            ui.add(
                egui::TextEdit::singleline(&mut self.picker)
                    .hint_text("fuzzy search by name or folder")
                    .desired_width(260.0),
            )
            .labelled_by(label.id);
        });
        if self.picker.trim().is_empty() {
            return;
        }
        struct Candidate {
            text: String,
            id: i64,
        }
        impl AsRef<str> for Candidate {
            fn as_ref(&self) -> &str {
                &self.text
            }
        }
        let shown: HashSet<i64> = rows.iter().map(|c| c.id).collect();
        let candidates: Vec<Candidate> = cx
            .clones
            .iter()
            .filter(|c| !shown.contains(&c.id))
            .map(|c| Candidate {
                text: format!("{}  {}", c.display_name(), c.rel_path),
                id: c.id,
            })
            .collect();
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let pattern = Pattern::parse(&self.picker, CaseMatching::Ignore, Normalization::Smart);
        let found = pattern.match_list(candidates.iter(), &mut matcher);
        ui.horizontal_wrapped(|ui| {
            if found.is_empty() {
                ui.weak("No other clone matches.");
            }
            for (candidate, _) in found.into_iter().take(8) {
                if ui.button(&candidate.text).clicked() {
                    self.extra.push(candidate.id);
                    self.picker.clear();
                }
            }
        });
    }

    fn rules(&mut self, ui: &mut egui::Ui, cx: &TagsContext<'_>, action: &mut Option<TagsAction>) {
        ui.horizontal(|ui| {
            if ui.button("New rule").clicked() {
                self.editor = Some(Rule::new(RuleField::FullName));
            }
            ui.weak(
                "Rules add tags automatically and apply live: changing one updates every clone.",
            );
        });
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
                ui.weak(format!("· tags {}", plural(tagged, "clone", "clones")));
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
                                    let excepted = rule.exceptions.contains(&facts.clone_id);
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
                                            rule.exceptions.insert(facts.clone_id);
                                        } else {
                                            rule.exceptions.remove(&facts.clone_id);
                                        }
                                        *action = Some(TagsAction::SetRuleException {
                                            rule_id: rule.id,
                                            clone_id: facts.clone_id,
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

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::path::PathBuf;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::git::scan::MainRemote;

    struct Fixture {
        clones: Vec<CloneInfo>,
        facts: Vec<CloneFacts>,
        rules: Vec<Rule>,
        index: TagIndex,
    }

    fn fixture() -> Fixture {
        let repos = [
            (1, "me/api", Some("Rust")),
            (2, "me/web", Some("TypeScript")),
            (3, "org/cli", Some("Rust")),
            (4, "me/notes", None),
        ];
        let clones = repos
            .iter()
            .map(|(id, full, _)| {
                let name = full.split_once('/').unwrap().1;
                CloneInfo {
                    id: *id,
                    repo_id: None,
                    rel_path: format!("{}/{name}", &name[..1]),
                    abs_path: PathBuf::from("/code").join(name),
                    remotes: Vec::new(),
                    main: MainRemote::NoRemotes,
                    status: Ok(Default::default()),
                    problems: Vec::new(),
                }
            })
            .collect();
        let facts: Vec<CloneFacts> = repos
            .iter()
            .map(|(id, full, language)| {
                let (owner, name) = full.split_once('/').unwrap();
                CloneFacts {
                    clone_id: *id,
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
        Fixture {
            clones,
            facts,
            rules,
            index,
        }
    }

    type State = (TagsPage, Fixture, Vec<TagsAction>);

    fn harness<'a>(builder: egui_kittest::HarnessBuilder<State>) -> Harness<'a, State> {
        builder.with_size([1200.0, 600.0]).build_ui_state(
            |ui, (page, fixture, actions): &mut State| {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    let cx = TagsContext {
                        clones: &fixture.clones,
                        facts: &fixture.facts,
                        index: &fixture.index,
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
    fn sidebar_and_query_build_the_working_set() {
        let mut h = harness(Harness::builder());
        h.run();
        assert!(h.query_by_label("work  (2)").is_some());
        assert!(h.query_by_label("personal  (1)").is_some());
        h.get_by_label("lang:rust  (2)").click();
        h.run();
        assert_eq!(h.state().0.query, "lang:rust");
        assert!(h.query_by_label("2 clones match").is_some());
        assert!(h.query_by_label("2 in the working set").is_some());

        h.state_mut().0.query = "work AND NOT lang:typescript".into();
        h.run();
        assert!(h.query_by_label("1 clone match").is_some());
        assert!(h.query_by_label("a/api").is_some());

        h.state_mut().0.query = "work AND".into();
        h.run();
        assert!(h.query_by_label_contains("ends too early").is_some());

        h.state_mut().0.query = "nosuch".into();
        h.run();
        assert!(
            h.query_by_label_contains("no clone has the tag nosuch")
                .is_some()
        );

        h.get_by_label("All clones (4)").click();
        h.run();
        assert!(h.query_by_label("4 in the working set").is_some());
    }

    #[test]
    fn working_set_can_be_adjusted_then_used() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.query = "lang:rust".into();
        h.run();
        h.get_by_label("c/cli").click();
        h.run();
        assert!(h.query_by_label("1 in the working set").is_some());

        h.state_mut().0.picker = "nots".into();
        h.run();
        h.get_by_label("notes  n/notes").click();
        h.run();
        assert!(h.query_by_label("2 in the working set").is_some());
        assert!(h.query_by_label("added").is_some());

        h.state_mut().0.tag_name = "Needs Review".into();
        h.run();
        h.get_by_label("Add to all").click();
        h.run();
        h.get_by_label("Run command…").click();
        h.run();
        assert_eq!(
            h.state().2,
            vec![
                TagsAction::AddTag {
                    clone_ids: vec![1, 4],
                    tag: "Needs Review".into()
                },
                TagsAction::RunOn {
                    label: "lang:rust".into(),
                    clone_ids: vec![1, 4]
                },
            ]
        );
    }

    #[test]
    fn rule_editor_previews_and_saves() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.view = View::Rules;
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
        h.state_mut().0.view = View::Rules;
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
        h.state_mut().0.query = "work OR lang:rust".into();
        h.state_mut().0.picker = "no".into();
        h.run();
        h.render()
            .unwrap()
            .save(dir.join("tags-groups.png"))
            .unwrap();
        h.state_mut().0.view = View::Rules;
        h.state_mut().0.picker.clear();
        h.run();
        h.get_all_by_label("Edit").next().unwrap().click();
        h.run();
        h.render()
            .unwrap()
            .save(dir.join("tags-rules.png"))
            .unwrap();
    }
}
