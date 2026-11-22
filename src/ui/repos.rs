//! The Repositories tab: every clone with its status, plus a Problems view.

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::git::scan::{CloneInfo, MainRemote, Problem, ScanResult};
use crate::git::status::Status;

#[derive(Debug, PartialEq, Eq)]
pub enum ReposAction {
    Rescan,
    SetMainRemote {
        clone_id: i64,
        remote: Option<String>,
    },
    ForgetClone(i64),
    OpenSettings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Clones,
    Problems,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Show {
    All,
    Changes,
    OutOfSync,
    Stashes,
    Problems,
}

impl Show {
    const ALL: [Show; 5] = [
        Show::All,
        Show::Changes,
        Show::OutOfSync,
        Show::Stashes,
        Show::Problems,
    ];

    fn label(self) -> &'static str {
        match self {
            Show::All => "All clones",
            Show::Changes => "Uncommitted changes",
            Show::OutOfSync => "Ahead, behind or upstream gone",
            Show::Stashes => "Has stashes",
            Show::Problems => "Has problems",
        }
    }

    fn matches(self, clone: &CloneInfo) -> bool {
        let status = clone.status.as_ref().ok();
        match self {
            Show::All => true,
            Show::Changes => status.is_some_and(|s| s.total_changes() > 0),
            Show::OutOfSync => status.is_some_and(Status::out_of_sync),
            Show::Stashes => status.is_some_and(|s| s.stashes > 0),
            Show::Problems => !clone.problems.is_empty(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Folder,
    Branch,
    Changes,
    Sync,
}

/// Problem groups, in the order the Problems view shows them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProblemKind {
    NoMainRemote,
    NoRemotes,
    NotHosted,
    OtherHost,
    OutOfLayout,
    GitError,
}

impl ProblemKind {
    const ALL: [ProblemKind; 6] = [
        ProblemKind::NoMainRemote,
        ProblemKind::OutOfLayout,
        ProblemKind::NoRemotes,
        ProblemKind::NotHosted,
        ProblemKind::OtherHost,
        ProblemKind::GitError,
    ];

    fn of(problem: &Problem) -> Self {
        match problem {
            Problem::NoRemotes => ProblemKind::NoRemotes,
            Problem::NoMainRemote => ProblemKind::NoMainRemote,
            Problem::NotHosted { .. } => ProblemKind::NotHosted,
            Problem::OtherHost { .. } => ProblemKind::OtherHost,
            Problem::OutOfLayout { .. } => ProblemKind::OutOfLayout,
            Problem::GitError { .. } => ProblemKind::GitError,
        }
    }

    fn title(self) -> &'static str {
        match self {
            ProblemKind::NoMainRemote => "Main remote not chosen",
            ProblemKind::NoRemotes => "No remotes",
            ProblemKind::NotHosted => "Main remote isn't a hosted repo",
            ProblemKind::OtherHost => "On another host",
            ProblemKind::OutOfLayout => "Outside the layout",
            ProblemKind::GitError => "git couldn't read these",
        }
    }

    fn help(self) -> &'static str {
        match self {
            ProblemKind::NoMainRemote => {
                "These have several remotes, and none has the default remote name or is called \
                 origin. Pick the remote that says which repo the folder is."
            }
            ProblemKind::NoRemotes => {
                "Local-only repos. They stay listed, but can't be matched to anything on GitHub."
            }
            ProblemKind::NotHosted => {
                "The main remote is a local path or a URL this app can't read."
            }
            ProblemKind::OtherHost => {
                "The main remote isn't on GitHub. These still count as clones and can be tagged."
            }
            ProblemKind::OutOfLayout => {
                "Not at <letter>/<repo name>. Import will be able to move them into place."
            }
            ProblemKind::GitError => "git failed when reading these folders.",
        }
    }
}

const MISSING_HELP: &str = "An earlier scan found these, but their folder is gone. A folder moved \
    within the clone root is matched back to its old entry by its main remote. Forget removes the \
    entry and its tags.";
const STRAYS_HELP: &str = "Folders under the clone root that aren't git repos. Only \
    <letter>/<name> is searched, so Import will be able to look inside these for repos.";

pub struct ReposPage {
    view: View,
    filter: String,
    show: Show,
    sort: SortKey,
    descending: bool,
    selected: Option<i64>,
    /// `Some((done, total))` while a scan runs.
    scanning: Option<(usize, usize)>,
    result: Option<Result<ScanResult, String>>,
    action_error: Option<String>,
}

impl ReposPage {
    pub fn new() -> Self {
        Self {
            view: View::Clones,
            filter: String::new(),
            show: Show::All,
            sort: SortKey::Name,
            descending: false,
            selected: None,
            scanning: None,
            result: None,
            action_error: None,
        }
    }

    pub fn scan_started(&mut self) {
        self.scanning = Some((0, 0));
    }

    pub fn scan_progress(&mut self, done: usize, total: usize) {
        self.scanning = Some((done, total));
    }

    pub fn scan_finished(&mut self, result: Result<ScanResult, String>) {
        self.scanning = None;
        if let (Ok(scan), Some(id)) = (&result, self.selected)
            && !scan.clones.iter().any(|c| c.id == id)
        {
            self.selected = None;
        }
        self.result = Some(result);
    }

    pub fn action_failed(&mut self, message: String) {
        self.action_error = Some(message);
    }

    /// The clones from the latest successful scan.
    pub fn clones(&self) -> Option<&[CloneInfo]> {
        match &self.result {
            Some(Ok(scan)) => Some(&scan.clones),
            _ => None,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, has_clone_root: bool) -> Option<ReposAction> {
        let mut action = None;
        // Taken out so the views can borrow it while changing the page's own state.
        let result = self.result.take();

        self.toolbar(ui, result.as_ref(), &mut action);
        if let Some(error) = &self.action_error {
            let mut dismiss = false;
            ui.horizontal(|ui| {
                ui.colored_label(ui.visuals().error_fg_color, error);
                dismiss = ui.small_button("Dismiss").clicked();
            });
            if dismiss {
                self.action_error = None;
            }
        }
        ui.separator();

        match &result {
            None if self.scanning.is_some() => {
                ui.weak("Scanning the clone root…");
            }
            None if !has_clone_root => no_clone_root(ui, &mut action),
            None => {
                ui.weak("Not scanned yet.");
            }
            Some(Err(error)) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
                if !has_clone_root {
                    no_clone_root(ui, &mut action);
                }
            }
            Some(Ok(scan)) => match self.view {
                View::Clones => self.clones_view(ui, scan, &mut action),
                View::Problems => self.problems_view(ui, scan, &mut action),
            },
        }

        self.result = result;
        action
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        result: Option<&Result<ScanResult, String>>,
        action: &mut Option<ReposAction>,
    ) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.scanning.is_none(), egui::Button::new("Rescan"))
                .clicked()
            {
                *action = Some(ReposAction::Rescan);
            }
            ui.separator();

            if let Some(Ok(scan)) = result {
                ui.selectable_value(
                    &mut self.view,
                    View::Clones,
                    format!("Clones ({})", scan.clones.len()),
                );
                ui.selectable_value(
                    &mut self.view,
                    View::Problems,
                    format!("Problems ({})", scan.problem_count()),
                );
                ui.separator();
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Filter by name or folder")
                        .desired_width(200.0),
                );
                if self.view == View::Clones {
                    let label = ui.label("Show");
                    egui::ComboBox::from_id_salt("repos_show")
                        .selected_text(self.show.label())
                        .show_ui(ui, |ui| {
                            for show in Show::ALL {
                                ui.selectable_value(&mut self.show, show, show.label());
                            }
                        })
                        .response
                        .labelled_by(label.id);
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((done, total)) = self.scanning {
                    if total > 0 {
                        ui.label(format!("Scanning… {done} / {total}"));
                    } else {
                        ui.label("Scanning…");
                    }
                    ui.spinner();
                } else if let Some(Ok(scan)) = result {
                    ui.weak(format!("Scanned in {}", short_duration(scan.took)))
                        .on_hover_text(format!("Clone root: {}", scan.root.display()));
                }
            });
        });
    }

    fn clones_view(
        &mut self,
        ui: &mut egui::Ui,
        scan: &ScanResult,
        action: &mut Option<ReposAction>,
    ) {
        if let Some(id) = self.selected
            && let Some(clone) = scan.clones.iter().find(|c| c.id == id)
        {
            let mut close = false;
            egui::Panel::right("clone_details")
                .resizable(true)
                .default_size(400.0)
                .min_size(280.0)
                .show(ui, |ui| close = details(ui, clone, action));
            if close {
                self.selected = None;
            }
        }

        let needle = self.filter.trim().to_lowercase();
        let mut rows: Vec<&CloneInfo> = scan
            .clones
            .iter()
            .filter(|c| matches_text(&needle, &c.display_name(), &c.rel_path) && self.show.matches(c))
            .collect();
        sort_rows(&mut rows, self.sort, self.descending);

        if rows.is_empty() {
            ui.add_space(8.0);
            ui.weak(if scan.clones.is_empty() {
                "No clones in the clone root yet."
            } else {
                "Nothing matches the filter."
            });
            return;
        }

        // Selectable labels would swallow clicks meant for selecting the row.
        ui.style_mut().interaction.selectable_labels = false;
        // When the details panel leaves too little room, scroll sideways instead of overlapping it.
        egui::ScrollArea::horizontal()
            .auto_shrink(false)
            .show(ui, |ui| self.clones_table(ui, &rows));
    }

    fn clones_table(&mut self, ui: &mut egui::Ui, rows: &[&CloneInfo]) {
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(220.0).at_least(110.0).clip(true).resizable(true))
            .column(Column::initial(150.0).at_least(70.0).clip(true).resizable(true))
            .column(Column::initial(120.0).at_least(60.0).clip(true).resizable(true))
            .column(Column::initial(70.0).at_least(60.0))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::initial(50.0).at_least(45.0))
            .column(Column::remainder().at_least(110.0).clip(true))
            .header(24.0, |mut header| {
                header.col(|ui| self.sort_button(ui, "Repository", SortKey::Name));
                header.col(|ui| self.sort_button(ui, "Folder", SortKey::Folder));
                header.col(|ui| self.sort_button(ui, "Branch", SortKey::Branch));
                header.col(|ui| self.sort_button(ui, "Changes", SortKey::Changes));
                header.col(|ui| self.sort_button(ui, "Sync", SortKey::Sync));
                header.col(|ui| {
                    ui.strong("Stash");
                });
                header.col(|ui| {
                    ui.strong("Problems");
                });
            })
            .body(|body| {
                body.rows(row_height, rows.len(), |mut row| {
                    let clone = rows[row.index()];
                    row.set_selected(self.selected == Some(clone.id));
                    row.col(|ui| name_cell(ui, clone));
                    row.col(|ui| {
                        ui.weak(&clone.rel_path);
                    });
                    row.col(|ui| branch_cell(ui, clone));
                    row.col(|ui| changes_cell(ui, clone));
                    row.col(|ui| sync_cell(ui, clone));
                    row.col(|ui| stash_cell(ui, clone));
                    row.col(|ui| problems_cell(ui, clone));
                    if row.response().clicked() {
                        self.selected = if self.selected == Some(clone.id) {
                            None
                        } else {
                            Some(clone.id)
                        };
                    }
                });
            });
    }

    fn sort_button(&mut self, ui: &mut egui::Ui, label: &str, key: SortKey) {
        let arrow = match (self.sort == key, self.descending) {
            (true, false) => " ⏶",
            (true, true) => " ⏷",
            (false, _) => "",
        };
        let button = egui::Button::new(egui::RichText::new(format!("{label}{arrow}")).strong())
            .frame(false);
        if ui.add(button).on_hover_text("Sort").clicked() {
            if self.sort == key {
                self.descending = !self.descending;
            } else {
                self.sort = key;
                self.descending = false;
            }
        }
    }

    fn problems_view(
        &mut self,
        ui: &mut egui::Ui,
        scan: &ScanResult,
        action: &mut Option<ReposAction>,
    ) {
        if scan.problem_count() == 0 {
            ui.add_space(8.0);
            ui.weak("No problems found.");
            return;
        }
        let needle = self.filter.trim().to_lowercase();

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let missing: Vec<_> = scan
                    .missing
                    .iter()
                    .filter(|m| matches_text(&needle, m.name.as_deref().unwrap_or(""), &m.path))
                    .collect();
                problem_section(ui, "Missing folders", MISSING_HELP, missing.len(), |ui| {
                    for gone in missing {
                        ui.horizontal(|ui| {
                            ui.label(gone.name.as_deref().unwrap_or(&gone.path));
                            ui.weak(&gone.path);
                            ui.weak(format!("· missing since {} UTC", gone.since));
                            if ui
                                .small_button("Forget")
                                .on_hover_text("Removes it from the database, along with its tags")
                                .clicked()
                            {
                                *action = Some(ReposAction::ForgetClone(gone.id));
                            }
                        });
                    }
                });

                for kind in ProblemKind::ALL {
                    let entries: Vec<(&CloneInfo, &Problem)> = scan
                        .clones
                        .iter()
                        .filter(|c| matches_text(&needle, &c.display_name(), &c.rel_path))
                        .flat_map(|c| {
                            c.problems
                                .iter()
                                .filter(|p| ProblemKind::of(p) == kind)
                                .map(move |p| (c, p))
                        })
                        .collect();
                    problem_section(ui, kind.title(), kind.help(), entries.len(), |ui| {
                        for (clone, problem) in entries {
                            ui.horizontal(|ui| {
                                if ui
                                    .link(clone.display_name())
                                    .on_hover_text("Show in the clones list")
                                    .clicked()
                                {
                                    self.selected = Some(clone.id);
                                    self.view = View::Clones;
                                }
                                ui.weak(&clone.rel_path);
                                problem_detail(ui, clone, problem, action);
                            });
                        }
                    });
                }

                let strays: Vec<&String> = scan
                    .strays
                    .iter()
                    .filter(|s| matches_text(&needle, "", s))
                    .collect();
                problem_section(ui, "Not git repos", STRAYS_HELP, strays.len(), |ui| {
                    for stray in strays {
                        ui.label(stray);
                    }
                });
            });
    }
}

fn no_clone_root(ui: &mut egui::Ui, action: &mut Option<ReposAction>) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.3);
        ui.heading("No clone root yet");
        ui.weak("Pick the folder that holds (or will hold) your clones.");
        ui.add_space(6.0);
        if ui.button("Open Settings").clicked() {
            *action = Some(ReposAction::OpenSettings);
        }
    });
}

fn matches_text(needle: &str, name: &str, path: &str) -> bool {
    needle.is_empty()
        || name.to_lowercase().contains(needle)
        || path.to_lowercase().contains(needle)
}

fn sort_rows(rows: &mut [&CloneInfo], key: SortKey, descending: bool) {
    let changes = |c: &CloneInfo| c.status.as_ref().map_or(0, Status::total_changes);
    let sync = |c: &CloneInfo| {
        c.status.as_ref().map_or(0, |s| match s.ahead_behind {
            Some((ahead, behind)) => ahead + behind,
            None if s.upstream_gone() => u32::MAX,
            None => 0,
        })
    };
    let branch = |c: &CloneInfo| {
        c.status
            .as_ref()
            .ok()
            .and_then(|s| s.branch.clone())
            .unwrap_or_default()
    };
    rows.sort_by(|a, b| {
        let order = match key {
            SortKey::Name => a
                .display_name()
                .to_lowercase()
                .cmp(&b.display_name().to_lowercase()),
            SortKey::Folder => a.rel_path.cmp(&b.rel_path),
            SortKey::Branch => branch(a).cmp(&branch(b)),
            SortKey::Changes => changes(a).cmp(&changes(b)),
            SortKey::Sync => sync(a).cmp(&sync(b)),
        };
        let order = order.then_with(|| a.rel_path.cmp(&b.rel_path));
        if descending { order.reverse() } else { order }
    });
}

fn name_cell(ui: &mut egui::Ui, clone: &CloneInfo) {
    ui.label(clone.display_name());
    if let Some(repo) = clone.repo()
        && !repo.is_github()
    {
        ui.weak(&repo.host);
    }
}

fn branch_cell(ui: &mut egui::Ui, clone: &CloneInfo) {
    match &clone.status {
        Err(error) => {
            ui.colored_label(ui.visuals().error_fg_color, "git error")
                .on_hover_text(error);
        }
        Ok(status) => {
            match &status.branch {
                Some(branch) => ui.label(branch),
                None => ui.weak("(detached)"),
            };
            if status.head.is_none() {
                ui.weak("no commits");
            }
        }
    }
}

fn changes_cell(ui: &mut egui::Ui, clone: &CloneInfo) {
    let Ok(status) = &clone.status else { return };
    let total = status.total_changes();
    if total == 0 {
        ui.weak("—");
        return;
    }
    let color = if status.conflicts > 0 {
        ui.visuals().error_fg_color
    } else {
        ui.visuals().warn_fg_color
    };
    ui.colored_label(color, total.to_string())
        .on_hover_text(describe_changes(status));
}

fn sync_cell(ui: &mut egui::Ui, clone: &CloneInfo) {
    let Ok(status) = &clone.status else { return };
    match (status.ahead_behind, &status.upstream) {
        (Some((0, 0)), upstream) => {
            ui.weak("✔").on_hover_text(format!(
                "In sync with {}, as of the last fetch",
                upstream.as_deref().unwrap_or("its upstream")
            ));
        }
        (Some((ahead, behind)), upstream) => {
            ui.colored_label(ui.visuals().warn_fg_color, ahead_behind_text(ahead, behind))
                .on_hover_text(format!(
                    "{ahead} ahead, {behind} behind {}, as of the last fetch",
                    upstream.as_deref().unwrap_or("its upstream")
                ));
        }
        (None, Some(upstream)) => {
            ui.colored_label(ui.visuals().warn_fg_color, "gone")
                .on_hover_text(format!("The upstream branch {upstream} no longer exists"));
        }
        (None, None) => {
            ui.weak("—").on_hover_text("No upstream branch");
        }
    }
}

fn stash_cell(ui: &mut egui::Ui, clone: &CloneInfo) {
    if let Ok(status) = &clone.status
        && status.stashes > 0
    {
        ui.label(status.stashes.to_string())
            .on_hover_text(plural(status.stashes, "stash", "stashes"));
    }
}

fn problems_cell(ui: &mut egui::Ui, clone: &CloneInfo) {
    if clone.problems.is_empty() {
        return;
    }
    let short: Vec<String> = clone.problems.iter().map(short_problem).collect();
    let long: Vec<String> = clone.problems.iter().map(describe_problem).collect();
    let informational = clone
        .problems
        .iter()
        .all(|p| matches!(p, Problem::OtherHost { .. }));
    let response = if informational {
        ui.weak(short.join(", "))
    } else {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("⚠ {}", short.join(", ")),
        )
    };
    response.on_hover_text(long.join("\n"));
}

/// The side panel for the selected clone. Returns true when it should close.
fn details(ui: &mut egui::Ui, clone: &CloneInfo, action: &mut Option<ReposAction>) -> bool {
    let mut close = false;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.heading(clone.display_name());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = ui.button("Close").clicked();
        });
    });
    ui.weak(clone.abs_path.display().to_string());
    ui.separator();

    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.strong("Remotes");
        if clone.remotes.is_empty() {
            ui.weak("None");
        } else {
            let main_index = match clone.main {
                MainRemote::Chosen { index, .. } => Some(index),
                _ => None,
            };
            egui::Grid::new(("remotes", clone.id))
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for (index, remote) in clone.remotes.iter().enumerate() {
                        if Some(index) == main_index {
                            ui.strong(&remote.name).on_hover_text("Main remote");
                        } else {
                            ui.label(&remote.name);
                        }
                        ui.vertical(|ui| {
                            ui.label(&remote.url);
                            match &remote.parsed {
                                Some(repo) => ui.weak(format!("{} · {}", repo.host, repo.full_name())),
                                None => ui.weak("Not a hosted repo URL"),
                            };
                        });
                        ui.end_row();
                    }
                });
            if clone.remotes.len() > 1 {
                ui.add_space(4.0);
                main_remote_picker(ui, clone, action);
            }
        }

        ui.add_space(10.0);
        ui.strong("Status");
        match &clone.status {
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            Ok(status) => {
                egui::Grid::new(("status", clone.id))
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Branch");
                        ui.label(status.branch.as_deref().unwrap_or("(detached HEAD)"));
                        ui.end_row();

                        ui.label("Commit");
                        ui.label(match &status.head {
                            Some(head) => head.chars().take(12).collect(),
                            None => "no commits yet".to_owned(),
                        });
                        ui.end_row();

                        ui.label("Upstream");
                        ui.label(match (&status.upstream, status.ahead_behind) {
                            (None, _) => "none".to_owned(),
                            (Some(up), None) => format!("{up} (gone)"),
                            (Some(up), Some((0, 0))) => format!("{up}, in sync"),
                            (Some(up), Some((a, b))) => format!("{up}, {a} ahead, {b} behind"),
                        });
                        ui.end_row();

                        ui.label("Changes");
                        ui.label(describe_changes(status));
                        ui.end_row();

                        ui.label("Stashes");
                        ui.label(status.stashes.to_string());
                        ui.end_row();
                    });
                ui.weak("Ahead/behind counts are as of the last fetch.");
            }
        }

        ui.add_space(10.0);
        ui.strong("Problems");
        if clone.problems.is_empty() {
            ui.weak("None");
        }
        for problem in &clone.problems {
            ui.label(format!("• {}", describe_problem(problem)));
        }
    });
    close
}

fn main_remote_picker(ui: &mut egui::Ui, clone: &CloneInfo, action: &mut Option<ReposAction>) {
    let (current, picked_by_hand) = match clone.main {
        MainRemote::Chosen {
            index,
            picked_by_hand,
        } => (clone.remotes[index].name.clone(), picked_by_hand),
        _ => ("Choose…".to_owned(), false),
    };
    ui.horizontal(|ui| {
        let label = ui.label("Main remote");
        egui::ComboBox::from_id_salt(("main_remote", clone.id))
            .selected_text(if picked_by_hand {
                format!("{current} (picked)")
            } else {
                current.clone()
            })
            .show_ui(ui, |ui| {
                for remote in &clone.remotes {
                    let selected = matches!(clone.main, MainRemote::Chosen { index, .. }
                        if clone.remotes[index].name == remote.name);
                    if ui
                        .selectable_label(selected, &remote.name)
                        .on_hover_text(&remote.url)
                        .clicked()
                    {
                        *action = Some(ReposAction::SetMainRemote {
                            clone_id: clone.id,
                            remote: Some(remote.name.clone()),
                        });
                    }
                }
                if picked_by_hand
                    && ui
                        .selectable_label(false, "Automatic")
                        .on_hover_text("Default remote name, then origin, then the only remote")
                        .clicked()
                {
                    *action = Some(ReposAction::SetMainRemote {
                        clone_id: clone.id,
                        remote: None,
                    });
                }
            })
            .response
            .labelled_by(label.id);
    });
}

fn problem_section(
    ui: &mut egui::Ui,
    title: &str,
    help: &str,
    count: usize,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    if count == 0 {
        return;
    }
    egui::CollapsingHeader::new(egui::RichText::new(format!("{title} ({count})")).strong())
        .id_salt(title)
        .default_open(true)
        .show(ui, |ui| {
            ui.weak(help);
            ui.add_space(4.0);
            add_contents(ui);
        });
    ui.add_space(6.0);
}

fn problem_detail(
    ui: &mut egui::Ui,
    clone: &CloneInfo,
    problem: &Problem,
    action: &mut Option<ReposAction>,
) {
    match problem {
        Problem::NoMainRemote => main_remote_picker(ui, clone, action),
        Problem::NoRemotes => {}
        Problem::NotHosted { url } => {
            ui.weak(format!("· {url}"));
        }
        Problem::OtherHost { host } => {
            ui.weak(format!("· {host}"));
        }
        Problem::OutOfLayout { expected } => {
            ui.weak(format!("· expected at {expected}"));
        }
        Problem::GitError { message } => {
            ui.colored_label(ui.visuals().error_fg_color, message);
        }
    }
}

fn short_problem(problem: &Problem) -> String {
    match problem {
        Problem::NoRemotes => "no remotes".to_owned(),
        Problem::NoMainRemote => "pick main remote".to_owned(),
        Problem::NotHosted { .. } => "not hosted".to_owned(),
        Problem::OtherHost { host } => format!("on {host}"),
        Problem::OutOfLayout { .. } => "outside layout".to_owned(),
        Problem::GitError { .. } => "git error".to_owned(),
    }
}

fn describe_problem(problem: &Problem) -> String {
    match problem {
        Problem::NoRemotes => "No remotes".to_owned(),
        Problem::NoMainRemote => "Several remotes, and none is the main one".to_owned(),
        Problem::NotHosted { url } => format!("Main remote isn't a hosted repo: {url}"),
        Problem::OtherHost { host } => format!("Main remote is on {host}, not GitHub"),
        Problem::OutOfLayout { expected } => {
            format!("Not where the layout puts it (expected at {expected})")
        }
        Problem::GitError { message } => format!("git couldn't read it: {message}"),
    }
}

fn describe_changes(status: &Status) -> String {
    let mut parts = Vec::new();
    if status.changed > 0 {
        parts.push(format!("{} changed", status.changed));
    }
    if status.untracked > 0 {
        parts.push(format!("{} untracked", status.untracked));
    }
    if status.conflicts > 0 {
        parts.push(format!("{} conflicted", status.conflicts));
    }
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(", ")
    }
}

fn ahead_behind_text(ahead: u32, behind: u32) -> String {
    match (ahead, behind) {
        (a, 0) => format!("⬆{a}"),
        (0, b) => format!("⬇{b}"),
        (a, b) => format!("⬆{a} ⬇{b}"),
    }
}

fn short_duration(duration: std::time::Duration) -> String {
    if duration.as_secs() >= 1 {
        format!("{:.1} s", duration.as_secs_f32())
    } else {
        format!("{} ms", duration.as_millis())
    }
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::db::MissingClone;
    use crate::git::remote_url;
    use crate::git::scan::{Location, Remote, choose_main_remote, find_problems};

    fn remote(name: &str, url: &str) -> Remote {
        Remote {
            name: name.into(),
            url: url.into(),
            parsed: remote_url::parse(url, &[]),
        }
    }

    fn clone(id: i64, rel_path: &str, remotes: Vec<Remote>, status: Status) -> CloneInfo {
        let main = choose_main_remote(&remotes, "me", None);
        let letter = rel_path.chars().next().unwrap();
        CloneInfo {
            id,
            rel_path: rel_path.into(),
            abs_path: PathBuf::from("/home/someone/code").join(rel_path),
            problems: find_problems(rel_path, Location::Letter(letter), &remotes, main),
            remotes,
            main,
            status: Ok(status),
        }
    }

    fn sample_scan() -> ScanResult {
        let clean = Status {
            branch: Some("main".into()),
            head: Some("4f2a9c01de77".into()),
            upstream: Some("me/main".into()),
            ahead_behind: Some((0, 0)),
            ..Default::default()
        };
        ScanResult {
            root: PathBuf::from("/home/someone/code"),
            clones: vec![
                clone(
                    1,
                    "r/react",
                    vec![remote("me", "git@github.com:facebook/react.git")],
                    clean.clone(),
                ),
                clone(
                    2,
                    "r/react~2",
                    vec![
                        remote("me", "git@github.com:someone/react.git"),
                        remote("upstream", "git@github.com:facebook/react.git"),
                    ],
                    Status {
                        ahead_behind: Some((2, 1)),
                        changed: 3,
                        untracked: 1,
                        stashes: 2,
                        ..clean.clone()
                    },
                ),
                clone(
                    3,
                    "t/thing",
                    vec![
                        remote("alice", "git@github.com:alice/thing.git"),
                        remote("bob", "git@github.com:bob/thing.git"),
                    ],
                    clean.clone(),
                ),
                clone(
                    4,
                    "n/notes",
                    vec![],
                    Status {
                        branch: Some("main".into()),
                        untracked: 1,
                        ..Default::default()
                    },
                ),
                clone(
                    5,
                    "x/serde",
                    vec![remote("me", "git@github.com:serde-rs/serde.git")],
                    clean,
                ),
            ],
            strays: vec!["p/photos".into()],
            missing: vec![MissingClone {
                id: 9,
                path: "d/doomed".into(),
                name: Some("someone/doomed".into()),
                since: "2026-11-21 18:00:00".into(),
            }],
            took: Duration::from_millis(40),
        }
    }

    type State = (ReposPage, Vec<ReposAction>);

    fn harness<'a>(builder: egui_kittest::HarnessBuilder<State>) -> Harness<'a, State> {
        let mut page = ReposPage::new();
        page.scan_finished(Ok(sample_scan()));
        builder.with_size([1280.0, 560.0]).build_ui_state(
            |ui, (page, actions): &mut State| {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    if let Some(action) = page.show(ui, true) {
                        actions.push(action);
                    }
                });
            },
            (page, Vec::new()),
        )
    }

    #[test]
    fn clicking_a_row_opens_and_closes_details() {
        let mut h = harness(Harness::builder());
        h.run();
        assert!(h.query_by_label("Remotes").is_none());
        h.get_by_label("facebook/react").click();
        h.run();
        assert!(h.query_by_label("Remotes").is_some());
        assert_eq!(h.state().0.selected, Some(1));
        h.get_by_label("Close").click();
        h.run();
        assert_eq!(h.state().0.selected, None);
    }

    #[test]
    fn filter_narrows_the_list() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.filter = "serde".into();
        h.run();
        assert!(h.query_by_label("serde-rs/serde").is_some());
        assert!(h.query_by_label("facebook/react").is_none());
    }

    #[test]
    fn problems_view_groups_and_acts() {
        let mut h = harness(Harness::builder());
        h.run();
        h.get_by_label("Problems (5)").click();
        h.run();
        for title in [
            "Missing folders (1)",
            "Main remote not chosen (1)",
            "Outside the layout (1)",
            "No remotes (1)",
            "Not git repos (1)",
        ] {
            assert!(h.query_by_label(title).is_some(), "missing section {title}");
        }

        h.get_by_label("Forget").click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Main remote")
            .click();
        h.run();
        h.get_by_label("bob").click();
        h.run();
        assert_eq!(
            h.state().1,
            vec![
                ReposAction::ForgetClone(9),
                ReposAction::SetMainRemote {
                    clone_id: 3,
                    remote: Some("bob".into())
                },
            ]
        );
    }

    /// Renders the views to PNGs for a visual check. Only runs when
    /// `DEPOSITORY_UI_SHOTS` names a folder: `DEPOSITORY_UI_SHOTS=/tmp/shots cargo test ui_shots`.
    #[test]
    fn ui_shots() {
        let Some(dir) = std::env::var_os("DEPOSITORY_UI_SHOTS").map(PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = harness(Harness::builder().wgpu());
        h.run();
        h.get_by_label("someone/react").click();
        h.run();
        h.render().unwrap().save(dir.join("clones.png")).unwrap();
        h.get_by_label("Problems (5)").click();
        h.run();
        h.render().unwrap().save(dir.join("problems.png")).unwrap();
    }

    #[test]
    fn ahead_behind_labels() {
        assert_eq!(ahead_behind_text(2, 0), "⬆2");
        assert_eq!(ahead_behind_text(0, 5), "⬇5");
        assert_eq!(ahead_behind_text(2, 5), "⬆2 ⬇5");
    }

    #[test]
    fn change_descriptions() {
        let status = Status {
            changed: 3,
            untracked: 1,
            ..Default::default()
        };
        assert_eq!(describe_changes(&status), "3 changed, 1 untracked");
        assert_eq!(describe_changes(&Status::default()), "none");
    }
}
