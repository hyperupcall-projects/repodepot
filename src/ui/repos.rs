//! The Repositories tab: the Remote List, Ignored and Forks lists from GitHub, local clones
//! with their status, and a Problems view.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::backend::{ImportMove, LauncherInfo, SyncReport};
use crate::db::{Catalog, ForkDecision};
use crate::git::import::ImportPreview;
use crate::git::scan::{CloneInfo, MainRemote, Problem, ScanResult};
use crate::git::status::Status;
use crate::tags::query::{self, Query};
use crate::tags::{Rule, TagIndex};
use crate::ui::import::ImportPage;
use crate::ui::remote_list::{self, ListContext, ListKind, ListState};
use crate::ui::tags::{TagsAction, tag_summary};
use crate::ui::{sort_header, thousands};

#[derive(Debug, PartialEq, Eq)]
pub enum ReposAction {
    Rescan,
    SetMainRemote {
        clone_id: i64,
        remote: Option<String>,
    },
    ForgetClone(i64),
    OpenSettings,
    OpenRunner,
    Sync,
    Clone(Vec<i64>),
    SetIgnored {
        repo_ids: Vec<i64>,
        ignored: bool,
    },
    SetForkDecision {
        fork_id: i64,
        parent_id: i64,
        decision: ForkDecision,
    },
    AddManual(String),
    RemoveManualOwner(String),
    SetRemote {
        path: PathBuf,
        name: String,
        url: String,
        add: bool,
    },
    PickImportFolder,
    /// Plan an import: search `folder`, or take `paths` as the repos to move.
    PreviewImport {
        folder: Option<PathBuf>,
        paths: Vec<PathBuf>,
    },
    Import(Vec<ImportMove>),
    /// Tag changes made from a clone's details.
    Tags(TagsAction),
    /// Open one clone in an app.
    Open {
        launcher: String,
        clone_id: i64,
    },
}

pub struct ReposContext<'a> {
    pub has_clone_root: bool,
    /// Remote names from the settings.
    pub default_remote: &'a str,
    pub upstream_name: &'a str,
    /// Tags of clones, by clone ID.
    pub tags: &'a TagIndex,
    /// Tags of repos that aren't cloned (Remote List and Ignored), by repo ID.
    pub repo_tags: &'a TagIndex,
    pub rules: &'a [Rule],
    pub launchers: &'a [LauncherInfo],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    RemoteList,
    LocalClones,
    Ignored,
    Forks,
    Problems,
    Import,
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
    Renamed,
    MissingUpstream,
    NoRemotes,
    NotHosted,
    OtherHost,
    OutOfLayout,
    NotOnGitHub,
    NotListed,
    GitError,
}

impl ProblemKind {
    const ALL: [ProblemKind; 10] = [
        ProblemKind::NoMainRemote,
        ProblemKind::Renamed,
        ProblemKind::MissingUpstream,
        ProblemKind::OutOfLayout,
        ProblemKind::NotOnGitHub,
        ProblemKind::NotListed,
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
            Problem::NotListed => ProblemKind::NotListed,
            Problem::NotOnGitHub => ProblemKind::NotOnGitHub,
            Problem::Renamed { .. } => ProblemKind::Renamed,
            Problem::MissingUpstream { .. } => ProblemKind::MissingUpstream,
        }
    }

    fn title(self) -> &'static str {
        match self {
            ProblemKind::NoMainRemote => "Main remote not chosen",
            ProblemKind::Renamed => "Renamed or moved on GitHub",
            ProblemKind::MissingUpstream => "Linked forks without an upstream remote",
            ProblemKind::NoRemotes => "No remotes",
            ProblemKind::NotHosted => "Main remote isn't a hosted repo",
            ProblemKind::OtherHost => "On another host",
            ProblemKind::OutOfLayout => "Outside the layout",
            ProblemKind::NotOnGitHub => "Gone from GitHub",
            ProblemKind::NotListed => "Not in any source",
            ProblemKind::GitError => "git couldn't read these",
        }
    }

    fn help(self) -> &'static str {
        match self {
            ProblemKind::Renamed => {
                "Their main remote still uses the old name. GitHub redirects it for now; point \
                 the remote at the new name to be safe, then move the folder from \
                 Outside the layout."
            }
            ProblemKind::MissingUpstream => {
                "You marked these forks as linked, but their clones have no remote for the parent."
            }
            ProblemKind::NotOnGitHub => {
                "GitHub says these don't exist, or the token can't see them (deleted, made \
                 private, or in an org that needs SSO approval for the token)."
            }
            ProblemKind::NotListed => {
                "Clones of GitHub repos that none of your sources list. Add one by hand to keep \
                 it in your lists, or leave it be."
            }
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
                "Not at <letter>/<repo name>. Moving them goes through Import, which shows where each one will go first."
            }
            ProblemKind::GitError => "git failed when reading these folders.",
        }
    }
}

const MISSING_HELP: &str = "An earlier scan found these, but their folder is gone. A folder moved \
    within the clone root is matched back to its old entry by its main remote. Forget removes the \
    entry and its tags.";
const STRAYS_HELP: &str = "Folders under the clone root that aren't git repos. Only \
    <letter>/<name> is scanned, so repos deeper inside these aren't seen; Import can find them.";

pub struct ReposPage {
    view: View,
    filter: String,
    /// A tag query for the Remote List, Ignored and Local Clones, like `work AND NOT old`.
    tag_filter: String,
    show: Show,
    sort: SortKey,
    descending: bool,
    selected: Option<i64>,
    /// `Some((done, total))` while a scan runs.
    scanning: Option<(usize, usize)>,
    result: Option<Result<ScanResult, String>>,
    action_error: Option<String>,
    notice: Option<String>,
    /// `Some(latest progress message)` while a sync runs.
    syncing: Option<String>,
    sync_report: Option<SyncReport>,
    catalog: Option<Catalog>,
    remote: ListState,
    ignored: ListState,
    /// Repo IDs with a clone run in progress.
    cloning: HashSet<i64>,
    add_text: String,
    only_undecided: bool,
    upstream_name: String,
    import: ImportPage,
    /// The "add tag" field in the details panel.
    new_tag: String,
}

impl ReposPage {
    pub fn new() -> Self {
        Self {
            view: View::LocalClones,
            filter: String::new(),
            tag_filter: String::new(),
            show: Show::All,
            sort: SortKey::Name,
            descending: false,
            selected: None,
            scanning: None,
            result: None,
            action_error: None,
            notice: None,
            syncing: None,
            sync_report: None,
            catalog: None,
            remote: ListState::new(),
            ignored: ListState::new(),
            cloning: HashSet::new(),
            add_text: String::new(),
            only_undecided: false,
            upstream_name: "upstream".to_owned(),
            import: ImportPage::new(),
            new_tag: String::new(),
        }
    }

    pub fn sync_started(&mut self) {
        self.syncing = Some("Starting…".to_owned());
    }

    pub fn sync_progress(&mut self, message: String) {
        self.syncing = Some(message);
    }

    pub fn sync_finished(&mut self, result: Result<SyncReport, String>) {
        self.syncing = None;
        match result {
            Ok(report) => self.sync_report = Some(report),
            Err(e) => self.action_error = Some(format!("Sync failed: {e}")),
        }
    }

    pub fn set_catalog(&mut self, catalog: Catalog) {
        self.remote.retain(&catalog.inbox);
        self.ignored.retain(&catalog.ignored);
        self.catalog = Some(catalog);
    }

    pub fn clones_finished(&mut self, repo_ids: &[i64]) {
        for id in repo_ids {
            self.cloning.remove(id);
        }
    }

    pub fn import_searching(&mut self) {
        self.import.searching();
    }

    pub fn import_preview(&mut self, preview: Result<ImportPreview, String>) {
        self.import.set_preview(preview);
    }

    /// A folder was picked for Import: show it there (the app starts the search).
    pub fn import_folder_picked(&mut self, path: &std::path::Path) {
        self.import.set_folder(path);
        self.view = View::Import;
    }

    pub fn notice(&mut self, message: String) {
        self.notice = Some(message);
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

    /// The Remote List, Ignored and Forks lists from the last sync.
    pub fn catalog(&self) -> Option<&Catalog> {
        self.catalog.as_ref()
    }

    /// The clones from the latest successful scan.
    pub fn clones(&self) -> Option<&[CloneInfo]> {
        match &self.result {
            Some(Ok(scan)) => Some(&scan.clones),
            _ => None,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: ReposContext<'_>) -> Option<ReposAction> {
        let mut action = None;
        self.upstream_name = cx.upstream_name.to_owned();
        // Taken out so the views can borrow them while changing the page's own state.
        let result = self.result.take();
        let catalog = self.catalog.take();
        let tag_query = match self.tag_filter.trim() {
            "" => None,
            text => Some(query::parse(text)),
        };
        // A query that's still being typed (or is wrong) doesn't filter; the toolbar says why.
        let valid_query = tag_query.as_ref().and_then(|q| q.as_ref().ok());

        self.toolbar(
            ui,
            result.as_ref(),
            catalog.as_ref(),
            (&cx, tag_query.as_ref()),
            &mut action,
        );
        self.messages(ui, &mut action);
        ui.separator();

        match self.view {
            View::Import => self.import.show(ui, cx.default_remote, &mut action),
            View::RemoteList | View::Ignored | View::Forks => {
                self.github_view(ui, catalog.as_ref(), cx.repo_tags, valid_query, &mut action);
            }
            View::LocalClones | View::Problems => match &result {
                None if self.scanning.is_some() => {
                    ui.weak("Scanning the clone root…");
                }
                None if !cx.has_clone_root => no_clone_root(ui, &mut action),
                None => {
                    ui.weak("Not scanned yet.");
                }
                Some(Err(error)) => {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    if !cx.has_clone_root {
                        no_clone_root(ui, &mut action);
                    }
                }
                Some(Ok(scan)) => match self.view {
                    View::Problems => self.problems_view(ui, scan, &mut action),
                    _ => self.clones_view(
                        ui,
                        scan,
                        (cx.tags, cx.rules, valid_query),
                        cx.launchers,
                        &mut action,
                    ),
                },
            },
        }

        self.result = result;
        self.catalog = catalog;
        match &action {
            Some(ReposAction::PreviewImport { folder, .. }) => {
                self.view = View::Import;
                if let Some(folder) = folder {
                    self.import.set_folder(folder);
                }
            }
            Some(ReposAction::Import(moves)) => {
                self.import.moves_started();
                self.notice = Some(format!(
                    "Moving {} into place. Progress is in the Runner tab.",
                    plural(moves.len() as u32, "repo", "repos")
                ));
            }
            _ => {}
        }
        if let Some(ReposAction::Clone(ids)) = &action {
            self.cloning.extend(ids.iter().copied());
            self.notice = Some(format!(
                "Cloning {}. Progress is in the Runner tab.",
                plural(ids.len() as u32, "repo", "repos")
            ));
        }
        action
    }

    /// Errors, notices and the last sync's summary.
    fn messages(&mut self, ui: &mut egui::Ui, action: &mut Option<ReposAction>) {
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
        if let Some(notice) = &self.notice {
            let mut dismiss = false;
            ui.horizontal(|ui| {
                ui.label(notice);
                if notice.contains("Runner tab") && ui.small_button("Open Runner").clicked() {
                    *action = Some(ReposAction::OpenRunner);
                }
                dismiss = ui.small_button("Dismiss").clicked();
            });
            if dismiss {
                self.notice = None;
            }
        }
        if let Some(report) = &self.sync_report {
            let mut dismiss = false;
            ui.horizontal(|ui| {
                let counts: Vec<String> = report
                    .counts
                    .iter()
                    .map(|(source, count)| {
                        format!("{}: {}", source.label(), thousands(*count as u64))
                    })
                    .collect();
                ui.label(format!(
                    "Synced in {}: {} repos listed",
                    short_duration(report.took),
                    thousands(report.relevant as u64)
                ))
                .on_hover_text(counts.join("\n"));
                if report.new_forks > 0 {
                    ui.weak(format!(
                        "· {} new to decide on in Forks",
                        plural(report.new_forks as u32, "fork", "forks")
                    ));
                }
                if !report.warnings.is_empty() {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!(
                            "· ⚠ {}",
                            plural(report.warnings.len() as u32, "warning", "warnings")
                        ),
                    )
                    .on_hover_text(report.warnings.join("\n"));
                }
                dismiss = ui.small_button("Dismiss").clicked();
            });
            if dismiss {
                self.sync_report = None;
            }
        }
    }

    fn github_view(
        &mut self,
        ui: &mut egui::Ui,
        catalog: Option<&Catalog>,
        tags: &TagIndex,
        tag_query: Option<&Query>,
        action: &mut Option<ReposAction>,
    ) {
        let Some(catalog) = catalog else {
            ui.weak("Loading…");
            return;
        };
        if catalog.last_sync.is_none() && catalog.relevant == 0 {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.heading("Not synced yet");
                ui.weak("Sync with GitHub to list your repos here.");
                ui.add_space(6.0);
                if ui
                    .add_enabled(
                        self.syncing.is_none(),
                        egui::Button::new("Sync with GitHub"),
                    )
                    .clicked()
                {
                    *action = Some(ReposAction::Sync);
                }
            });
            return;
        }
        let list = ListContext {
            filter: &self.filter,
            tag_query,
            tags,
            cloning: &self.cloning,
        };
        match self.view {
            View::RemoteList => {
                remote_list::add_by_hand(ui, &mut self.add_text, &catalog.manual_owners, action);
                ui.add_space(4.0);
                remote_list::repo_list(
                    ui,
                    ListKind::Remote,
                    &catalog.inbox,
                    &mut self.remote,
                    &list,
                    action,
                );
            }
            View::Ignored => remote_list::repo_list(
                ui,
                ListKind::Ignored,
                &catalog.ignored,
                &mut self.ignored,
                &list,
                action,
            ),
            _ => {
                remote_list::forks_view(ui, catalog, &self.filter, &mut self.only_undecided, action)
            }
        }
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        result: Option<&Result<ScanResult, String>>,
        catalog: Option<&Catalog>,
        (cx, tag_query): (&ReposContext<'_>, Option<&Result<Query, String>>),
        action: &mut Option<ReposAction>,
    ) {
        ui.horizontal(|ui| {
            let count = |n: usize| thousands(n as u64);
            let remote = match catalog {
                Some(catalog) => format!("Remote List ({})", count(catalog.inbox.len())),
                None => "Remote List".to_owned(),
            };
            ui.selectable_value(&mut self.view, View::RemoteList, remote);
            let clones = match result {
                Some(Ok(scan)) => format!("Local Clones ({})", count(scan.clones.len())),
                _ => "Local Clones".to_owned(),
            };
            ui.selectable_value(&mut self.view, View::LocalClones, clones);
            if let Some(catalog) = catalog {
                ui.selectable_value(
                    &mut self.view,
                    View::Ignored,
                    format!("Ignored ({})", count(catalog.ignored.len())),
                );
                let undecided = catalog
                    .forks
                    .iter()
                    .flat_map(|f| &f.forks)
                    .filter(|f| f.decision == ForkDecision::Undecided)
                    .count();
                let forks = if undecided > 0 {
                    format!("Forks ({} undecided)", count(undecided))
                } else {
                    "Forks".to_owned()
                };
                ui.selectable_value(&mut self.view, View::Forks, forks);
            }
            let problems = match result {
                Some(Ok(scan)) => format!("Problems ({})", count(scan.problem_count())),
                _ => "Problems".to_owned(),
            };
            ui.selectable_value(&mut self.view, View::Problems, problems);
            ui.selectable_value(&mut self.view, View::Import, "Import");

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(self.scanning.is_none(), egui::Button::new("Rescan"))
                    .on_hover_text("Look at the clone root again")
                    .clicked()
                {
                    *action = Some(ReposAction::Rescan);
                }
                if ui
                    .add_enabled(
                        self.syncing.is_none(),
                        egui::Button::new("Sync with GitHub"),
                    )
                    .on_hover_text("Fetch your repos from every source enabled in Settings")
                    .clicked()
                {
                    *action = Some(ReposAction::Sync);
                }
            });
        });

        ui.horizontal(|ui| {
            let hint = match self.view {
                View::LocalClones | View::Problems => "Filter by name or folder",
                _ => "Filter by name, description, language, topic or source",
            };
            if self.view != View::Import {
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text(hint)
                        .desired_width(300.0),
                );
            }
            if matches!(
                self.view,
                View::RemoteList | View::Ignored | View::LocalClones
            ) {
                self.tag_filter_field(ui, cx, tag_query);
            }
            {
                if self.view == View::LocalClones {
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
                ui.separator();
                if let Some(message) = &self.syncing {
                    ui.label(format!("Syncing: {message}"));
                    ui.spinner();
                } else if let Some(catalog) = catalog {
                    match &catalog.last_sync {
                        Some(when) => ui.weak(format!("Synced {when} UTC")),
                        None => ui.weak("Never synced"),
                    };
                }
            });
        });
    }

    /// The tag query field, with a menu of known tags.
    fn tag_filter_field(
        &mut self,
        ui: &mut egui::Ui,
        cx: &ReposContext<'_>,
        parsed: Option<&Result<Query, String>>,
    ) {
        ui.separator();
        let label = ui.label("Tags");
        ui.add(
            egui::TextEdit::singleline(&mut self.tag_filter)
                .hint_text("work AND NOT old, or untagged")
                .desired_width(200.0),
        )
        .labelled_by(label.id)
        .on_hover_text(
            "Tag names, combined with AND, OR, NOT and parentheses. \
             untagged matches repos with no tags at all.",
        );
        let known: BTreeSet<&str> = cx
            .tags
            .counts
            .keys()
            .chain(cx.repo_tags.counts.keys())
            .map(String::as_str)
            .collect();
        ui.menu_button("⏷", |ui| {
            if ui
                .button("untagged")
                .on_hover_text("Repos with no tags at all")
                .clicked()
            {
                self.tag_filter = "untagged".to_owned();
                ui.close();
            }
            ui.separator();
            if known.is_empty() {
                ui.weak("No tags yet");
            }
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for tag in &known {
                        ui.horizontal(|ui| {
                            if ui
                                .button(*tag)
                                .on_hover_text(format!("Has {tag}"))
                                .clicked()
                            {
                                self.tag_filter = (*tag).to_owned();
                                ui.close();
                            }
                            if ui
                                .small_button("not")
                                .on_hover_text(format!("Doesn't have {tag}"))
                                .clicked()
                            {
                                self.tag_filter = format!("NOT {tag}");
                                ui.close();
                            }
                        });
                    }
                });
        })
        .response
        .on_hover_text("Pick a tag");
        if let Some(Err(error)) = parsed {
            ui.colored_label(ui.visuals().warn_fg_color, error);
        }
    }

    fn clones_view(
        &mut self,
        ui: &mut egui::Ui,
        scan: &ScanResult,
        (tags, rules, tag_query): (&TagIndex, &[Rule], Option<&Query>),
        launchers: &[LauncherInfo],
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
                .show(ui, |ui| {
                    close = details(ui, clone, launchers, action);
                    ui.add_space(10.0);
                    tags_section(ui, clone.id, tags, rules, &mut self.new_tag, action);
                });
            if close {
                self.selected = None;
            }
        }

        let needle = self.filter.trim().to_lowercase();
        let mut rows: Vec<&CloneInfo> = scan
            .clones
            .iter()
            .filter(|c| {
                matches_text(&needle, &c.display_name(), &c.rel_path)
                    && self.show.matches(c)
                    && tag_query.is_none_or(|q| q.matches(&tags.tags_of(c.id)))
            })
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
            .show(ui, |ui| self.clones_table(ui, &rows, tags));
    }

    fn clones_table(&mut self, ui: &mut egui::Ui, rows: &[&CloneInfo], tags: &TagIndex) {
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(
                Column::initial(220.0)
                    .at_least(110.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(
                Column::initial(150.0)
                    .at_least(70.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(
                Column::initial(120.0)
                    .at_least(60.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(Column::initial(70.0).at_least(60.0))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::initial(50.0).at_least(45.0))
            .column(
                Column::initial(140.0)
                    .at_least(60.0)
                    .clip(true)
                    .resizable(true),
            )
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
                    ui.strong("Tags");
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
                    row.col(|ui| {
                        if let Some(tagging) = tags.by_clone.get(&clone.id) {
                            tag_summary(ui, tagging);
                        }
                    });
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
        sort_header(ui, label, key, &mut self.sort, &mut self.descending, false);
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
                        if kind == ProblemKind::OutOfLayout
                            && entries.len() > 1
                            && ui
                                .button(format!("Move all {} into place…", entries.len()))
                                .clicked()
                        {
                            *action = Some(ReposAction::PreviewImport {
                                folder: None,
                                paths: entries.iter().map(|(c, _)| c.abs_path.clone()).collect(),
                            });
                        }
                        for (clone, problem) in entries {
                            ui.horizontal(|ui| {
                                if ui
                                    .link(clone.display_name())
                                    .on_hover_text("Show in Local Clones")
                                    .clicked()
                                {
                                    self.selected = Some(clone.id);
                                    self.view = View::LocalClones;
                                }
                                ui.weak(&clone.rel_path);
                                problem_detail(ui, clone, problem, &self.upstream_name, action);
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
                        ui.horizontal(|ui| {
                            ui.label(stray);
                            if ui
                                .small_button("Look for repos inside")
                                .on_hover_text("Open it in Import")
                                .clicked()
                            {
                                *action = Some(ReposAction::PreviewImport {
                                    folder: Some(scan.root.join(stray)),
                                    paths: Vec::new(),
                                });
                            }
                        });
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
        ui.label(status.stashes.to_string()).on_hover_text(plural(
            status.stashes,
            "stash",
            "stashes",
        ));
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
        .all(|p| matches!(p, Problem::OtherHost { .. } | Problem::NotListed));
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
/// Tags of the selected clone: manual ones can be removed, automatic ones excepted.
fn tags_section(
    ui: &mut egui::Ui,
    clone_id: i64,
    index: &TagIndex,
    rules: &[Rule],
    new_tag: &mut String,
    action: &mut Option<ReposAction>,
) {
    ui.strong("Tags");
    let tagging = index.by_clone.get(&clone_id).cloned().unwrap_or_default();
    if tagging.manual.is_empty() && tagging.auto.is_empty() {
        ui.weak("None yet");
    }
    ui.horizontal_wrapped(|ui| {
        for tag in &tagging.manual {
            if ui
                .add(egui::Button::new(format!("{tag} ×")).small())
                .on_hover_text("Manual tag. Click to remove it.")
                .clicked()
            {
                *action = Some(ReposAction::Tags(TagsAction::RemoveTag {
                    clone_ids: vec![clone_id],
                    repo_ids: Vec::new(),
                    tag: tag.clone(),
                }));
            }
        }
    });
    for (tag, rule_ids) in &tagging.auto {
        ui.horizontal(|ui| {
            let from: Vec<String> = rule_ids
                .iter()
                .filter_map(|id| rules.iter().find(|r| r.id == *id))
                .map(Rule::describe)
                .collect();
            ui.label(egui::RichText::new(tag).italics())
                .on_hover_text(format!("Added by a rule:\n{}", from.join("\n")));
            if tagging.manual.contains(tag) {
                ui.weak("(also manual)");
            }
            if ui
                .small_button("Add exception")
                .on_hover_text("Stop the rule that adds this tag from applying to this clone")
                .clicked()
            {
                // One action per frame: except the first rule; any others show up next.
                if let Some(rule_id) = rule_ids.first() {
                    *action = Some(ReposAction::Tags(TagsAction::SetRuleException {
                        rule_id: *rule_id,
                        clone_id,
                        excepted: true,
                    }));
                }
            }
        });
    }
    ui.horizontal(|ui| {
        let label = ui.label("Add tag");
        let field = ui
            .add(
                egui::TextEdit::singleline(new_tag)
                    .hint_text("work, lang:rust, …")
                    .desired_width(160.0),
            )
            .labelled_by(label.id);
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let valid = crate::tags::normalize_tag(new_tag);
        let clicked = ui
            .add_enabled(valid.is_ok(), egui::Button::new("Add"))
            .on_disabled_hover_text(valid.as_ref().err().cloned().unwrap_or_default())
            .clicked();
        if (clicked || entered) && valid.is_ok() {
            *action = Some(ReposAction::Tags(TagsAction::AddTag {
                clone_ids: vec![clone_id],
                repo_ids: Vec::new(),
                tag: std::mem::take(new_tag),
            }));
        }
    });
}

fn details(
    ui: &mut egui::Ui,
    clone: &CloneInfo,
    launchers: &[LauncherInfo],
    action: &mut Option<ReposAction>,
) -> bool {
    let mut close = false;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.heading(clone.display_name());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = ui.button("Close").clicked();
            let apps: Vec<&LauncherInfo> = launchers.iter().filter(|l| l.each).collect();
            ui.add_enabled_ui(!apps.is_empty(), |ui| {
                ui.menu_button("Open in", |ui| {
                    for app in apps {
                        if ui.button(&app.name).clicked() {
                            *action = Some(ReposAction::Open {
                                launcher: app.name.clone(),
                                clone_id: clone.id,
                            });
                            ui.close();
                        }
                    }
                });
            });
        });
    });
    ui.weak(clone.abs_path.display().to_string());
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt(("details", clone.id))
        .max_height(ui.available_height() * 0.6)
        .show(ui, |ui| {
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
                                    Some(repo) => {
                                        ui.weak(format!("{} · {}", repo.host, repo.full_name()))
                                    }
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
    upstream_name: &str,
    action: &mut Option<ReposAction>,
) {
    match problem {
        Problem::NotListed => {
            if let Some(repo) = clone.repo()
                && ui
                    .small_button("Add by hand")
                    .on_hover_text("List it from now on, as if a source did")
                    .clicked()
            {
                *action = Some(ReposAction::AddManual(repo.full_name()));
            }
        }
        Problem::NotOnGitHub => {}
        Problem::Renamed { name, url } => {
            ui.weak(format!("· now {name}"));
            if let Some(remote) = clone.main_remote()
                && ui
                    .small_button("Use the new URL")
                    .on_hover_text(format!("git remote set-url {} {url}", remote.name))
                    .clicked()
            {
                *action = Some(ReposAction::SetRemote {
                    path: clone.abs_path.clone(),
                    name: remote.name.clone(),
                    url: url.clone(),
                    add: false,
                });
            }
        }
        Problem::MissingUpstream { name, url } => {
            ui.weak(format!("· parent is {name}"));
            if ui
                .small_button(format!("Add remote \"{upstream_name}\""))
                .on_hover_text(format!("git remote add {upstream_name} {url}"))
                .clicked()
            {
                *action = Some(ReposAction::SetRemote {
                    path: clone.abs_path.clone(),
                    name: upstream_name.to_owned(),
                    url: url.clone(),
                    add: true,
                });
            }
        }
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
            if ui
                .small_button("Move into place…")
                .on_hover_text("Preview the move in Import")
                .clicked()
            {
                *action = Some(ReposAction::PreviewImport {
                    folder: None,
                    paths: vec![clone.abs_path.clone()],
                });
            }
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
        Problem::NotListed => "not listed".to_owned(),
        Problem::NotOnGitHub => "gone from GitHub".to_owned(),
        Problem::Renamed { .. } => "renamed".to_owned(),
        Problem::MissingUpstream { .. } => "no upstream remote".to_owned(),
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
        Problem::NotListed => {
            "None of your sources list this repo (as of the last sync)".to_owned()
        }
        Problem::NotOnGitHub => {
            "GitHub says this repo doesn't exist, or the token can't see it".to_owned()
        }
        Problem::Renamed { name, .. } => format!("Renamed or moved on GitHub to {name}"),
        Problem::MissingUpstream { name, .. } => {
            format!("Linked fork, but there's no remote for its parent {name}")
        }
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
    use crate::db::github::{ForkEntry, ForkFamily};
    use crate::db::{RepoRow, Source};
    use crate::git::import::ImportItem;
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
            repo_id: None,
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

    /// The sample scan plus two problems only a sync can find.
    fn synced_scan() -> ScanResult {
        let mut scan = sample_scan();
        scan.clones[0].problems.push(Problem::Renamed {
            name: "meta/react".into(),
            url: "git@github.com:meta/react.git".into(),
        });
        scan.clones[1].problems.push(Problem::MissingUpstream {
            name: "facebook/react".into(),
            url: "git@github.com:facebook/react.git".into(),
        });
        scan
    }

    fn repo_row(
        id: i64,
        full_name: &str,
        language: &str,
        pushed: &str,
        sources: &[Source],
    ) -> RepoRow {
        let (owner, name) = full_name.split_once('/').unwrap();
        RepoRow {
            id,
            owner: owner.into(),
            name: name.into(),
            description: Some(format!("The {name} project")),
            language: Some(language.into()),
            topics: Vec::new(),
            is_fork: false,
            is_archived: false,
            is_private: false,
            pushed_at: Some(format!("{pushed}T12:00:00Z")),
            stars: Some(1),
            sources: sources.to_vec(),
            fork_of: None,
        }
    }

    fn sample_catalog() -> Catalog {
        let mut website = repo_row(
            12,
            "myorg/website",
            "TypeScript",
            "2025-02-01",
            &[Source::OrgMember, Source::Contributed],
        );
        website.is_archived = true;
        website.stars = None; // not synced since stars were added
        let mut fork = repo_row(13, "me/react", "JavaScript", "2026-09-01", &[Source::Owned]);
        fork.is_fork = true;
        fork.fork_of = Some(("facebook/react".into(), ForkDecision::Undecided));
        fork.stars = Some(0);
        let mut api = repo_row(11, "myorg/api", "Rust", "2026-11-01", &[Source::OrgMember]);
        api.stars = Some(1520);
        Catalog {
            last_sync: Some("2026-11-21 18:00:00".into()),
            relevant: 5,
            inbox: vec![
                repo_row(10, "me/dotfiles", "Shell", "2026-11-20", &[Source::Owned]),
                api,
                website,
                fork,
            ],
            ignored: vec![repo_row(
                14,
                "myorg/old-thing",
                "Perl",
                "2019-01-01",
                &[Source::OrgMember],
            )],
            forks: vec![ForkFamily {
                parent_id: 20,
                parent_name: "facebook/react".into(),
                parent_listed: true,
                parent_cloned: false,
                forks: vec![ForkEntry {
                    fork_id: 13,
                    name: "me/react".into(),
                    decision: ForkDecision::Undecided,
                    cloned: false,
                    suggest_linked: true,
                }],
            }],
            manual_owners: vec!["rust-lang".into()],
        }
    }

    type State = (ReposPage, Vec<ReposAction>);

    fn harness<'a>(builder: egui_kittest::HarnessBuilder<State>) -> Harness<'a, State> {
        let mut page = ReposPage::new();
        page.scan_finished(Ok(synced_scan()));
        page.set_catalog(sample_catalog());
        builder.with_size([1280.0, 560.0]).build_ui_state(
            |ui, (page, actions): &mut State| {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    let tags = TagIndex::default();
                    let cx = ReposContext {
                        has_clone_root: true,
                        default_remote: "me",
                        upstream_name: "upstream",
                        tags: &tags,
                        repo_tags: &tags,
                        rules: &[],
                        launchers: &[],
                    };
                    if let Some(action) = page.show(ui, cx) {
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
        h.get_by_label("Problems (7)").click();
        h.run();
        for title in [
            "Missing folders (1)",
            "Main remote not chosen (1)",
            "Renamed or moved on GitHub (1)",
            "Linked forks without an upstream remote (1)",
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
        h.get_by_label("Use the new URL").click();
        h.run();
        h.get_by_label("Add remote \"upstream\"").click();
        h.run();
        assert_eq!(
            h.state().1,
            vec![
                ReposAction::ForgetClone(9),
                ReposAction::SetMainRemote {
                    clone_id: 3,
                    remote: Some("bob".into())
                },
                ReposAction::SetRemote {
                    path: "/home/someone/code/r/react".into(),
                    name: "me".into(),
                    url: "git@github.com:meta/react.git".into(),
                    add: false,
                },
                ReposAction::SetRemote {
                    path: "/home/someone/code/r/react~2".into(),
                    name: "upstream".into(),
                    url: "git@github.com:facebook/react.git".into(),
                    add: true,
                },
            ]
        );
    }

    /// Like `harness`, but clone 1 has a manual tag and a rule tags facebook/ repos.
    fn tagged_harness<'a>() -> Harness<'a, State> {
        use std::collections::{BTreeSet, HashMap};

        use crate::tags::{CloneFacts, RuleField};

        let mut page = ReposPage::new();
        page.scan_finished(Ok(synced_scan()));
        Harness::builder()
            .with_size([1280.0, 640.0])
            .build_ui_state(
                |ui, (page, actions): &mut State| {
                    let facts: Vec<CloneFacts> = page
                        .clones()
                        .unwrap()
                        .iter()
                        .map(|c| CloneFacts::from_clone(c, None))
                        .collect();
                    let rules = vec![Rule {
                        id: 10,
                        pattern: "^facebook/".into(),
                        tag_template: "upstream".into(),
                        ..Rule::new(RuleField::FullName)
                    }];
                    let manual = HashMap::from([(1, BTreeSet::from(["work".to_owned()]))]);
                    let tags = TagIndex::build(&facts, &manual, &rules);
                    let repo_tags = TagIndex::default();
                    egui::CentralPanel::default_margins().show(ui, |ui| {
                        let cx = ReposContext {
                            has_clone_root: true,
                            default_remote: "me",
                            upstream_name: "upstream",
                            tags: &tags,
                            repo_tags: &repo_tags,
                            rules: &rules,
                            launchers: &[LauncherInfo {
                                name: "VS Code".into(),
                                each: true,
                                together: true,
                            }],
                        };
                        if let Some(action) = page.show(ui, cx) {
                            actions.push(action);
                        }
                    });
                },
                (page, Vec::new()),
            )
    }

    #[test]
    fn details_panel_opens_a_clone() {
        let mut h = tagged_harness();
        h.run();
        h.get_by_label("facebook/react").click();
        h.run();
        h.get_by_label("Open in").click();
        h.run();
        h.get_by_label("VS Code").click();
        h.run();
        assert_eq!(
            h.state().1,
            vec![ReposAction::Open {
                launcher: "VS Code".into(),
                clone_id: 1
            }]
        );
    }

    #[test]
    fn details_panel_edits_tags() {
        let mut h = tagged_harness();
        h.run();
        h.get_by_label("facebook/react").click();
        h.run();
        assert_eq!(
            h.query_all_by_label("upstream").count(),
            2,
            "the rule tag shows in the Tags column and the details panel"
        );
        h.get_by_label("work ×").click();
        h.run();
        h.get_by_label("Add exception").click();
        h.run();
        h.state_mut().0.new_tag = "Later".into();
        h.run();
        h.get_by_label("Add").click();
        h.run();
        assert_eq!(
            h.state().1,
            vec![
                ReposAction::Tags(TagsAction::RemoveTag {
                    clone_ids: vec![1],
                    repo_ids: vec![],
                    tag: "work".into()
                }),
                ReposAction::Tags(TagsAction::SetRuleException {
                    rule_id: 10,
                    clone_id: 1,
                    excepted: true
                }),
                ReposAction::Tags(TagsAction::AddTag {
                    clone_ids: vec![1],
                    repo_ids: vec![],
                    tag: "Later".into()
                }),
            ]
        );
        assert!(h.state().0.new_tag.is_empty());
    }

    #[test]
    fn remote_list_comes_before_local_clones() {
        let mut h = harness(Harness::builder());
        h.run();
        let remote = h.get_by_label("Remote List (4)").rect();
        let local = h.get_by_label("Local Clones (5)").rect();
        assert!(remote.right() <= local.left());
        assert!(h.query_by_label_contains("Inbox").is_none());
    }

    /// Repository names in the Remote List, top to bottom.
    fn remote_list_order(h: &Harness<'_, State>) -> Vec<String> {
        let mut rows: Vec<(f32, String)> =
            ["me/dotfiles", "myorg/api", "myorg/website", "me/react"]
                .into_iter()
                .map(|name| (h.get_by_label(name).rect().top(), name.to_owned()))
                .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        rows.into_iter().map(|(_, name)| name).collect()
    }

    #[test]
    fn remote_list_sorts_by_clicked_column() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.view = View::RemoteList;
        h.run();
        assert_eq!(
            remote_list_order(&h),
            ["me/dotfiles", "me/react", "myorg/api", "myorg/website"]
        );
        assert!(h.query_by_label("1,520").is_some(), "stars use separators");

        // Stars start most-first; the repo with no count yet stays last either way.
        h.get_by_label("Stars").click();
        h.run();
        assert!(h.query_by_label("Stars ⏷").is_some());
        assert_eq!(
            remote_list_order(&h),
            ["myorg/api", "me/dotfiles", "me/react", "myorg/website"]
        );
        h.get_by_label("Stars ⏷").click();
        h.run();
        assert_eq!(
            remote_list_order(&h),
            ["me/react", "me/dotfiles", "myorg/api", "myorg/website"]
        );

        h.get_by_label("Language").click();
        h.run();
        assert_eq!(
            remote_list_order(&h),
            ["me/react", "myorg/api", "me/dotfiles", "myorg/website"]
        );
        h.get_by_label("Repository").click();
        h.run();
        h.get_by_label("Repository ⏶").click();
        h.run();
        assert_eq!(
            remote_list_order(&h),
            ["myorg/website", "myorg/api", "me/react", "me/dotfiles"]
        );
    }

    #[test]
    fn remote_list_selects_then_clones_or_ignores() {
        let mut h = harness(Harness::builder());
        h.run();
        h.get_by_label("Remote List (4)").click();
        h.run();
        h.get_by_label("me/dotfiles").click();
        h.run();
        h.get_by_label("myorg/api").click();
        h.run();
        assert!(h.query_by_label("2 selected").is_some());
        h.get_by_label("Clone").click();
        h.run_steps(3); // the cloning spinner never settles
        let Some(ReposAction::Clone(ids)) = h.state_mut().1.pop() else {
            panic!("expected a clone action");
        };
        assert_eq!(
            ids.iter().copied().collect::<HashSet<_>>(),
            HashSet::from([10, 11])
        );
        assert!(h.state().0.cloning.contains(&10));
        assert!(h.query_by_label_contains("Cloning 2 repos").is_some());

        h.get_by_label("myorg/website").click();
        h.run_steps(3); // the cloning spinner never settles
        h.get_by_label("Ignore").click();
        h.run_steps(3); // the cloning spinner never settles
        assert_eq!(
            h.state().1.last(),
            Some(&ReposAction::SetIgnored {
                repo_ids: vec![12],
                ignored: true
            })
        );
        assert!(h.query_by_label("0 selected").is_some());
    }

    #[test]
    fn remote_list_filters_and_hides_archived() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.view = View::RemoteList;
        h.state_mut().0.filter = "rust".into();
        h.run();
        assert!(h.query_by_label("myorg/api").is_some());
        assert!(h.query_by_label("me/dotfiles").is_none());

        h.state_mut().0.filter.clear();
        h.run();
        assert!(h.query_by_label("myorg/website").is_some());
        h.get_by_label("Hide archived").click();
        h.run();
        assert!(h.query_by_label("myorg/website").is_none());
        assert!(h.query_by_label("Select all shown (3)").is_some());
    }

    #[test]
    fn add_by_hand_and_remove_an_owner() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.view = View::RemoteList;
        h.state_mut().0.add_text = " rust-lang/rust ".into();
        h.run();
        h.get_by_label("Add").click();
        h.run();
        h.get_by_label("×").click();
        h.run();
        assert_eq!(
            h.state().1,
            vec![
                ReposAction::AddManual("rust-lang/rust".into()),
                ReposAction::RemoveManualOwner("rust-lang".into()),
            ]
        );
    }

    /// Like `harness`, with tags: clone 1 (facebook/react) has `work`; in the Remote List,
    /// me/dotfiles has `later` and myorg/api has `later` and `work`.
    fn tags_harness<'a>(builder: egui_kittest::HarnessBuilder<State>) -> Harness<'a, State> {
        use std::collections::{BTreeSet, HashMap};

        use crate::tags::CloneFacts;

        let set = |tags: &[&str]| {
            tags.iter()
                .map(|t| (*t).to_owned())
                .collect::<BTreeSet<_>>()
        };
        let mut page = ReposPage::new();
        page.scan_finished(Ok(synced_scan()));
        page.set_catalog(sample_catalog());
        let facts: Vec<CloneFacts> = page
            .clones()
            .unwrap()
            .iter()
            .map(|c| CloneFacts::from_clone(c, None))
            .collect();
        let tags = TagIndex::build(&facts, &HashMap::from([(1, set(&["work"]))]), &[]);
        let catalog = page.catalog().unwrap();
        let repos: Vec<CloneFacts> = catalog
            .inbox
            .iter()
            .chain(&catalog.ignored)
            .map(CloneFacts::from_repo)
            .collect();
        let repo_tags = TagIndex::build_for_repos(
            &repos,
            &HashMap::from([(10, set(&["later"])), (11, set(&["later", "work"]))]),
            &[],
        );
        builder.with_size([1280.0, 560.0]).build_ui_state(
            move |ui, (page, actions): &mut State| {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    let cx = ReposContext {
                        has_clone_root: true,
                        default_remote: "me",
                        upstream_name: "upstream",
                        tags: &tags,
                        repo_tags: &repo_tags,
                        rules: &[],
                        launchers: &[],
                    };
                    if let Some(action) = page.show(ui, cx) {
                        actions.push(action);
                    }
                });
            },
            (page, Vec::new()),
        )
    }

    fn shown(h: &Harness<'_, State>, names: &[&str]) -> Vec<String> {
        names
            .iter()
            .filter(|n| h.query_by_label(n).is_some())
            .map(|n| (*n).to_owned())
            .collect()
    }

    #[test]
    fn remote_list_filters_by_tag_query() {
        let all = ["me/dotfiles", "myorg/api", "myorg/website", "me/react"];
        let mut h = tags_harness(Harness::builder());
        h.state_mut().0.view = View::RemoteList;
        h.state_mut().0.tag_filter = "later".into();
        h.run();
        assert_eq!(shown(&h, &all), ["me/dotfiles", "myorg/api"]);

        h.state_mut().0.tag_filter = "later AND NOT work".into();
        h.run();
        assert_eq!(shown(&h, &all), ["me/dotfiles"]);

        h.state_mut().0.tag_filter = "untagged".into();
        h.run();
        assert_eq!(shown(&h, &all), ["myorg/website", "me/react"]);

        // A query still being typed doesn't hide anything, and says what's wrong.
        h.state_mut().0.tag_filter = "work AND".into();
        h.run();
        assert_eq!(shown(&h, &all), all);
        assert!(h.query_by_label_contains("ends too early").is_some());
    }

    #[test]
    fn local_clones_filter_by_tag_query() {
        let all = ["facebook/react", "someone/react", "serde-rs/serde"];
        let mut h = tags_harness(Harness::builder());
        h.state_mut().0.tag_filter = "work".into();
        h.run();
        assert_eq!(shown(&h, &all), ["facebook/react"]);
        h.state_mut().0.tag_filter = "NOT work".into();
        h.run();
        assert_eq!(shown(&h, &all), ["someone/react", "serde-rs/serde"]);
        h.state_mut().0.tag_filter = "untagged".into();
        h.run();
        assert_eq!(shown(&h, &all), ["someone/react", "serde-rs/serde"]);
    }

    #[test]
    fn tag_menu_fills_the_query() {
        let mut h = tags_harness(Harness::builder());
        h.state_mut().0.view = View::RemoteList;
        h.run();
        h.get_by_label("⏷").click();
        h.run();
        // Tags from clones and from repos, alphabetically: later, work.
        h.get_all_by_label("not").next().unwrap().click();
        h.run();
        assert_eq!(h.state().0.tag_filter, "NOT later");
        h.get_by_label("⏷").click();
        h.run();
        h.get_by_label("untagged").click();
        h.run();
        assert_eq!(h.state().0.tag_filter, "untagged");
    }

    #[test]
    fn remote_list_tags_the_selected_repos() {
        let mut h = tags_harness(Harness::builder());
        h.state_mut().0.view = View::RemoteList;
        h.run();
        h.get_by_label("me/dotfiles").click();
        h.run();
        h.get_by_label("myorg/website").click();
        h.run();
        h.get_by_label("Tag").focus();
        h.run();
        h.get_by_label("Tag").type_text("Some Day");
        h.run();
        h.get_by_label("Add tag").click();
        h.run();
        assert!(
            h.query_by_label("2 selected").is_some(),
            "tagging keeps the selection"
        );
        h.get_by_label("Tag").focus();
        h.run();
        h.get_by_label("Tag").type_text("later");
        h.run();
        h.get_by_label("Remove tag").click();
        h.run();

        let actions = std::mem::take(&mut h.state_mut().1);
        let [
            ReposAction::Tags(TagsAction::AddTag {
                clone_ids: added_clones,
                repo_ids: added,
                tag: added_tag,
            }),
            ReposAction::Tags(TagsAction::RemoveTag {
                clone_ids: removed_clones,
                repo_ids: removed,
                tag: removed_tag,
            }),
        ] = actions.as_slice()
        else {
            panic!("expected an add and a remove, got {actions:?}");
        };
        let ids = |v: &[i64]| v.iter().copied().collect::<HashSet<_>>();
        assert!(added_clones.is_empty() && removed_clones.is_empty());
        assert_eq!(ids(added), HashSet::from([10, 12]));
        assert_eq!(ids(removed), HashSet::from([10, 12]));
        assert_eq!(
            (added_tag.as_str(), removed_tag.as_str()),
            ("Some Day", "later")
        );
    }

    fn sample_import() -> ImportPreview {
        let item =
            |source: &str, name: &str, destination: &str, rename: bool, note: &str| ImportItem {
                source: PathBuf::from("/home/someone/old").join(source),
                name: name.into(),
                destination: destination.into(),
                can_rename_origin: rename,
                notes: vec![note.into()],
            };
        ImportPreview {
            searched: Some("/home/someone/old".into()),
            items: vec![
                item(
                    "react",
                    "facebook/react",
                    "r/react~2",
                    true,
                    "You already have a clone of it at r/react",
                ),
                item("notes", "notes", "n/notes", false, "No remotes"),
                item("lab/tool", "group/tool", "t/tool", true, "On gitlab.com"),
            ],
            in_place: 2,
            skipped: vec![("/home/someone/old/wt".into(), "a linked worktree".into())],
            truncated: false,
        }
    }

    #[test]
    fn import_moves_the_selected_repos() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.view = View::Import;
        h.state_mut().0.import_preview(Ok(sample_import()));
        h.run();
        assert!(h.query_by_label("3 selected").is_some());
        assert!(h.query_by_label("· 2 already in place").is_some());
        h.get_by_label("n/notes").click();
        h.run();
        h.get_by_label("Move 2 into place").click();
        h.run();
        let moves = |source: &str, destination: &str| ImportMove {
            source: PathBuf::from("/home/someone/old").join(source),
            destination: destination.into(),
            rename_origin: true,
        };
        assert_eq!(
            h.state().1,
            vec![ReposAction::Import(vec![
                moves("react", "r/react~2"),
                moves("lab/tool", "t/tool")
            ])]
        );
        assert!(
            h.query_by_label_contains("Moving 2 repos into place")
                .is_some()
        );
        assert!(
            h.query_by_label("Move 2 into place").is_none(),
            "the preview is cleared"
        );
    }

    #[test]
    fn unticking_rename_keeps_origin() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.view = View::Import;
        h.state_mut().0.import_preview(Ok(sample_import()));
        h.run();
        h.get_by_label("Rename the remote \"origin\" to \"me\"")
            .click();
        h.run();
        h.get_by_label("Move 3 into place").click();
        h.run();
        let Some(ReposAction::Import(moves)) = h.state().1.first() else {
            panic!("expected an import");
        };
        assert!(moves.iter().all(|m| !m.rename_origin));
    }

    #[test]
    fn problems_link_to_import() {
        let mut h = harness(Harness::builder());
        h.run();
        h.get_by_label("Problems (7)").click();
        h.run();
        h.get_by_label("Move into place…").click();
        h.run();
        assert!(h.state().0.view == View::Import);
        h.get_by_label("Problems (7)").click();
        h.run();
        h.get_by_label("Look for repos inside").scroll_to_me();
        h.run();
        h.get_by_label("Look for repos inside").click();
        h.run();
        assert_eq!(
            h.state().1,
            vec![
                ReposAction::PreviewImport {
                    folder: None,
                    paths: vec!["/home/someone/code/x/serde".into()],
                },
                ReposAction::PreviewImport {
                    folder: Some("/home/someone/code/p/photos".into()),
                    paths: vec![],
                },
            ]
        );
        assert_eq!(h.state().0.import.folder, "/home/someone/code/p/photos");
    }

    #[test]
    fn fork_decisions() {
        let mut h = harness(Harness::builder());
        h.run();
        h.get_by_label("Forks (1 undecided)").click();
        h.run();
        assert!(h.query_by_label_contains("suggested: Linked").is_some());
        h.get_by_label("Linked").click();
        h.run();
        assert_eq!(
            h.state().1,
            vec![ReposAction::SetForkDecision {
                fork_id: 13,
                parent_id: 20,
                decision: ForkDecision::Linked
            }]
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
        let mut h = tags_harness(Harness::builder().wgpu());
        h.run();
        h.get_by_label("someone/react").click();
        h.run();
        h.render().unwrap().save(dir.join("clones.png")).unwrap();
        h.get_by_label("Problems (7)").click();
        h.run();
        h.render().unwrap().save(dir.join("problems.png")).unwrap();
        h.get_by_label("Remote List (4)").click();
        h.run();
        h.get_by_label("myorg/api").click();
        h.run();
        h.get_by_label("Stars").click();
        h.run();
        h.render()
            .unwrap()
            .save(dir.join("remote_list.png"))
            .unwrap();
        h.state_mut().0.tag_filter = "later".into();
        h.run();
        h.render()
            .unwrap()
            .save(dir.join("remote_list_tagged.png"))
            .unwrap();
        h.state_mut().0.tag_filter.clear();
        h.get_by_label("Forks (1 undecided)").click();
        h.run();
        h.render().unwrap().save(dir.join("forks.png")).unwrap();
        h.state_mut().0.import.folder = "/home/someone/old".into();
        h.state_mut().0.import_preview(Ok(sample_import()));
        h.get_by_label("Import").click();
        h.run();
        h.render().unwrap().save(dir.join("import.png")).unwrap();
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
