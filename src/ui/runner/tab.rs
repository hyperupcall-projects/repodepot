//! One run tab: its rows (one per repo), filters, search, grouping and output.

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, MutexGuard};
use std::time::Duration;

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use regex::{Regex, RegexBuilder};

use super::text::{Highlight, line_job, success_color};
use super::{one_line, plural, short_duration, shorten};
use crate::jobs::{Run, Slot, Target, TargetStatus};

/// One repo's result: which run it came from and its index there. Re-running replaces it.
#[derive(Clone)]
pub struct Row {
    pub run: Arc<Run>,
    pub index: usize,
}

impl Row {
    fn slot(&self) -> MutexGuard<'_, Slot> {
        self.run.slot(self.index)
    }

    pub fn target(&self) -> &Target {
        &self.run.targets[self.index]
    }

    fn status(&self) -> TargetStatus {
        self.run.status(self.index)
    }
}

/// What a tab asks the page to do.
pub enum TabAction {
    /// Run the tab's command again for these rows, replacing their results.
    Rerun(Vec<usize>),
    /// Run a command in these rows' repos, in a new tab.
    RunOnSelection(Vec<usize>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusFilter {
    All,
    Failed,
    Succeeded,
    Unfinished,
    Cancelled,
}

impl StatusFilter {
    const ALL: [StatusFilter; 5] = [
        StatusFilter::All,
        StatusFilter::Failed,
        StatusFilter::Succeeded,
        StatusFilter::Unfinished,
        StatusFilter::Cancelled,
    ];

    fn label(self) -> &'static str {
        match self {
            StatusFilter::All => "Any status",
            StatusFilter::Failed => "Failed",
            StatusFilter::Succeeded => "Succeeded",
            StatusFilter::Unfinished => "Running or queued",
            StatusFilter::Cancelled => "Cancelled",
        }
    }

    fn matches(self, status: TargetStatus) -> bool {
        match self {
            StatusFilter::All => true,
            StatusFilter::Failed => status.failed(),
            StatusFilter::Succeeded => matches!(status, TargetStatus::Exited { code: 0, .. }),
            StatusFilter::Unfinished => !status.is_finished(),
            StatusFilter::Cancelled => matches!(status, TargetStatus::Cancelled { .. }),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchMode {
    /// Show every repo; matches are highlighted.
    Highlight,
    OnlyMatching,
    OnlyNotMatching,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SortKey {
    Status,
    Name,
    Time,
    Lines,
    Matches,
}

/// What the view knows about a row this frame.
struct RowInfo {
    row: usize,
    name: String,
    status: TargetStatus,
    lines: usize,
    empty: bool,
    took: Option<Duration>,
    matches: usize,
    hash: u64,
}

/// Identifies a row's output as it is now: (run, index, lines kept, lines dropped).
type OutputKey = (u64, usize, usize, usize);

/// Search results for one row, valid while its output and the search stay the same.
struct MatchCache {
    key: (u64, usize, usize, usize, String),
    lines: Vec<usize>,
    count: usize,
}

pub struct RunTab {
    pub id: u64,
    pub title: String,
    pub command: String,
    pub shell: String,
    pub parallel: usize,
    pub colors: bool,
    /// Where this tab came from, e.g. "rust (24) › failed (5)".
    pub breadcrumb: Option<String>,
    pub rows: Vec<Row>,
    /// Every run whose results are in `rows`.
    pub runs: Vec<Arc<Run>>,
    pub(super) selected: Option<usize>,
    pub(super) ticked: BTreeSet<usize>,
    pub(super) sort: Option<(SortKey, bool)>,
    pub(super) status_filter: StatusFilter,
    hide_empty: bool,
    name_filter: String,
    pub(super) search: String,
    regex: bool,
    case_sensitive: bool,
    pub(super) search_mode: SearchMode,
    pub(super) group: bool,
    pub(super) current_match: Option<usize>,
    scroll_to_line: Option<usize>,
    match_cache: HashMap<usize, MatchCache>,
    hash_cache: HashMap<usize, (OutputKey, u64)>,
}

impl RunTab {
    pub fn new(id: u64, run: Arc<Run>, breadcrumb: Option<String>) -> Self {
        let rows = (0..run.targets.len())
            .map(|index| Row {
                run: run.clone(),
                index,
            })
            .collect();
        Self {
            id,
            title: run.title.clone(),
            command: run.command.clone(),
            shell: run.shell.clone(),
            parallel: run.parallel,
            colors: run.colors,
            breadcrumb,
            rows,
            runs: vec![run],
            selected: None,
            ticked: BTreeSet::new(),
            sort: None,
            status_filter: StatusFilter::All,
            hide_empty: false,
            name_filter: String::new(),
            search: String::new(),
            regex: false,
            case_sensitive: false,
            search_mode: SearchMode::Highlight,
            group: false,
            current_match: None,
            scroll_to_line: None,
            match_cache: HashMap::new(),
            hash_cache: HashMap::new(),
        }
    }

    /// Puts a new run's results in place of `rows` (in order).
    pub fn replace_rows(&mut self, run: Arc<Run>, rows: &[usize]) {
        for (index, row) in rows.iter().enumerate() {
            if let Some(slot) = self.rows.get_mut(*row) {
                *slot = Row {
                    run: run.clone(),
                    index,
                };
            }
        }
        self.runs.push(run);
    }

    pub fn is_finished(&self) -> bool {
        self.rows.iter().all(|r| r.status().is_finished())
    }

    pub fn has_failures(&self) -> bool {
        self.rows.iter().any(|r| r.status().failed())
    }

    pub fn cancel(&self) {
        for run in &self.runs {
            run.cancel();
        }
    }

    pub fn clear_output(&self) {
        for row in &self.rows {
            row.run.clear_output(row.index);
        }
    }

    /// A short label for breadcrumbs: the title and how many repos.
    pub fn label(&self) -> String {
        format!(
            "{} ({})",
            shorten(&one_line(&self.title), 24),
            self.rows.len()
        )
    }

    fn compiled_search(&self) -> Option<Result<Regex, String>> {
        let text = self.search.trim();
        if text.is_empty() {
            return None;
        }
        let pattern = if self.regex {
            text.to_owned()
        } else {
            regex::escape(text)
        };
        Some(
            RegexBuilder::new(&pattern)
                .case_insensitive(!self.case_sensitive)
                .size_limit(1 << 20)
                .build()
                .map_err(|e| format!("Not a valid regex: {e}")),
        )
    }

    fn search_key(&self) -> String {
        format!(
            "{}\u{0}{}\u{0}{}",
            self.search.trim(),
            self.regex,
            self.case_sensitive
        )
    }

    /// Line indices (in the current buffer) that match, and how many matches there are.
    fn matches_of(&mut self, row: usize, regex: &Regex) -> (Vec<usize>, usize) {
        let r = &self.rows[row];
        let slot = r.slot();
        let key = (
            r.run.id,
            r.index,
            slot.output.len(),
            slot.output.dropped(),
            self.search_key(),
        );
        if let Some(cache) = self.match_cache.get(&row)
            && cache.key == key
        {
            return (cache.lines.clone(), cache.count);
        }
        let mut lines = Vec::new();
        let mut count = 0;
        for (i, line) in slot.output.iter().enumerate() {
            let n = regex
                .find_iter(&line.text)
                .filter(|m| !m.is_empty())
                .count();
            if n > 0 {
                lines.push(i);
                count += n;
            }
        }
        drop(slot);
        self.match_cache.insert(
            row,
            MatchCache {
                key,
                lines: lines.clone(),
                count,
            },
        );
        (lines, count)
    }

    /// A fingerprint of a row's result, for grouping identical output.
    fn hash_of(&mut self, row: usize) -> u64 {
        let r = &self.rows[row];
        let status = r.status();
        let slot = r.slot();
        let key = (r.run.id, r.index, slot.output.len(), slot.output.dropped());
        if let Some((cached, hash)) = self.hash_cache.get(&row)
            && *cached == key
        {
            return *hash;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match status {
            TargetStatus::Exited { code, .. } => code.hash(&mut hasher),
            other => std::mem::discriminant(&other).hash(&mut hasher),
        }
        slot.output.dropped().hash(&mut hasher);
        for line in slot.output.iter() {
            line.stream.hash(&mut hasher);
            line.text.hash(&mut hasher);
        }
        let hash = hasher.finish();
        drop(slot);
        self.hash_cache.insert(row, (key, hash));
        hash
    }

    fn infos(&mut self, regex: Option<&Regex>) -> Vec<RowInfo> {
        (0..self.rows.len())
            .map(|row| {
                let (lines, empty, status, took) = {
                    let r = &self.rows[row];
                    let slot = r.slot();
                    (
                        slot.output.len() + slot.output.dropped(),
                        slot.output.is_empty(),
                        slot.status,
                        slot.status.took(),
                    )
                };
                let matches = regex.map_or(0, |regex| self.matches_of(row, regex).1);
                let hash = if self.group { self.hash_of(row) } else { 0 };
                RowInfo {
                    row,
                    name: self.rows[row].target().name.clone(),
                    status,
                    lines,
                    empty,
                    took,
                    matches,
                    hash,
                }
            })
            .collect()
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<TabAction> {
        let mut action = None;
        let search = self.compiled_search();
        let regex = search.as_ref().and_then(|r| r.as_ref().ok()).cloned();
        let infos = self.infos(regex.as_ref());

        self.header(ui, &infos, &mut action);
        self.filter_bar(ui, search.as_ref());

        // Which rows show, in which order, and grouped how.
        let needle = self.name_filter.trim().to_lowercase();
        let mut visible: Vec<&RowInfo> = infos
            .iter()
            .filter(|i| self.status_filter.matches(i.status))
            .filter(|i| !self.hide_empty || !i.empty)
            .filter(|i| needle.is_empty() || i.name.to_lowercase().contains(&needle))
            .filter(|i| match (self.search_mode, regex.is_some()) {
                (SearchMode::OnlyMatching, true) => i.matches > 0,
                (SearchMode::OnlyNotMatching, true) => i.matches == 0,
                _ => true,
            })
            .collect();
        if let Some((key, descending)) = self.sort {
            visible.sort_by(|a, b| {
                let order = match key {
                    SortKey::Status => status_rank(a.status).cmp(&status_rank(b.status)),
                    SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                    SortKey::Time => a.took.cmp(&b.took),
                    SortKey::Lines => a.lines.cmp(&b.lines),
                    SortKey::Matches => a.matches.cmp(&b.matches),
                };
                let order = order.then(a.row.cmp(&b.row));
                if descending { order.reverse() } else { order }
            });
        }
        let entries = group_entries(&visible, self.group);

        self.match_navigation(ui, &entries, regex.as_ref());
        self.tick_bar(ui, &infos, &visible, &mut action);
        ui.add_space(2.0);

        egui::Panel::left(("run_rows", self.id))
            .resizable(true)
            .default_size(380.0)
            .min_size(240.0)
            .show(ui, |ui| self.table(ui, &infos, &entries, regex.is_some()));

        match self.selected {
            Some(row) if row < self.rows.len() => {
                let members = entries
                    .iter()
                    .find(|e| e.contains(&row))
                    .cloned()
                    .unwrap_or_else(|| vec![row]);
                self.single_output(ui, row, &members, regex.as_ref());
            }
            _ => self.combined_output(ui, &entries, regex.as_ref()),
        }
        action
    }

    fn header(&mut self, ui: &mut egui::Ui, infos: &[RowInfo], action: &mut Option<TabAction>) {
        let failed: Vec<usize> = infos
            .iter()
            .filter(|i| i.status.failed())
            .map(|i| i.row)
            .collect();
        let finished = infos.iter().all(|i| i.status.is_finished());
        ui.horizontal(|ui| {
            let title = egui::RichText::new(shorten(&one_line(&self.title), 80)).strong();
            let title = if self.title == self.command {
                title.monospace()
            } else {
                title
            };
            ui.label(title).on_hover_text(&self.command);
            let elapsed = self.runs.last().map_or(Duration::ZERO, |r| r.elapsed());
            ui.weak(format!(
                "· {} · {} at once · {}",
                plural(self.rows.len(), "repo", "repos"),
                self.parallel,
                short_duration(elapsed)
            ))
            .on_hover_text(format!("Run with {} -c", self.shell));
            summary_chips(ui, infos);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !finished && ui.button("Cancel all").clicked() {
                    self.cancel();
                }
                if ui
                    .button("Clear output")
                    .on_hover_text("Forget this tab's output so far")
                    .clicked()
                {
                    self.clear_output();
                }
                if ui
                    .add_enabled(
                        finished && !failed.is_empty(),
                        egui::Button::new(format!("Re-run failed ({})", failed.len())),
                    )
                    .on_hover_text("Run again in the failed repos only, replacing their results")
                    .clicked()
                {
                    *action = Some(TabAction::Rerun(failed.clone()));
                }
                if ui
                    .add_enabled(finished, egui::Button::new("Re-run"))
                    .on_hover_text("Run again in every repo of this tab")
                    .clicked()
                {
                    *action = Some(TabAction::Rerun((0..self.rows.len()).collect()));
                }
            });
        });
        if let Some(breadcrumb) = &self.breadcrumb {
            ui.weak(format!("From {breadcrumb}"));
        }
    }

    fn filter_bar(&mut self, ui: &mut egui::Ui, search: Option<&Result<Regex, String>>) {
        ui.horizontal(|ui| {
            let label = ui.label("Show");
            egui::ComboBox::from_id_salt(("status_filter", self.id))
                .selected_text(self.status_filter.label())
                .show_ui(ui, |ui| {
                    for filter in StatusFilter::ALL {
                        ui.selectable_value(&mut self.status_filter, filter, filter.label());
                    }
                })
                .response
                .labelled_by(label.id);
            ui.checkbox(&mut self.hide_empty, "Hide empty");
            let label = ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut self.name_filter)
                    .hint_text("filter repos")
                    .desired_width(120.0),
            )
            .labelled_by(label.id);
            ui.checkbox(&mut self.group, "Group identical")
                .on_hover_text("Repos with exactly the same output share one row");
        });
        ui.horizontal(|ui| {
            let label = ui.label("Search");
            let edited = ui
                .add(
                    egui::TextEdit::singleline(&mut self.search)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("text in the output")
                        .desired_width(200.0),
                )
                .labelled_by(label.id)
                .changed();
            let toggled = ui.checkbox(&mut self.regex, "Regex").changed()
                | ui.checkbox(&mut self.case_sensitive, "Match case")
                    .changed();
            if edited || toggled {
                self.current_match = None;
            }
            if search.is_some() {
                let label = ui.label("Repos");
                let text = match self.search_mode {
                    SearchMode::Highlight => "all",
                    SearchMode::OnlyMatching => "with matches",
                    SearchMode::OnlyNotMatching => "without matches",
                };
                egui::ComboBox::from_id_salt(("search_mode", self.id))
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.search_mode, SearchMode::Highlight, "all");
                        ui.selectable_value(
                            &mut self.search_mode,
                            SearchMode::OnlyMatching,
                            "with matches",
                        );
                        ui.selectable_value(
                            &mut self.search_mode,
                            SearchMode::OnlyNotMatching,
                            "without matches",
                        );
                    })
                    .response
                    .labelled_by(label.id);
            }
            if let Some(Err(error)) = search {
                ui.colored_label(ui.visuals().warn_fg_color, error);
            }
        });
    }

    /// "Match 3 of 12" with previous/next buttons that jump to the repo and line.
    fn match_navigation(
        &mut self,
        ui: &mut egui::Ui,
        entries: &[Vec<usize>],
        regex: Option<&Regex>,
    ) {
        let Some(regex) = regex else {
            return;
        };
        let mut all: Vec<(usize, usize)> = Vec::new();
        for entry in entries {
            let row = entry[0];
            for line in self.matches_of(row, regex).0 {
                all.push((row, line));
            }
        }
        ui.horizontal(|ui| {
            if all.is_empty() {
                ui.weak("No matches");
                return;
            }
            let repos = entries
                .iter()
                .filter(|e| !self.matches_of(e[0], regex).0.is_empty())
                .count();
            let mut step = None;
            if ui.button("⏶ Previous").clicked() {
                step = Some(false);
            }
            if ui.button("⏷ Next").clicked() {
                step = Some(true);
            }
            if let Some(forward) = step {
                let next = match (self.current_match, forward) {
                    (None, true) => 0,
                    (None, false) => all.len() - 1,
                    (Some(i), true) => (i + 1) % all.len(),
                    (Some(i), false) => (i + all.len() - 1) % all.len(),
                };
                self.current_match = Some(next);
                self.selected = Some(all[next].0);
                self.scroll_to_line = Some(all[next].1);
            }
            let lines = plural(all.len(), "matching line", "matching lines");
            match self.current_match.filter(|i| *i < all.len()) {
                Some(i) => ui.label(format!(
                    "Line {} of {} in {}",
                    i + 1,
                    all.len(),
                    plural(repos, "repo", "repos")
                )),
                None => ui.label(format!("{lines} in {}", plural(repos, "repo", "repos"))),
            };
        });
    }

    fn tick_bar(
        &mut self,
        ui: &mut egui::Ui,
        infos: &[RowInfo],
        visible: &[&RowInfo],
        action: &mut Option<TabAction>,
    ) {
        ui.horizontal(|ui| {
            if ui.button("Tick shown").clicked() {
                self.ticked.extend(visible.iter().map(|i| i.row));
            }
            if ui.button("Tick failed").clicked() {
                self.ticked
                    .extend(infos.iter().filter(|i| i.status.failed()).map(|i| i.row));
            }
            if ui
                .add_enabled(!self.ticked.is_empty(), egui::Button::new("Clear ticks"))
                .clicked()
            {
                self.ticked.clear();
            }
            let count = self.ticked.len();
            if ui
                .add_enabled(
                    count > 0,
                    egui::Button::new(format!("Run on ticked ({count})")),
                )
                .on_hover_text("Run the command above in just these repos, in a new tab")
                .clicked()
            {
                *action = Some(TabAction::RunOnSelection(
                    self.ticked.iter().copied().collect(),
                ));
            }
        });
    }

    fn table(
        &mut self,
        ui: &mut egui::Ui,
        infos: &[RowInfo],
        entries: &[Vec<usize>],
        searching: bool,
    ) {
        if entries.is_empty() {
            ui.weak("No repo matches the filters.");
            return;
        }
        if ui
            .selectable_label(self.selected.is_none(), "All shown repos")
            .on_hover_text("Show every shown repo's output, one after another")
            .clicked()
        {
            self.selected = None;
        }
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let id = self.id;
        TableBuilder::new(ui)
            .id_salt(("run_table", id))
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(64.0))
            .column(Column::remainder().at_least(100.0).clip(true))
            .column(Column::exact(54.0))
            .column(Column::exact(44.0))
            .column(Column::exact(if searching { 56.0 } else { 0.0 }))
            .header(20.0, |mut header| {
                for (title, key) in [
                    ("Status", SortKey::Status),
                    ("Repository", SortKey::Name),
                    ("Time", SortKey::Time),
                    ("Lines", SortKey::Lines),
                    ("Hits", SortKey::Matches),
                ] {
                    header.col(|ui| {
                        if key == SortKey::Matches && !searching {
                            return;
                        }
                        let arrow = match self.sort {
                            Some((k, false)) if k == key => " ⏶",
                            Some((k, true)) if k == key => " ⏷",
                            _ => "",
                        };
                        let button = egui::Button::new(
                            egui::RichText::new(format!("{title}{arrow}")).strong(),
                        )
                        .frame(false);
                        if ui.add(button).on_hover_text("Sort").clicked() {
                            self.sort = match self.sort {
                                Some((k, false)) if k == key => Some((key, true)),
                                Some((k, true)) if k == key => None,
                                _ => Some((key, false)),
                            };
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(row_height, entries.len(), |mut row| {
                    let members = &entries[row.index()];
                    let info = &infos[members[0]];
                    let selected = self.selected.is_some_and(|s| members.contains(&s));
                    row.set_selected(selected);
                    row.col(|ui| status_cell(ui, info.status));
                    row.col(|ui| {
                        let mut ticked = members.iter().all(|r| self.ticked.contains(r));
                        let label = if members.len() == 1 {
                            info.name.clone()
                        } else {
                            format!("×{} {}, …", members.len(), info.name)
                        };
                        let response = ui.checkbox(&mut ticked, label);
                        if members.len() > 1 {
                            let names: Vec<&str> =
                                members.iter().map(|r| infos[*r].name.as_str()).collect();
                            response
                                .clone()
                                .on_hover_text(format!("Same output:\n{}", names.join("\n")));
                        }
                        if response.changed() {
                            for member in members {
                                if ticked {
                                    self.ticked.insert(*member);
                                } else {
                                    self.ticked.remove(member);
                                }
                            }
                        }
                    });
                    row.col(|ui| {
                        if let Some(took) = info.took {
                            ui.weak(short_duration(took));
                        }
                    });
                    row.col(|ui| {
                        if info.lines > 0 {
                            ui.weak(info.lines.to_string());
                        }
                    });
                    row.col(|ui| {
                        if searching && info.matches > 0 {
                            ui.label(info.matches.to_string());
                        }
                    });
                    if row.response().clicked() {
                        self.selected = if selected { None } else { Some(members[0]) };
                        self.scroll_to_line = None;
                    }
                });
            });
    }

    fn single_output(
        &mut self,
        ui: &mut egui::Ui,
        row: usize,
        members: &[usize],
        regex: Option<&Regex>,
    ) {
        let r = self.rows[row].clone();
        let target = r.target();
        let status = r.status();
        ui.horizontal(|ui| {
            ui.strong(&target.name);
            ui.weak(target.path.display().to_string());
            status_cell(ui, status);
            if !status.is_finished()
                && ui
                    .button("Cancel")
                    .on_hover_text("Cancel just this repo")
                    .clicked()
            {
                r.run.cancel_target(r.index);
            }
        });
        if members.len() > 1 {
            let names: Vec<&str> = members
                .iter()
                .skip(1)
                .map(|m| self.rows[*m].target().name.as_str())
                .collect();
            ui.weak(format!("Same output in: {}", names.join(", ")));
        }
        ui.separator();

        let match_lines: Vec<usize> = match regex {
            Some(regex) => self.matches_of(row, regex).0,
            None => Vec::new(),
        };
        let current_line = self.scroll_to_line;
        let (len, dropped) = {
            let slot = r.slot();
            (slot.output.len(), slot.output.dropped())
        };
        if len == 0 {
            ui.weak(if status.is_finished() {
                "(no output)"
            } else {
                "(no output yet)"
            });
            return;
        }
        let extra = usize::from(dropped > 0);
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        let mut area = egui::ScrollArea::both()
            .id_salt(("single_output", r.run.id, r.index))
            .auto_shrink(false)
            .stick_to_bottom(!status.is_finished() && current_line.is_none());
        if let Some(line) = self.scroll_to_line.take() {
            let spacing = ui.spacing().item_spacing.y;
            let offset =
                (line + extra) as f32 * (row_height + spacing) - ui.available_height() / 3.0;
            area = area.vertical_scroll_offset(offset.max(0.0));
        }
        area.show_rows(ui, row_height, len + extra, |ui, range| {
            let slot = r.slot();
            for i in range {
                if i < extra {
                    dropped_line(ui, dropped);
                } else if let Some(line) = slot.output.get(i - extra) {
                    let index = i - extra;
                    let highlight = if current_line == Some(index) && self.current_match.is_some() {
                        Highlight::Current
                    } else if match_lines.binary_search(&index).is_ok() {
                        Highlight::Matches
                    } else {
                        Highlight::None
                    };
                    ui.add(egui::Label::new(line_job(ui, line, regex, highlight)).extend());
                }
            }
        });
        if current_line.is_some() {
            self.scroll_to_line = None;
        }
    }

    fn combined_output(
        &mut self,
        ui: &mut egui::Ui,
        entries: &[Vec<usize>],
        regex: Option<&Regex>,
    ) {
        struct Segment {
            members: Vec<usize>,
            start: usize,
            dropped: usize,
            lines: usize,
        }
        impl Segment {
            fn rows(&self) -> usize {
                1 + usize::from(self.dropped > 0) + self.lines.max(1) + 1
            }
        }
        let mut segments = Vec::new();
        let mut total = 0;
        for members in entries {
            let row = &self.rows[members[0]];
            let status = row.status();
            if matches!(
                status,
                TargetStatus::Queued | TargetStatus::Cancelled { took: None }
            ) {
                continue;
            }
            let slot = row.slot();
            let segment = Segment {
                members: members.clone(),
                start: total,
                dropped: slot.output.dropped(),
                lines: slot.output.len(),
            };
            drop(slot);
            total += segment.rows();
            segments.push(segment);
        }
        if segments.is_empty() {
            ui.weak("Nothing to show yet.");
            return;
        }
        let match_lines: HashMap<usize, Vec<usize>> = match regex {
            Some(regex) => segments
                .iter()
                .map(|s| (s.members[0], self.matches_of(s.members[0], regex).0))
                .collect(),
            None => HashMap::new(),
        };

        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        egui::ScrollArea::both()
            .id_salt(("combined_output", self.id))
            .auto_shrink(false)
            .show_rows(ui, row_height, total, |ui, rows| {
                for i in rows {
                    let at = segments.partition_point(|s| s.start <= i) - 1;
                    let segment = &segments[at];
                    let offset = i - segment.start;
                    let row = &self.rows[segment.members[0]];
                    let has_dropped = usize::from(segment.dropped > 0);
                    if offset == 0 {
                        let names: Vec<&str> = segment
                            .members
                            .iter()
                            .map(|m| self.rows[*m].target().name.as_str())
                            .collect();
                        segment_header(ui, &names, row.status());
                    } else if offset == 1 && has_dropped == 1 {
                        dropped_line(ui, segment.dropped);
                    } else if offset == segment.rows() - 1 {
                        ui.label(" ");
                    } else {
                        let index = offset - 1 - has_dropped;
                        let slot = row.slot();
                        match slot.output.get(index) {
                            Some(line) => {
                                let hit = match_lines
                                    .get(&segment.members[0])
                                    .is_some_and(|l| l.binary_search(&index).is_ok());
                                let highlight = if hit {
                                    Highlight::Matches
                                } else {
                                    Highlight::None
                                };
                                ui.add(
                                    egui::Label::new(line_job(ui, line, regex, highlight)).extend(),
                                );
                            }
                            None => {
                                ui.weak(egui::RichText::new("(no output)").monospace());
                            }
                        }
                    }
                }
            });
    }
}

/// Groups rows whose (finished) output is identical, keeping display order. Unfinished rows
/// are never grouped.
fn group_entries(visible: &[&RowInfo], group: bool) -> Vec<Vec<usize>> {
    let mut entries: Vec<Vec<usize>> = Vec::new();
    let mut by_hash: HashMap<u64, usize> = HashMap::new();
    for info in visible {
        if group && info.status.is_finished() {
            if let Some(&at) = by_hash.get(&info.hash) {
                entries[at].push(info.row);
                continue;
            }
            by_hash.insert(info.hash, entries.len());
        }
        entries.push(vec![info.row]);
    }
    entries
}

fn status_rank(status: TargetStatus) -> u8 {
    match status {
        TargetStatus::Running { .. } => 0,
        TargetStatus::Queued => 1,
        s if s.failed() => 2,
        TargetStatus::Cancelled { .. } => 3,
        _ => 4,
    }
}

fn summary_chips(ui: &mut egui::Ui, infos: &[RowInfo]) {
    let count = |f: &dyn Fn(TargetStatus) -> bool| infos.iter().filter(|i| f(i.status)).count();
    let succeeded = count(&|s| matches!(s, TargetStatus::Exited { code: 0, .. }));
    let failed = count(&|s| s.failed());
    let running = count(&|s| matches!(s, TargetStatus::Running { .. }));
    let queued = count(&|s| matches!(s, TargetStatus::Queued));
    let cancelled = count(&|s| matches!(s, TargetStatus::Cancelled { .. }));
    if succeeded > 0 {
        ui.colored_label(success_color(ui), format!("✔ {succeeded}"));
    }
    if failed > 0 {
        ui.colored_label(ui.visuals().error_fg_color, format!("✖ {failed}"));
    }
    if running > 0 {
        ui.spinner();
        ui.label(format!("{running} running"));
    }
    if queued > 0 {
        ui.weak(format!("{queued} queued"));
    }
    if cancelled > 0 {
        ui.weak(format!("{cancelled} cancelled"));
    }
}

pub fn status_cell(ui: &mut egui::Ui, status: TargetStatus) {
    match status {
        TargetStatus::Queued => {
            ui.weak("queued");
        }
        TargetStatus::Running { .. } => {
            ui.spinner();
        }
        TargetStatus::Exited { code: 0, .. } => {
            ui.colored_label(success_color(ui), "✔");
        }
        TargetStatus::Exited { code, .. } => {
            ui.colored_label(ui.visuals().error_fg_color, format!("✖ {code}"))
                .on_hover_text(format!("Exited with code {code}"));
        }
        TargetStatus::Killed { signal, .. } => {
            ui.colored_label(ui.visuals().error_fg_color, format!("sig {signal}"))
                .on_hover_text(format!("Killed by signal {signal}"));
        }
        TargetStatus::TimedOut { .. } => {
            ui.colored_label(ui.visuals().error_fg_color, "timeout");
        }
        TargetStatus::Cancelled { .. } => {
            ui.weak("cancelled");
        }
        TargetStatus::FailedToStart => {
            ui.colored_label(ui.visuals().error_fg_color, "failed")
                .on_hover_text("Couldn't start; the reason is in the output");
        }
    }
}

fn segment_header(ui: &mut egui::Ui, names: &[&str], status: TargetStatus) {
    ui.horizontal(|ui| {
        let title = if names.len() == 1 {
            format!("── {}", names[0])
        } else {
            format!("── ×{} {}", names.len(), names.join(", "))
        };
        ui.label(egui::RichText::new(title).monospace().strong());
        status_cell(ui, status);
    });
}

fn dropped_line(ui: &mut egui::Ui, dropped: usize) {
    ui.weak(
        egui::RichText::new(format!(
            "… {} dropped (over the line limit)",
            plural(dropped, "earlier line", "earlier lines")
        ))
        .monospace(),
    );
}
