//! The Tags tab: groups (tag sidebar, query, working set) and rules.

use std::collections::{BTreeMap, HashSet};

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher};

use crate::backend::LauncherInfo;
use crate::db::RepoRow;
use crate::git::scan::CloneInfo;
use crate::launch;
use crate::tags::query::{self, Query};
use crate::tags::{self, CloneFacts, Rule, RuleField, TagCount, TagIndex, Tagging};
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
    /// Open the Runner with these clones as the targets.
    RunOn {
        label: String,
        clone_ids: Vec<i64>,
    },
    /// Open clones in an app, each in its own window or all together.
    Open {
        launcher: String,
        together: bool,
        clone_ids: Vec<i64>,
        workspace_name: String,
    },
}

pub struct TagsContext<'a> {
    pub clones: &'a [CloneInfo],
    /// Repos in the Remote List: listed, not cloned, not ignored.
    pub repos: &'a [RepoRow],
    pub facts: &'a [CloneFacts],
    pub index: &'a TagIndex,
    /// Tags of repos that aren't cloned, by repo ID.
    pub repo_index: &'a TagIndex,
    pub rules: &'a [Rule],
    pub launchers: &'a [LauncherInfo],
    /// Ask before opening more windows than this.
    pub confirm_over: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Groups,
    Rules,
}

/// One entry of the working set: a clone, or a repo that isn't cloned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Item {
    Clone(i64),
    Repo(i64),
}

#[derive(Clone, Copy)]
enum Row<'a> {
    Clone(&'a CloneInfo),
    Repo(&'a RepoRow),
}

impl Row<'_> {
    fn item(self) -> Item {
        match self {
            Row::Clone(clone) => Item::Clone(clone.id),
            Row::Repo(repo) => Item::Repo(repo.id),
        }
    }

    fn name(self) -> String {
        match self {
            Row::Clone(clone) => clone.display_name(),
            Row::Repo(repo) => repo.full_name(),
        }
    }

    fn tagging<'i>(self, cx: &'i TagsContext<'_>) -> Option<&'i Tagging> {
        match self {
            Row::Clone(clone) => cx.index.by_clone.get(&clone.id),
            Row::Repo(repo) => cx.repo_index.by_clone.get(&repo.id),
        }
    }
}

/// The clone IDs and the repo IDs in a set of items.
fn split(items: &[Item]) -> (Vec<i64>, Vec<i64>) {
    let mut clones = Vec::new();
    let mut repos = Vec::new();
    for item in items {
        match item {
            Item::Clone(id) => clones.push(*id),
            Item::Repo(id) => repos.push(*id),
        }
    }
    (clones, repos)
}

/// "3 clones", or "3 clones and 2 repos not cloned".
fn describe_count(clones: usize, repos: usize, include_repos: bool) -> String {
    let clones = plural(clones, "clone", "clones");
    if include_repos {
        format!("{clones} and {} not cloned", plural(repos, "repo", "repos"))
    } else {
        clones
    }
}

pub struct TagsPage {
    view: View,
    query: String,
    /// The last parsed query, keyed by its text.
    parsed: Option<(String, Result<Query, String>)>,
    /// Also list repos from the Remote List, which can be tagged but not opened or run on.
    include_repos: bool,
    /// Query matches unticked from the working set.
    excluded: HashSet<Item>,
    /// Clones (or repos) added to the working set by hand.
    extra: Vec<Item>,
    picker: String,
    tag_name: String,
    editor: Option<Rule>,
    error: Option<String>,
    /// "Open each" waiting for confirmation: (launcher, clone IDs).
    pending_open: Option<(String, Vec<i64>)>,
}

impl TagsPage {
    pub fn new() -> Self {
        Self {
            view: View::Groups,
            query: String::new(),
            parsed: None,
            include_repos: false,
            excluded: HashSet::new(),
            extra: Vec::new(),
            picker: String::new(),
            tag_name: String::new(),
            editor: None,
            error: None,
            pending_open: None,
        }
    }

    /// Names the workspace file: the query when the set is exactly its result, otherwise a
    /// hash of the clones, so the same group always reuses the same file. `set` is clone IDs.
    fn workspace_name(&self, set: &[i64]) -> String {
        let unchanged = self.excluded.is_empty() && self.extra.is_empty();
        match self.query.trim() {
            "*" if unchanged => "all-clones".to_owned(),
            query if unchanged && !query.is_empty() => query.to_owned(),
            _ => launch::set_name(set),
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
        let include = self.include_repos;
        // Tag counts over the Remote List's repos (the repo index also holds Ignored ones).
        let mut repo_counts = BTreeMap::new();
        cx.repo_index
            .count_into(cx.repos.iter().map(|r| r.id), &mut repo_counts);
        let counts: BTreeMap<String, TagCount> = if include {
            let mut merged = cx.index.counts.clone();
            for (tag, count) in &repo_counts {
                let entry = merged.entry(tag.clone()).or_default();
                entry.total += count.total;
                entry.manual += count.manual;
                entry.auto += count.auto;
            }
            merged
        } else {
            cx.index.counts.clone()
        };

        egui::Panel::left("tag_sidebar")
            .resizable(true)
            .default_size(220.0)
            .min_size(160.0)
            .show(ui, |ui| self.sidebar(ui, cx, &counts));

        ui.horizontal(|ui| {
            let label = ui.label("Query");
            let edited = ui
                .add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("rust AND NOT archived, or untagged")
                        .desired_width(320.0),
                )
                .labelled_by(label.id)
                .changed();
            if edited {
                self.excluded.clear();
                self.extra.clear();
            }
            ui.checkbox(&mut self.include_repos, "Include repos not cloned")
                .on_hover_text(
                    "Also list repos from the Remote List. They can be tagged, \
                     but not opened or run on until they're cloned.",
                );
        });

        let parsed = self.parsed_query();
        let mut matched: Vec<Row<'_>> = Vec::new();
        if let Ok(query) = &parsed {
            matched.extend(
                cx.clones
                    .iter()
                    .filter(|c| query.matches(&cx.index.tags_of(c.id)))
                    .map(Row::Clone),
            );
            if include {
                matched.extend(
                    cx.repos
                        .iter()
                        .filter(|r| query.matches(&cx.repo_index.tags_of(r.id)))
                        .map(Row::Repo),
                );
            }
        }
        match &parsed {
            Err(_) if self.query.trim().is_empty() => {
                ui.weak(
                    "Pick a tag on the left, or type a query. * means everything, \
                     untagged means no tags.",
                );
            }
            Err(error) => {
                ui.colored_label(ui.visuals().warn_fg_color, error);
            }
            Ok(query) => {
                let clones = matched
                    .iter()
                    .filter(|r| matches!(r, Row::Clone(_)))
                    .count();
                let repos = matched.len() - clones;
                let unknown: Vec<&str> = query
                    .tags()
                    .into_iter()
                    .filter(|t| !counts.contains_key(*t))
                    .collect();
                ui.horizontal(|ui| {
                    ui.weak(format!("{} match", describe_count(clones, repos, include)));
                    let (only_uncloned, nowhere): (Vec<&str>, Vec<&str>) = unknown
                        .iter()
                        .partition(|t| !include && repo_counts.contains_key(**t));
                    if !nowhere.is_empty() {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!(
                                "· no {} has the tag {}",
                                if include { "repo" } else { "clone" },
                                nowhere.join(", ")
                            ),
                        );
                    }
                    if !only_uncloned.is_empty() {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!(
                                "· only repos not cloned have the tag {}",
                                only_uncloned.join(", ")
                            ),
                        );
                    }
                });
            }
        }

        let matched_items: HashSet<Item> = matched.iter().map(|r| r.item()).collect();
        self.extra
            .retain(|i| !matched_items.contains(i) && (include || matches!(i, Item::Clone(_))));
        let mut rows: Vec<Row<'_>> = matched.clone();
        rows.extend(self.extra.iter().filter_map(|item| match item {
            Item::Clone(id) => cx.clones.iter().find(|c| c.id == *id).map(Row::Clone),
            Item::Repo(id) => cx.repos.iter().find(|r| r.id == *id).map(Row::Repo),
        }));
        let in_set = |item: Item, excluded: &HashSet<Item>, extra: &[Item]| {
            (matched_items.contains(&item) && !excluded.contains(&item)) || extra.contains(&item)
        };
        let set: Vec<Item> = rows
            .iter()
            .map(|r| r.item())
            .filter(|item| in_set(*item, &self.excluded, &self.extra))
            .collect();
        let (clone_set, repo_set) = split(&set);

        ui.add_space(4.0);
        self.set_actions(ui, &clone_set, &repo_set, action);
        self.open_actions(ui, cx, &clone_set, repo_set.len(), action);
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
                    let entry = rows[row.index()];
                    let item = entry.item();
                    let member = in_set(item, &self.excluded, &self.extra);
                    row.set_selected(member);
                    row.col(|ui| {
                        if member {
                            ui.label("✔");
                        }
                    });
                    row.col(|ui| {
                        ui.label(entry.name());
                        if !matched_items.contains(&item) {
                            ui.weak("added");
                        }
                    });
                    row.col(|ui| match entry {
                        Row::Clone(clone) => {
                            ui.weak(&clone.rel_path);
                        }
                        Row::Repo(_) => {
                            ui.weak("not cloned").on_hover_text(
                                "In the Remote List. Clone it there to open it or run on it.",
                            );
                        }
                    });
                    row.col(|ui| {
                        if let Some(tagging) = entry.tagging(cx) {
                            tag_summary(ui, tagging);
                        }
                    });
                    if row.response().clicked() {
                        if !matched_items.contains(&item) {
                            self.extra.retain(|i| *i != item);
                        } else if !self.excluded.remove(&item) {
                            self.excluded.insert(item);
                        }
                    }
                });
            });
    }

    fn sidebar(
        &mut self,
        ui: &mut egui::Ui,
        cx: &TagsContext<'_>,
        counts: &BTreeMap<String, TagCount>,
    ) {
        let include = self.include_repos;
        ui.add_space(4.0);
        let all = if include {
            format!("All repos ({})", cx.clones.len() + cx.repos.len())
        } else {
            format!("All clones ({})", cx.clones.len())
        };
        if ui.selectable_label(self.query.trim() == "*", all).clicked() {
            self.set_query("*");
        }
        let mut untagged = cx
            .clones
            .iter()
            .filter(|c| cx.index.is_untagged(c.id))
            .count();
        if include {
            untagged += cx
                .repos
                .iter()
                .filter(|r| cx.repo_index.is_untagged(r.id))
                .count();
        }
        if ui
            .selectable_label(
                self.query.trim().eq_ignore_ascii_case("untagged"),
                format!("Untagged ({untagged})"),
            )
            .on_hover_text("No tags at all, manual or automatic")
            .clicked()
        {
            self.set_query("untagged");
        }
        ui.separator();
        if counts.is_empty() {
            ui.weak(
                "No tags yet. Add one from a clone's details or the Remote List on the \
                 Repositories tab, or make a rule.",
            );
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("tag_sidebar_scroll")
            .auto_shrink(false)
            .show(ui, |ui| {
                for (tag, count) in counts {
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

    fn set_actions(
        &mut self,
        ui: &mut egui::Ui,
        clones: &[i64],
        repos: &[i64],
        action: &mut Option<TagsAction>,
    ) {
        ui.horizontal(|ui| {
            ui.label(format!("{} in the working set", clones.len() + repos.len()));
            if !repos.is_empty() {
                ui.weak(format!("· {} not cloned", repos.len()));
            }
            let skipped = if repos.is_empty() {
                "Open the Runner with these clones".to_owned()
            } else {
                format!(
                    "Open the Runner with the {}; repos that aren't cloned are skipped",
                    plural(clones.len(), "clone", "clones")
                )
            };
            if ui
                .add_enabled(!clones.is_empty(), egui::Button::new("Run command…"))
                .on_hover_text(skipped)
                .clicked()
            {
                let label = match self.query.trim() {
                    "" | "*" => "working set".to_owned(),
                    query => query.to_owned(),
                };
                *action = Some(TagsAction::RunOn {
                    label,
                    clone_ids: clones.to_vec(),
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
            let enabled = valid.is_ok() && !(clones.is_empty() && repos.is_empty());
            let hover = valid.as_ref().err().cloned().unwrap_or_default();
            if ui
                .add_enabled(enabled, egui::Button::new("Add to all"))
                .on_disabled_hover_text(&hover)
                .clicked()
            {
                *action = Some(TagsAction::AddTag {
                    clone_ids: clones.to_vec(),
                    repo_ids: repos.to_vec(),
                    tag: std::mem::take(&mut self.tag_name),
                });
            }
            if ui
                .add_enabled(enabled, egui::Button::new("Remove from all"))
                .on_hover_text("Removes manual tags only; automatic ones need an exception")
                .clicked()
            {
                *action = Some(TagsAction::RemoveTag {
                    clone_ids: clones.to_vec(),
                    repo_ids: repos.to_vec(),
                    tag: std::mem::take(&mut self.tag_name),
                });
            }
        });
    }

    /// Open each / Open together for the clones in the set (`set` is clone IDs).
    fn open_actions(
        &mut self,
        ui: &mut egui::Ui,
        cx: &TagsContext<'_>,
        set: &[i64],
        not_cloned: usize,
        action: &mut Option<TagsAction>,
    ) {
        ui.horizontal(|ui| {
            let each: Vec<&LauncherInfo> = cx.launchers.iter().filter(|l| l.each).collect();
            let together: Vec<&LauncherInfo> = cx.launchers.iter().filter(|l| l.together).collect();
            ui.add_enabled_ui(!set.is_empty() && !each.is_empty(), |ui| {
                ui.menu_button("Open each", |ui| {
                    for launcher in &each {
                        if ui.button(&launcher.name).clicked() {
                            if set.len() > cx.confirm_over {
                                self.pending_open = Some((launcher.name.clone(), set.to_vec()));
                            } else {
                                *action = Some(TagsAction::Open {
                                    launcher: launcher.name.clone(),
                                    together: false,
                                    clone_ids: set.to_vec(),
                                    workspace_name: String::new(),
                                });
                            }
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text("One window per repo");
            });
            ui.add_enabled_ui(!set.is_empty() && !together.is_empty(), |ui| {
                ui.menu_button("Open together", |ui| {
                    for launcher in &together {
                        if ui.button(&launcher.name).clicked() {
                            *action = Some(TagsAction::Open {
                                launcher: launcher.name.clone(),
                                together: true,
                                clone_ids: set.to_vec(),
                                workspace_name: self.workspace_name(set),
                            });
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text("Every repo in one window, for apps that support workspaces");
            });
            if cx.launchers.is_empty() {
                ui.weak("No editors found. Add one in Settings.");
            }
            if not_cloned > 0 {
                ui.weak(format!(
                    "· {} not cloned can't be opened",
                    plural(not_cloned, "repo", "repos")
                ));
            }
        });
        if let Some((launcher, ids)) = self.pending_open.clone() {
            ui.horizontal(|ui| {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Open {} windows in {launcher}?", ids.len()),
                );
                if ui.button("Open them").clicked() {
                    *action = Some(TagsAction::Open {
                        launcher,
                        together: false,
                        clone_ids: ids,
                        workspace_name: String::new(),
                    });
                    self.pending_open = None;
                }
                if ui.button("Cancel").clicked() {
                    self.pending_open = None;
                }
            });
        }
    }

    /// Fuzzy search for clones (and, when included, repos not cloned) to add to the working set.
    fn picker(&mut self, ui: &mut egui::Ui, cx: &TagsContext<'_>, rows: &[Row<'_>]) {
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
            item: Item,
        }
        impl AsRef<str> for Candidate {
            fn as_ref(&self) -> &str {
                &self.text
            }
        }
        let shown: HashSet<Item> = rows.iter().map(|r| r.item()).collect();
        let mut candidates: Vec<Candidate> = cx
            .clones
            .iter()
            .map(|c| Candidate {
                text: format!("{}  {}", c.display_name(), c.rel_path),
                item: Item::Clone(c.id),
            })
            .collect();
        if self.include_repos {
            candidates.extend(cx.repos.iter().map(|r| Candidate {
                text: format!("{}  (not cloned)", r.full_name()),
                item: Item::Repo(r.id),
            }));
        }
        candidates.retain(|c| !shown.contains(&c.item));
        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let pattern = Pattern::parse(&self.picker, CaseMatching::Ignore, Normalization::Smart);
        let found = pattern.match_list(candidates.iter(), &mut matcher);
        ui.horizontal_wrapped(|ui| {
            if found.is_empty() {
                ui.weak("Nothing else matches.");
            }
            for (candidate, _) in found.into_iter().take(8) {
                if ui.button(&candidate.text).clicked() {
                    self.extra.push(candidate.item);
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
    use crate::git::scan::MainRemote;

    struct Fixture {
        clones: Vec<CloneInfo>,
        repos: Vec<RepoRow>,
        facts: Vec<CloneFacts>,
        rules: Vec<Rule>,
        index: TagIndex,
        repo_index: TagIndex,
        launchers: Vec<LauncherInfo>,
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
        let launcher = |name: &str, together: bool| LauncherInfo {
            name: name.into(),
            each: true,
            together,
        };
        Fixture {
            clones,
            repos,
            facts,
            rules,
            index,
            repo_index,
            launchers: vec![launcher("VS Code", true), launcher("Kate", false)],
        }
    }

    type State = (TagsPage, Fixture, Vec<TagsAction>);

    fn harness<'a>(builder: egui_kittest::HarnessBuilder<State>) -> Harness<'a, State> {
        builder.with_size([1200.0, 600.0]).build_ui_state(
            |ui, (page, fixture, actions): &mut State| {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    let cx = TagsContext {
                        clones: &fixture.clones,
                        repos: &fixture.repos,
                        facts: &fixture.facts,
                        index: &fixture.index,
                        repo_index: &fixture.repo_index,
                        rules: &fixture.rules,
                        launchers: &fixture.launchers,
                        confirm_over: 2,
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
                    repo_ids: vec![],
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
    fn repos_not_cloned_are_left_out_until_included() {
        let mut h = harness(Harness::builder());
        h.run();
        assert!(h.query_by_label("All clones (4)").is_some());
        assert!(h.query_by_label("Untagged (0)").is_some());
        assert!(h.query_by_label("work  (2)").is_some());
        h.state_mut().0.query = "someday".into();
        h.run();
        assert!(h.query_by_label("0 clones match").is_some());
        assert!(
            h.query_by_label_contains("only repos not cloned have the tag someday")
                .is_some()
        );

        h.get_by_label("Include repos not cloned").click();
        h.run();
        assert!(
            h.query_by_label("0 clones and 1 repo not cloned match")
                .is_some()
        );
        assert!(h.query_by_label("All repos (6)").is_some());
        assert!(h.query_by_label("Untagged (1)").is_some());
        assert!(h.query_by_label("work  (3)").is_some());
        assert!(h.query_by_label("me/side-project").is_some());
        assert!(h.query_by_label("not cloned").is_some());

        h.get_by_label("Untagged (1)").click();
        h.run();
        assert!(h.query_by_label("org/empty").is_some());
        assert!(h.query_by_label("me/side-project").is_none());

        // Untick the toggle and the repos leave the working set.
        h.get_by_label("Include repos not cloned").click();
        h.run();
        assert!(h.query_by_label("org/empty").is_none());
        assert!(h.query_by_label("0 in the working set").is_some());
    }

    #[test]
    fn working_set_with_repos_tags_all_but_opens_and_runs_clones() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.include_repos = true;
        h.state_mut().0.query = "work".into();
        h.run();
        assert!(
            h.query_by_label("2 clones and 1 repo not cloned match")
                .is_some()
        );
        assert!(h.query_by_label("3 in the working set").is_some());
        assert!(h.query_by_label("· 1 not cloned").is_some());
        assert!(
            h.query_by_label("· 1 repo not cloned can't be opened")
                .is_some()
        );

        h.state_mut().0.picker = "empty".into();
        h.run();
        h.get_by_label("org/empty  (not cloned)").click();
        h.run();
        assert!(h.query_by_label("4 in the working set").is_some());

        h.state_mut().0.tag_name = "q4".into();
        h.run();
        h.get_by_label("Add to all").click();
        h.run();
        h.get_by_label("Run command…").click();
        h.run();
        h.get_by_label("Open together").click();
        h.run();
        h.get_by_label("VS Code").click();
        h.run();
        assert_eq!(
            h.state().2,
            vec![
                TagsAction::AddTag {
                    clone_ids: vec![1, 2],
                    repo_ids: vec![20, 21],
                    tag: "q4".into()
                },
                TagsAction::RunOn {
                    label: "work".into(),
                    clone_ids: vec![1, 2]
                },
                TagsAction::Open {
                    launcher: "VS Code".into(),
                    together: true,
                    clone_ids: vec![1, 2],
                    workspace_name: launch::set_name(&[1, 2]),
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

    #[test]
    fn opening_the_working_set() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.query = "lang:rust".into();
        h.run();
        h.get_by_label("Open each").click();
        h.run();
        h.get_by_label("VS Code").click();
        h.run();
        h.get_by_label("Open together").click();
        h.run();
        assert!(
            h.query_by_label("Kate").is_none(),
            "Kate can't open several in one window"
        );
        h.get_by_label("VS Code").click();
        h.run();

        // Over the limit (2 in this test) asks first.
        h.state_mut().0.query = "*".into();
        h.run();
        h.get_by_label("Open each").click();
        h.run();
        h.get_by_label("Kate").click();
        h.run();
        assert!(h.query_by_label("Open 4 windows in Kate?").is_some());
        h.get_by_label("Open them").click();
        h.run();
        assert_eq!(
            h.state().2,
            vec![
                TagsAction::Open {
                    launcher: "VS Code".into(),
                    together: false,
                    clone_ids: vec![1, 3],
                    workspace_name: String::new(),
                },
                TagsAction::Open {
                    launcher: "VS Code".into(),
                    together: true,
                    clone_ids: vec![1, 3],
                    workspace_name: "lang:rust".into(),
                },
                TagsAction::Open {
                    launcher: "Kate".into(),
                    together: false,
                    clone_ids: vec![1, 2, 3, 4],
                    workspace_name: String::new(),
                },
            ]
        );
    }

    #[test]
    fn workspace_names() {
        let mut page = TagsPage::new();
        page.set_query("rust");
        assert_eq!(page.workspace_name(&[1, 2]), "rust");
        page.excluded.insert(Item::Clone(2));
        assert_eq!(page.workspace_name(&[1]), launch::set_name(&[1]));
        page.set_query("*");
        assert_eq!(page.workspace_name(&[1, 2]), "all-clones");
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
        h.state_mut().0.include_repos = true;
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
