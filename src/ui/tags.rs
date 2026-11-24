//! The Tags view of the Repositories tab: a table of the rules that tag clones and repos
//! automatically, a search over them, and the rule editor. Tags are added by hand, and groups
//! picked by tag, in the other views.

use std::collections::{BTreeSet, HashMap};

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::db::RepoRow;
use crate::tags::{self, CloneFacts, Rule, RuleField, TagIndex, Tagging};
use crate::ui::{cmp_text, plural, present_first, sort_header, thousands};

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

/// The columns the rules table can be sorted by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortKey {
    Enabled,
    When,
    Pattern,
    Tag,
    Clones,
    Repos,
    Exceptions,
}

impl SortKey {
    /// Counts read best biggest first.
    fn starts_descending(self) -> bool {
        matches!(self, SortKey::Clones | SortKey::Repos | SortKey::Exceptions)
    }
}

/// A rule, with what it does right now.
struct RuleRow<'a> {
    rule: &'a Rule,
    /// Clones it tags.
    clones: usize,
    /// Repos in the Remote List it tags.
    repos: usize,
    /// The tags it gives them.
    adds: BTreeSet<&'a str>,
}

impl RuleRow<'_> {
    /// `search` is lowercase. Yes/no rules have no pattern to search.
    fn matches(&self, search: &str) -> bool {
        let has = |text: &str| text.to_lowercase().contains(search);
        (!self.rule.field.is_flag() && has(&self.rule.pattern))
            || has(&self.rule.tag_template)
            || self.adds.iter().any(|tag| has(tag))
    }
}

/// Every rule, with the clones and Remote List repos it tags and the tags it gives them.
fn rule_rows<'a>(cx: &TagsContext<'a>) -> Vec<RuleRow<'a>> {
    let (index, repo_index): (&'a TagIndex, &'a TagIndex) = (cx.index, cx.repo_index);
    let mut rows: Vec<RuleRow<'a>> = cx
        .rules
        .iter()
        .map(|rule| RuleRow {
            rule,
            clones: 0,
            repos: 0,
            adds: BTreeSet::new(),
        })
        .collect();
    let position: HashMap<i64, usize> = cx
        .rules
        .iter()
        .enumerate()
        .map(|(i, rule)| (rule.id, i))
        .collect();
    let mut add = |tagging: &'a Tagging, is_clone: bool| {
        // A rule that gives one clone several tags still tags one clone.
        let mut counted = BTreeSet::new();
        for (tag, rule_ids) in &tagging.auto {
            for id in rule_ids {
                let Some(&i) = position.get(id) else {
                    continue;
                };
                let row = &mut rows[i];
                row.adds.insert(tag.as_str());
                if counted.insert(*id) {
                    if is_clone {
                        row.clones += 1;
                    } else {
                        row.repos += 1;
                    }
                }
            }
        }
    };
    for tagging in index.by_clone.values() {
        add(tagging, true);
    }
    for repo in cx.repos {
        if let Some(tagging) = repo_index.by_clone.get(&repo.id) {
            add(tagging, false);
        }
    }
    rows
}

/// Sorts by `key`. Yes/no rules have no pattern, so they go last either way when sorting by
/// it. Ties go by tag.
fn sort_rows(rows: &mut [RuleRow<'_>], key: SortKey, descending: bool) {
    // The order the editor's drop-down lists the fields in: text fields, then yes/no ones.
    let field = |row: &RuleRow<'_>| RuleField::ALL.iter().position(|f| *f == row.rule.field);
    fn pattern<'a>(row: &RuleRow<'a>) -> Option<&'a str> {
        (!row.rule.field.is_flag()).then_some(row.rule.pattern.as_str())
    }
    rows.sort_by(|a, b| {
        let count = |x: usize, y: usize| present_first(Some(x), Some(y), descending, Ord::cmp);
        let order = match key {
            // Rules that are on come first.
            SortKey::Enabled => present_first(
                Some(!a.rule.enabled),
                Some(!b.rule.enabled),
                descending,
                Ord::cmp,
            ),
            SortKey::When => present_first(field(a), field(b), descending, Ord::cmp),
            SortKey::Pattern => {
                present_first(pattern(a), pattern(b), descending, |x, y| cmp_text(x, y))
            }
            SortKey::Tag => present_first(
                Some(&a.rule.tag_template),
                Some(&b.rule.tag_template),
                descending,
                |x, y| cmp_text(x, y),
            ),
            SortKey::Clones => count(a.clones, b.clones),
            SortKey::Repos => count(a.repos, b.repos),
            SortKey::Exceptions => count(a.rule.exceptions.len(), b.rule.exceptions.len()),
        };
        order
            .then_with(|| cmp_text(&a.rule.tag_template, &b.rule.tag_template))
            .then_with(|| a.rule.id.cmp(&b.rule.id))
    });
}

pub struct TagsPage {
    /// The rule being written or edited, shown above the table.
    editor: Option<Rule>,
    /// Part of a rule's pattern, its tag, or a tag it gives.
    search: String,
    sort: SortKey,
    descending: bool,
}

impl TagsPage {
    pub fn new() -> Self {
        Self {
            editor: None,
            search: String::new(),
            sort: SortKey::Tag,
            descending: false,
        }
    }

    pub fn has_search(&self) -> bool {
        !self.search.trim().is_empty()
    }

    pub fn clear_search(&mut self) {
        self.search.clear();
    }

    /// The search field, for the view's own filter row.
    pub fn search_field(&mut self, ui: &mut egui::Ui) {
        let label = ui.label("Search");
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Pattern or tag")
                .desired_width(220.0),
        )
        .labelled_by(label.id)
        .on_hover_text(
            "Part of a rule's pattern, of the tag it adds, or of a tag it gives right now \
             (lang:rust finds lang:$1). Ignores case.",
        );
    }

    /// The rules the table lists: those the search matches, sorted.
    fn shown_rows<'a>(&self, cx: &TagsContext<'a>) -> Vec<RuleRow<'a>> {
        let search = self.search.trim().to_lowercase();
        let mut rows: Vec<RuleRow<'a>> = rule_rows(cx)
            .into_iter()
            .filter(|row| row.matches(&search))
            .collect();
        sort_rows(&mut rows, self.sort, self.descending);
        rows
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: TagsContext<'_>) -> Option<TagsAction> {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            if ui.button("New rule").clicked() {
                self.editor = Some(Rule::new(RuleField::FullName));
            }
            ui.weak(
                "Rules add tags automatically and apply live: changing one updates every clone \
                 and repo. Add tags by hand, and pick repos by tag, in the other views.",
            );
        });
        ui.add_space(4.0);
        if self.editor.is_some() {
            self.rule_editor(ui, &cx, &mut action);
            ui.add_space(8.0);
        }

        if cx.rules.is_empty() {
            ui.weak(
                "No rules yet. For example: language matches ^rust$ adds rust, or \
                 owner/name matches ^([^/]+)/ adds org:$1.",
            );
            return action;
        }
        let rows = self.shown_rows(&cx);
        if rows.is_empty() {
            ui.weak("Nothing matches the search.");
            return action;
        }
        // In a narrow window, scroll sideways rather than squeezing the columns.
        egui::ScrollArea::horizontal()
            .id_salt("rules_scroll")
            .auto_shrink(false)
            .show(ui, |ui| self.table(ui, &rows, &cx, &mut action));
        action
    }

    fn table(
        &mut self,
        ui: &mut egui::Ui,
        rows: &[RuleRow<'_>],
        cx: &TagsContext<'_>,
        action: &mut Option<TagsAction>,
    ) {
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        let editing = self.editor.as_ref().map(|rule| rule.id);
        TableBuilder::new(ui)
            .id_salt("rules")
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(45.0).at_least(40.0))
            .column(
                Column::initial(130.0)
                    .at_least(80.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(
                Column::initial(240.0)
                    .at_least(80.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(
                Column::initial(260.0)
                    .at_least(80.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(Column::initial(75.0).at_least(65.0))
            .column(Column::initial(105.0).at_least(95.0))
            .column(Column::initial(95.0).at_least(90.0))
            .column(Column::remainder().at_least(110.0))
            .header(24.0, |mut header| {
                for (title, key) in [
                    ("On", SortKey::Enabled),
                    ("When", SortKey::When),
                    ("Pattern", SortKey::Pattern),
                    ("Tag", SortKey::Tag),
                    ("Clones", SortKey::Clones),
                    ("Remote List", SortKey::Repos),
                    ("Exceptions", SortKey::Exceptions),
                ] {
                    header.col(|ui| {
                        sort_header(
                            ui,
                            title,
                            key,
                            &mut self.sort,
                            &mut self.descending,
                            key.starts_descending(),
                        );
                    });
                }
                header.col(|_| {});
            })
            .body(|body| {
                body.rows(row_height, rows.len(), |mut row| {
                    let RuleRow {
                        rule,
                        clones,
                        repos,
                        adds,
                    } = &rows[row.index()];
                    row.set_selected(editing == Some(rule.id));
                    row.col(|ui| {
                        let mut enabled = rule.enabled;
                        if ui
                            .add(egui::Checkbox::without_text(&mut enabled))
                            .on_hover_text(if rule.enabled {
                                "On: it adds its tags. Untick to turn it off."
                            } else {
                                "Off: it adds nothing. Tick to turn it on."
                            })
                            .changed()
                        {
                            *action = Some(TagsAction::SetRuleEnabled {
                                rule_id: rule.id,
                                enabled,
                            });
                        }
                    });
                    row.col(|ui| {
                        ui.label(rule.flag_condition().unwrap_or(rule.field.label()));
                    });
                    row.col(|ui| pattern_cell(ui, rule, cx.index.rule_errors.get(&rule.id)));
                    row.col(|ui| tag_cell(ui, rule, adds));
                    row.col(|ui| count_cell(ui, *clones));
                    row.col(|ui| count_cell(ui, *repos));
                    row.col(|ui| count_cell(ui, rule.exceptions.len()));
                    row.col(|ui| {
                        if ui.small_button("Edit").clicked() {
                            self.editor = Some((*rule).clone());
                        }
                        if ui.small_button("Delete").clicked() {
                            *action = Some(TagsAction::DeleteRule(rule.id));
                            if editing == Some(rule.id) {
                                self.editor = None;
                            }
                        }
                    });
                });
            });
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

/// The regex, monospaced. Yes/no rules have none. A rule that doesn't compile (only possible
/// if the database was edited by hand) is flagged.
fn pattern_cell(ui: &mut egui::Ui, rule: &Rule, error: Option<&String>) {
    if let Some(error) = error {
        ui.colored_label(ui.visuals().error_fg_color, "Invalid")
            .on_hover_text(error);
    }
    if !rule.field.is_flag() {
        ui.label(egui::RichText::new(&rule.pattern).monospace())
            .on_hover_text(if rule.case_insensitive {
                "Ignores case"
            } else {
                "Matches case"
            });
    }
}

/// The tag the rule adds, followed by the tags it gives right now when those differ (a
/// capture group like `lang:$1` gives one per language).
fn tag_cell(ui: &mut egui::Ui, rule: &Rule, adds: &BTreeSet<&str>) {
    ui.label(&rule.tag_template);
    let plain = tags::normalize_tag(&rule.tag_template).ok();
    if adds.iter().any(|tag| Some(*tag) != plain.as_deref()) {
        let list = adds.iter().copied().collect::<Vec<_>>().join(", ");
        ui.weak(format!("gives {list}"))
            .on_hover_text(format!("Gives {list} right now"));
    }
}

fn count_cell(ui: &mut egui::Ui, n: usize) {
    let text = thousands(n as u64);
    if n == 0 {
        ui.weak(text);
    } else {
        ui.label(text);
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

    use egui::accesskit::Role;
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

    impl Fixture {
        fn cx(&self) -> TagsContext<'_> {
            TagsContext {
                repos: &self.repos,
                facts: &self.facts,
                index: &self.index,
                repo_index: &self.repo_index,
                rules: &self.rules,
            }
        }
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

    /// Three rules:
    /// - 10: language `(.+)` adds `lang:$1`: 3 clones (lang:rust, lang:typescript), 1 repo
    /// - 11: name `^notes$` adds `personal`: 1 clone
    /// - 12: isn't archived adds `active`: no clones (they weren't synced), 2 repos
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
                exceptions: BTreeSet::from([2, 3]),
                ..Rule::new(RuleField::Name)
            },
            Rule {
                id: 12,
                pattern: "no".into(),
                tag_template: "active".into(),
                ..Rule::new(RuleField::Archived)
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
                    ui.horizontal(|ui| page.search_field(ui));
                    if let Some(action) = page.show(ui, fixture.cx()) {
                        actions.push(action);
                    }
                });
            },
            (TagsPage::new(), fixture(), Vec::new()),
        )
    }

    /// The IDs of the rules the table lists, top to bottom.
    fn shown(page: &TagsPage, fixture: &Fixture) -> Vec<i64> {
        page.shown_rows(&fixture.cx())
            .iter()
            .map(|row| row.rule.id)
            .collect()
    }

    #[test]
    fn rows_count_what_each_rule_tags() {
        let fixture = fixture();
        let rows = rule_rows(&fixture.cx());
        let summary: Vec<(i64, usize, usize, Vec<&str>)> = rows
            .iter()
            .map(|row| {
                (
                    row.rule.id,
                    row.clones,
                    row.repos,
                    row.adds.iter().copied().collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (10, 3, 1, vec!["lang:rust", "lang:typescript"]),
                (11, 1, 0, vec!["personal"]),
                (12, 0, 2, vec!["active"]),
            ]
        );
    }

    #[test]
    fn rules_sort_by_each_column() {
        let fixture = fixture();
        let mut page = TagsPage::new();
        let mut order = |key: SortKey, descending: bool| {
            page.sort = key;
            page.descending = descending;
            shown(&page, &fixture)
        };
        assert_eq!(order(SortKey::Tag, false), [12, 10, 11], "by tag at first");
        assert_eq!(order(SortKey::Tag, true), [11, 10, 12]);
        assert_eq!(
            order(SortKey::When, false),
            [11, 10, 12],
            "in the editor's order"
        );
        // Yes/no rules have no pattern: last both ways.
        assert_eq!(order(SortKey::Pattern, false), [10, 11, 12]);
        assert_eq!(order(SortKey::Pattern, true), [11, 10, 12]);
        assert_eq!(order(SortKey::Clones, true), [10, 11, 12]);
        assert_eq!(order(SortKey::Repos, true), [12, 10, 11]);
        // Ties go by tag, whichever way the column is sorted.
        assert_eq!(order(SortKey::Exceptions, true), [11, 12, 10]);
        assert_eq!(order(SortKey::Exceptions, false), [12, 10, 11]);
        assert_eq!(order(SortKey::Enabled, false), [12, 10, 11]);
    }

    #[test]
    fn clicking_a_column_title_sorts_and_reverses() {
        let mut h = harness(Harness::builder());
        h.run();
        assert!(h.query_by_label("Tag ⏶").is_some());
        // Counts start biggest first.
        h.get_by_label("Clones").click();
        h.run();
        assert_eq!(shown(&h.state().0, &h.state().1), [10, 11, 12]);
        h.get_by_label("Clones ⏷").click();
        h.run();
        assert_eq!(shown(&h.state().0, &h.state().1), [12, 11, 10]);
        h.get_by_label("Pattern").click();
        h.run();
        assert_eq!(shown(&h.state().0, &h.state().1), [10, 11, 12]);
    }

    #[test]
    fn search_finds_patterns_tags_and_the_tags_rules_give() {
        let fixture = fixture();
        let mut page = TagsPage::new();
        let mut found = |text: &str| {
            page.search = text.into();
            shown(&page, &fixture)
        };
        assert_eq!(found(""), [12, 10, 11]);
        assert_eq!(found("notes"), [11], "the pattern");
        assert_eq!(
            found(" PERSONAL "),
            [11],
            "the tag, ignoring case and spaces"
        );
        assert_eq!(found("lang:$"), [10], "the tag as written");
        assert_eq!(found("typescript"), [10], "a tag the rule gives");
        assert_eq!(found("act"), [12]);
        // Yes/no rules' "yes" and "no" aren't patterns.
        assert_eq!(found("no"), [11]);
        assert_eq!(found("zzz"), Vec::<i64>::new());
    }

    #[test]
    fn typing_a_search_narrows_the_table() {
        let mut h = harness(Harness::builder());
        h.run();
        assert!(h.query_by_label("personal").is_some());
        h.get_by_role_and_label(Role::TextInput, "Search").focus();
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Search")
            .type_text("rust");
        h.run();
        assert_eq!(h.state().0.search, "rust");
        assert!(h.query_by_label("lang:$1").is_some());
        assert!(
            h.query_by_label("gives lang:rust, lang:typescript")
                .is_some()
        );
        assert!(h.query_by_label("personal").is_none());
        h.state_mut().0.search = "nothing".into();
        h.run();
        assert!(h.query_by_label("Nothing matches the search.").is_some());
        assert!(h.state().0.has_search());
        h.state_mut().0.clear_search();
        assert!(!h.state().0.has_search());
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
        // Sorted by tag: active (12), lang:$1 (10), personal (11).
        h.get_all_by_role(Role::CheckBox).nth(1).unwrap().click();
        h.run();
        h.get_all_by_label("Edit").nth(1).unwrap().click();
        h.run();
        assert_eq!(h.state().0.editor.as_ref().map(|r| r.id), Some(10));
        assert!(h.query_by_label("Tags 3 clones of 4").is_some());
        h.get_all_by_label("Add exception").next().unwrap().click();
        h.run();
        assert!(h.query_by_label("Remove exception").is_some());
        h.get_all_by_label("Delete").nth(2).unwrap().click();
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

    #[test]
    fn no_rules_says_how_to_start() {
        let mut h = harness(Harness::builder());
        h.state_mut().1.rules.clear();
        h.run();
        assert!(h.query_by_label_contains("No rules yet").is_some());
        assert!(h.query_by_label("Tag ⏶").is_none());
    }

    /// Renders the Tags view when `DEPOSITORY_UI_SHOTS` names a folder.
    #[test]
    fn ui_shots() {
        let Some(dir) = std::env::var_os("DEPOSITORY_UI_SHOTS").map(PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = harness(Harness::builder().wgpu());
        h.run();
        h.render().unwrap().save(dir.join("tags.png")).unwrap();
        h.get_all_by_label("Edit").nth(1).unwrap().click();
        h.run();
        h.render()
            .unwrap()
            .save(dir.join("tags-rules.png"))
            .unwrap();
    }
}
