//! The Repositories tab: the Remote List, Ignored and Forks lists from GitHub, local clones
//! with their status, and a Problems view.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::backend::{ImportMove, LauncherInfo, SyncReport};
use crate::db::github::RepoSummary;
use crate::db::{Catalog, ForkDecision};
use crate::git::import::ImportPreview;
use crate::git::scan::{CloneInfo, MainRemote, Problem, ScanResult};
use crate::git::status::Status;
use crate::layout;
use crate::tags::query::{self, Query};
use crate::tags::{Rule, TagIndex};
use crate::ui::filter::{Filter, Item, keep_together};
use crate::ui::import::ImportPage;
use crate::ui::remote_list::{
    self, ForkShow, ForksContext, ForksState, ListContext, ListKind, ListState, SourceFilter,
};
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

impl View {
    fn title(self) -> &'static str {
        match self {
            View::RemoteList => "Remote List",
            View::LocalClones => "Local Clones",
            View::Ignored => "Ignored",
            View::Forks => "Forks",
            View::Problems => "Problems",
            View::Import => "Import",
        }
    }
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
                "These forks are linked, but their clones have no remote for the parent. Add it, \
                 or make the fork Separate in Forks."
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

/// Which sections the Problems view shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProblemShow {
    All,
    Missing,
    Kind(ProblemKind),
    Strays,
}

impl ProblemShow {
    fn options() -> impl Iterator<Item = ProblemShow> {
        [ProblemShow::All, ProblemShow::Missing]
            .into_iter()
            .chain(ProblemKind::ALL.map(ProblemShow::Kind))
            .chain([ProblemShow::Strays])
    }

    fn label(self) -> &'static str {
        match self {
            ProblemShow::All => "All problems",
            ProblemShow::Missing => "Missing folders",
            ProblemShow::Kind(kind) => kind.title(),
            ProblemShow::Strays => "Not git repos",
        }
    }

    fn shows(self, section: ProblemShow) -> bool {
        self == ProblemShow::All || self == section
    }
}

const MISSING_HELP: &str = "An earlier scan found these, but their folder is gone. A folder moved \
    within the clone root is matched back to its old entry by its main remote. Forget removes the \
    entry and its tags.";
const STRAYS_HELP: &str = "Folders under the clone root that aren't git repos. Only \
    <letter>/<name> is scanned, so repos deeper inside these aren't seen; Import can find them.";

pub struct ReposPage {
    view: View,
    /// The search, language and hide toggles every view but Import shares.
    filter: Filter,
    /// A tag query for the Remote List, Ignored and Local Clones, like `work AND NOT old`.
    tag_filter: String,
    /// Remote List and Ignored: only repos from this source.
    source: SourceFilter,
    /// Local Clones: which clones to list.
    show: Show,
    /// Local Clones: branch names, like `main, dev` or `!main`.
    branch: String,
    /// Forks: which forks to list.
    fork_show: ForkShow,
    /// Problems: which sections to list.
    problem_show: ProblemShow,
    /// Every language a synced repo has, for the language menu.
    languages: Vec<String>,
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
    catalog: Option<Catalog>,
    remote: ListState,
    ignored: ListState,
    /// Repo IDs with a clone run in progress.
    cloning: HashSet<i64>,
    add_text: String,
    forks: ForksState,
    /// IDs of ignored repos, to tell which clones are of one.
    ignored_repos: HashSet<i64>,
    upstream_name: String,
    import: ImportPage,
    /// The "add tag" field in the details panel.
    new_tag: String,
}

impl ReposPage {
    pub fn new() -> Self {
        Self {
            view: View::LocalClones,
            filter: Filter::new(),
            tag_filter: String::new(),
            source: SourceFilter::Any,
            show: Show::All,
            branch: String::new(),
            fork_show: ForkShow::All,
            problem_show: ProblemShow::All,
            languages: Vec::new(),
            sort: SortKey::Name,
            descending: false,
            selected: None,
            scanning: None,
            result: None,
            action_error: None,
            notice: None,
            syncing: None,
            catalog: None,
            remote: ListState::new(),
            ignored: ListState::new(),
            cloning: HashSet::new(),
            add_text: String::new(),
            forks: ForksState::new(),
            ignored_repos: HashSet::new(),
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

    /// The sync's summary goes to the log; only a failure is shown here too.
    pub fn sync_finished(&mut self, result: Result<SyncReport, String>) {
        self.syncing = None;
        if let Err(e) = result {
            self.action_error = Some(format!("Sync failed: {e}"));
        }
    }

    pub fn set_catalog(&mut self, catalog: Catalog) {
        self.remote
            .retain(catalog.inbox.iter().chain(&catalog.ignored_remote));
        self.ignored.retain(&catalog.ignored);
        self.ignored_repos = catalog.ignored.iter().map(|r| r.id).collect();
        let mut languages: Vec<String> = catalog
            .summaries
            .values()
            .filter_map(|s| s.language.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        languages.sort_by_key(|l| l.to_lowercase());
        self.languages = languages;
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

        self.tabs(ui, result.as_ref(), catalog.as_ref(), &mut action);
        ui.separator();
        if self.view != View::Import {
            self.filters(
                ui,
                result.as_ref(),
                catalog.as_ref(),
                (&cx, tag_query.as_ref()),
            );
        }
        self.messages(ui, &mut action);
        if self.view != View::Import {
            ui.separator();
        }

        let summaries = catalog.as_ref().map(|c| &c.summaries);
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
                    View::Problems => self.problems_view(ui, scan, summaries, &mut action),
                    _ => self.clones_view(
                        ui,
                        (scan, summaries),
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

    /// Errors and notices.
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
        let matcher = self.filter.matcher(self.view != View::Ignored);
        let list = ListContext {
            matcher: &matcher,
            source: self.source,
            tag_query,
            show_ignored: !self.filter.hide_ignored,
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
                    (&catalog.inbox, &catalog.ignored_remote),
                    &mut self.remote,
                    &list,
                    action,
                );
            }
            View::Ignored => remote_list::repo_list(
                ui,
                ListKind::Ignored,
                (&catalog.ignored, &[]),
                &mut self.ignored,
                &list,
                action,
            ),
            _ => {
                let forks = ForksContext {
                    matcher: &matcher,
                    show: self.fork_show,
                    ignored: &self.ignored_repos,
                };
                remote_list::forks_view(ui, catalog, &forks, &mut self.forks, action);
            }
        }
    }

    /// The view tabs, with Sync and Rescan (and their progress) on the right.
    fn tabs(
        &mut self,
        ui: &mut egui::Ui,
        result: Option<&Result<ScanResult, String>>,
        catalog: Option<&Catalog>,
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
                ui.selectable_value(
                    &mut self.view,
                    View::Forks,
                    format!("Forks ({})", count(catalog.forks.len())),
                );
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

                // Work in progress. When it's done, the log says how it went.
                if let Some((done, total)) = self.scanning {
                    ui.spinner();
                    if total > 0 {
                        ui.label(format!("Scanning… {done} / {total}"));
                    } else {
                        ui.label("Scanning…");
                    }
                }
                if let Some(message) = &self.syncing {
                    ui.spinner();
                    ui.add(egui::Label::new(format!("Syncing: {message}")).truncate());
                }
            });
        });
    }

    /// Two rows under the tabs: the filters every view shares, then the ones only this view
    /// has.
    fn filters(
        &mut self,
        ui: &mut egui::Ui,
        result: Option<&Result<ScanResult, String>>,
        catalog: Option<&Catalog>,
        (cx, tag_query): (&ReposContext<'_>, Option<&Result<Query, String>>),
    ) {
        let scan = match result {
            Some(Ok(scan)) => Some(scan),
            _ => None,
        };
        let ignored_clones = || {
            scan.map_or(0, |scan| {
                scan.clones.iter().filter(|c| self.is_ignored(c)).count()
            })
        };
        let ignored_hover = match self.view {
            View::RemoteList => Some(format!(
                "Leave out ignored repos ({}); they're in the Ignored list",
                thousands(catalog.map_or(0, |c| c.ignored_remote.len()) as u64)
            )),
            View::LocalClones | View::Problems => Some(format!(
                "Leave out clones of repos you ignored ({})",
                thousands(ignored_clones() as u64)
            )),
            View::Forks => Some("Leave out forks you ignored".to_owned()),
            View::Ignored | View::Import => None,
        };
        // Wraps onto more lines in a narrow window.
        ui.horizontal_wrapped(|ui| {
            self.filter
                .controls(ui, &self.languages, ignored_hover.as_deref());
        });

        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .corner_radius(4.0)
            .inner_margin(egui::Margin::symmetric(6, 3))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(self.view.title()).strong())
                        .on_hover_text("Filters only this view has");
                    ui.separator();
                    match self.view {
                        View::RemoteList | View::Ignored => {
                            combo(
                                ui,
                                ("Source", "repos_source"),
                                &mut self.source,
                                SourceFilter::ALL.map(|s| (s, s.label())),
                            );
                            ui.separator();
                            keep_together(ui, 280.0);
                            self.tag_filter_field(ui, cx, tag_query);
                        }
                        View::LocalClones => {
                            combo(
                                ui,
                                ("Show", "repos_show"),
                                &mut self.show,
                                Show::ALL.map(|s| (s, s.label())),
                            );
                            ui.separator();
                            keep_together(ui, 220.0);
                            self.branch_field(ui, scan);
                            ui.separator();
                            keep_together(ui, 280.0);
                            self.tag_filter_field(ui, cx, tag_query);
                        }
                        View::Forks => {
                            combo(
                                ui,
                                ("Show", "forks_show"),
                                &mut self.fork_show,
                                ForkShow::ALL.map(|s| (s, s.label())),
                            );
                        }
                        View::Problems => {
                            combo(
                                ui,
                                ("Show", "problems_show"),
                                &mut self.problem_show,
                                ProblemShow::options().map(|s| (s, s.label())),
                            );
                        }
                        View::Import => {}
                    }

                    if self.any_filter() {
                        ui.separator();
                        if ui
                            .button("Clear filters")
                            .on_hover_text(
                                "Empty the search, language, branch and tag fields and show \
                                 everything; the toggles stay as they are",
                            )
                            .clicked()
                        {
                            self.clear_filters();
                        }
                    }
                });
            });
    }

    /// Something narrows the current view, besides the toggles.
    fn any_filter(&self) -> bool {
        self.filter.has_text()
            || match self.view {
                View::RemoteList | View::Ignored => {
                    self.source != SourceFilter::Any || !self.tag_filter.trim().is_empty()
                }
                View::LocalClones => {
                    self.show != Show::All
                        || !self.branch.trim().is_empty()
                        || !self.tag_filter.trim().is_empty()
                }
                View::Forks => self.fork_show != ForkShow::All,
                View::Problems => self.problem_show != ProblemShow::All,
                View::Import => false,
            }
    }

    fn clear_filters(&mut self) {
        self.filter.clear_text();
        self.tag_filter.clear();
        self.source = SourceFilter::Any;
        self.show = Show::All;
        self.branch.clear();
        self.fork_show = ForkShow::All;
        self.problem_show = ProblemShow::All;
    }

    /// Local Clones: the branch field, with a menu of the branches clones are on.
    fn branch_field(&mut self, ui: &mut egui::Ui, scan: Option<&ScanResult>) {
        let label = ui.label("Branch");
        ui.add(
            egui::TextEdit::singleline(&mut self.branch)
                .hint_text("main, or !main")
                .desired_width(130.0),
        )
        .labelled_by(label.id)
        .on_hover_text(
            "Part of a branch name. Separate several with commas to match any of them; \
             start one with ! to leave out branches that have it.",
        );
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for clone in scan.map_or(&[][..], |s| &s.clones) {
            if let Ok(Status {
                branch: Some(branch),
                ..
            }) = &clone.status
            {
                *counts.entry(branch).or_default() += 1;
            }
        }
        let mut branches: Vec<(&str, usize)> = counts.into_iter().collect();
        branches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        ui.menu_button("⏷", |ui| {
            if branches.is_empty() {
                ui.weak("No branches yet");
            }
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for (branch, count) in branches {
                        ui.horizontal(|ui| {
                            if ui
                                .button(branch)
                                .on_hover_text(format!(
                                    "On {branch} ({})",
                                    plural(count as u32, "clone", "clones")
                                ))
                                .clicked()
                            {
                                self.branch = branch.to_owned();
                                ui.close();
                            }
                            if ui
                                .small_button("not")
                                .on_hover_text(format!("On any branch but {branch}"))
                                .clicked()
                            {
                                self.branch = format!("!{branch}");
                                ui.close();
                            }
                            ui.weak(thousands(count as u64));
                        });
                    }
                });
        })
        .response
        .on_hover_text("Pick a branch");
    }

    /// The tag query field, with a menu of known tags.
    fn tag_filter_field(
        &mut self,
        ui: &mut egui::Ui,
        cx: &ReposContext<'_>,
        parsed: Option<&Result<Query, String>>,
    ) {
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

    /// The clone's main repo is ignored.
    fn is_ignored(&self, clone: &CloneInfo) -> bool {
        clone
            .repo_id
            .is_some_and(|id| self.ignored_repos.contains(&id))
    }

    fn clones_view(
        &mut self,
        ui: &mut egui::Ui,
        (scan, summaries): (&ScanResult, Option<&HashMap<i64, RepoSummary>>),
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

        let matcher = self.filter.matcher(true);
        let mut rows: Vec<&CloneInfo> = scan
            .clones
            .iter()
            .filter(|c| {
                matcher.matches(&clone_item(c, summaries, &self.ignored_repos))
                    && self.show.matches(c)
                    && branch_matches(
                        &self.branch,
                        c.status.as_ref().ok().and_then(|s| s.branch.as_deref()),
                    )
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
                    let ignored = !self.filter.hide_ignored && self.is_ignored(clone);
                    row.col(|ui| name_cell(ui, clone, ignored));
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
        summaries: Option<&HashMap<i64, RepoSummary>>,
        action: &mut Option<ReposAction>,
    ) {
        if scan.problem_count() == 0 {
            ui.add_space(8.0);
            ui.weak("No problems found.");
            return;
        }
        let matcher = self.filter.matcher(true);
        let show = self.problem_show;
        // Missing folders and stray folders have no repo to describe them: they match by name
        // and folder alone, and only while no language is picked.
        let bare = |full_name: Option<&str>, path: &str| {
            let folder = path.rsplit('/').next().unwrap_or(path);
            let (owner, name) = match full_name.and_then(|n| n.split_once('/')) {
                Some((owner, name)) => (Some(owner), name),
                None => (None, layout::split_suffix(folder).0),
            };
            matcher.matches(&Item {
                owner,
                name,
                folder: Some(folder),
                ..Item::default()
            })
        };
        let mut listed = 0;

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let missing: Vec<_> = scan
                    .missing
                    .iter()
                    .filter(|m| {
                        show.shows(ProblemShow::Missing) && bare(m.name.as_deref(), &m.path)
                    })
                    .collect();
                listed += missing.len();
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
                    if !show.shows(ProblemShow::Kind(kind)) {
                        continue;
                    }
                    let entries: Vec<(&CloneInfo, &Problem)> = scan
                        .clones
                        .iter()
                        .filter(|c| matcher.matches(&clone_item(c, summaries, &self.ignored_repos)))
                        .flat_map(|c| {
                            c.problems
                                .iter()
                                .filter(|p| ProblemKind::of(p) == kind)
                                .map(move |p| (c, p))
                        })
                        .collect();
                    listed += entries.len();
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
                    .filter(|s| show.shows(ProblemShow::Strays) && bare(None, s))
                    .collect();
                listed += strays.len();
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
                if listed == 0 {
                    ui.add_space(8.0);
                    ui.weak("Nothing matches the filter.");
                }
            });
    }
}

/// A labelled drop-down that picks one of `options`.
fn combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    (label, id): (&str, &str),
    value: &mut T,
    options: impl IntoIterator<Item = (T, &'static str)>,
) {
    let options: Vec<(T, &str)> = options.into_iter().collect();
    let selected = options
        .iter()
        .find(|(option, _)| option == value)
        .map_or("", |(_, text)| text);
    keep_together(ui, 200.0);
    let label = ui.label(label);
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (option, text) in options {
                ui.selectable_value(value, option, text);
            }
        })
        .response
        .labelled_by(label.id);
}

/// `spec` is comma-separated parts of branch names: a branch matches if it has any of them
/// and none of the ones starting with `!`. A detached HEAD has no branch name.
fn branch_matches(spec: &str, branch: Option<&str>) -> bool {
    let branch = branch.unwrap_or("").to_lowercase();
    let mut wanted = false;
    let mut found = false;
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.strip_prefix('!').map(str::trim) {
            Some("") => {}
            Some(unwanted) => {
                if branch.contains(&unwanted.to_lowercase()) {
                    return false;
                }
            }
            None => {
                wanted = true;
                found |= branch.contains(&part.to_lowercase());
            }
        }
    }
    !wanted || found
}

/// What the shared filter sees of a clone: its repo's owner and name (or its folder), and what
/// the last sync said about that repo.
fn clone_item<'a>(
    clone: &'a CloneInfo,
    summaries: Option<&'a HashMap<i64, RepoSummary>>,
    ignored: &HashSet<i64>,
) -> Item<'a> {
    let summary = clone
        .repo_id
        .and_then(|id| summaries.and_then(|s| s.get(&id)));
    let url = clone.repo();
    Item {
        owner: url.map(|u| u.owner.as_str()),
        name: url.map_or_else(
            || layout::split_suffix(clone.folder_name()).0,
            |u| u.name.as_str(),
        ),
        folder: Some(clone.folder_name()),
        description: summary.and_then(|s| s.description.as_deref()),
        language: summary.and_then(|s| s.language.as_deref()),
        ignored: clone.repo_id.is_some_and(|id| ignored.contains(&id)),
        archived: summary.is_some_and(|s| s.is_archived),
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

/// `ignored`: the clone's repo is ignored, and that's worth saying here.
fn name_cell(ui: &mut egui::Ui, clone: &CloneInfo, ignored: bool) {
    ui.label(clone.display_name());
    if let Some(repo) = clone.repo()
        && !repo.is_github()
    {
        ui.weak(&repo.host);
    }
    if ignored {
        ui.weak("ignored")
            .on_hover_text("Its repo is in the Ignored list.");
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
    use crate::db::github::ForkRow;
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
            ignored: false,
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
        fork.fork_of = Some(("facebook/react".into(), ForkDecision::Linked));
        fork.stars = Some(0);
        let mut api = repo_row(11, "myorg/api", "Rust", "2026-11-01", &[Source::OrgMember]);
        api.stars = Some(1520);
        let mut old_thing = repo_row(
            14,
            "myorg/old-thing",
            "Perl",
            "2019-01-01",
            &[Source::OrgMember],
        );
        old_thing.ignored = true;
        let mut serde = repo_row(16, "serde-rs/serde", "Rust", "2026-11-02", &[]);
        serde.ignored = true;
        let inbox = vec![
            repo_row(10, "me/dotfiles", "Shell", "2026-11-20", &[Source::Owned]),
            api,
            website,
            fork,
        ];
        let ignored = vec![old_thing.clone(), serde];
        let summary = |row: &RepoRow| RepoSummary {
            description: row.description.clone(),
            language: row.language.clone(),
            is_archived: row.is_archived,
        };
        let mut summaries: HashMap<i64, RepoSummary> = inbox
            .iter()
            .chain(&ignored)
            .map(|r| (r.id, summary(r)))
            .collect();
        summaries.insert(
            20,
            RepoSummary {
                description: Some("The library for web and native user interfaces".into()),
                language: Some("JavaScript".into()),
                is_archived: false,
            },
        );
        Catalog {
            last_sync: Some("2026-11-21 18:00:00".into()),
            relevant: 5,
            inbox,
            ignored,
            ignored_remote: vec![old_thing],
            forks: vec![
                ForkRow {
                    fork_id: 13,
                    fork_name: "me/react".into(),
                    parent_id: 20,
                    parent_name: "facebook/react".into(),
                    decision: ForkDecision::Linked,
                    chosen: false,
                    fork_cloned: false,
                    parent_cloned: false,
                    parent_listed: true,
                    has_upstream_remote: false,
                },
                ForkRow {
                    fork_id: 15,
                    fork_name: "me/serde".into(),
                    parent_id: 21,
                    parent_name: "serde-rs/serde".into(),
                    decision: ForkDecision::Separate,
                    chosen: false,
                    fork_cloned: true,
                    parent_cloned: true,
                    parent_listed: false,
                    has_upstream_remote: false,
                },
            ],
            manual_owners: vec!["rust-lang".into()],
            summaries,
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
    fn ignored_repos_are_hidden_until_unticked() {
        let mut h = harness(Harness::builder());
        // x/serde is a clone of serde-rs/serde, which is ignored.
        if let Some(Ok(scan)) = &mut h.state_mut().0.result {
            scan.clones[4].repo_id = Some(16);
        }
        h.run();
        assert!(h.query_by_label("serde-rs/serde").is_none());
        assert!(h.query_by_label("facebook/react").is_some());
        h.get_by_label("Hide ignored").click();
        h.run();
        assert!(h.query_by_label("serde-rs/serde").is_some());
        assert!(h.query_by_label("ignored").is_some());

        // The toggle is shared, so the Remote List lists ignored repos now too.
        h.get_by_label("Remote List (4)").click();
        h.run();
        assert!(h.query_by_label("ignored").is_some());
        h.get_by_label("Hide ignored").click();
        h.run();
        assert!(h.query_by_label("myorg/old-thing").is_none());
        assert!(h.query_by_label("ignored").is_none());
        h.get_by_label("Hide ignored").click();
        h.run();
        // Everything in Ignored is ignored, so the toggle isn't offered there.
        h.get_by_label("Ignored (2)").click();
        h.run();
        assert!(h.query_by_label("Hide ignored").is_none());
        assert!(h.query_by_label("myorg/old-thing").is_some());
        h.get_by_label("Remote List (4)").click();
        h.run();
        h.get_by_label("myorg/old-thing").click();
        h.run();
        h.get_by_label("Un-ignore").click();
        h.run();
        assert_eq!(
            h.state().1,
            vec![ReposAction::SetIgnored {
                repo_ids: vec![14],
                ignored: false
            }]
        );
    }

    #[test]
    fn filter_narrows_the_list() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.filter.text = "serde".into();
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

        // The column title, not the filter's label.
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Language")
            .click();
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
        h.state_mut().0.filter.language = "rust".into();
        h.run();
        assert!(h.query_by_label("myorg/api").is_some());
        assert!(h.query_by_label("me/dotfiles").is_none());

        h.state_mut().0.filter.language.clear();
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

    /// The ⏷ menu button right after the field labelled `label`.
    fn menu_after<'h>(h: &'h Harness<'_, State>, label: &str) -> egui_kittest::Node<'h> {
        let field = h
            .get_by_role_and_label(egui::accesskit::Role::TextInput, label)
            .rect();
        h.get_all_by_label("⏷")
            .filter(|menu| menu.rect().left() >= field.right())
            .min_by(|a, b| a.rect().left().total_cmp(&b.rect().left()))
            .unwrap()
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
        menu_after(&h, "Tags").click();
        h.run();
        // Tags from clones and from repos, alphabetically: later, work.
        h.get_all_by_label("not").next().unwrap().click();
        h.run();
        assert_eq!(h.state().0.tag_filter, "NOT later");
        menu_after(&h, "Tags").click();
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
        h.get_by_label("Forks (2)").click();
        h.run();
        for label in [
            "Fork Repository",
            "Upstream Repository",
            "Linked / Separate",
            "Notes",
            "facebook/react",
            "upstream listed",
            "upstream not in your sources",
            "cloned without upstream remote",
        ] {
            assert!(h.query_by_label_contains(label).is_some(), "{label}");
        }
        assert!(h.query_by_label_contains("ndecided").is_none());
        // The button shows the decision; clicking it switches to the other one.
        h.get_by_label("Linked").click();
        h.run();
        h.get_by_label("Separate").click();
        h.run();
        assert_eq!(
            h.state().1,
            vec![
                ReposAction::SetForkDecision {
                    fork_id: 13,
                    parent_id: 20,
                    decision: ForkDecision::Separate
                },
                ReposAction::SetForkDecision {
                    fork_id: 15,
                    parent_id: 21,
                    decision: ForkDecision::Linked
                }
            ]
        );
    }

    const CLONES: [&str; 5] = [
        "facebook/react",
        "someone/react",
        "thing",
        "notes",
        "serde-rs/serde",
    ];

    #[test]
    fn local_clones_search_chosen_fields() {
        let mut h = harness(Harness::builder());
        // facebook/react's repo has a description from the last sync.
        if let Some(Ok(scan)) = &mut h.state_mut().0.result {
            scan.clones[0].repo_id = Some(20);
        }
        h.state_mut().0.filter.text = "react".into();
        h.run();
        assert_eq!(shown(&h, &CLONES), ["facebook/react", "someone/react"]);

        // Owner only: "react" is nobody's name.
        h.get_by_label("Name").click();
        h.run();
        assert_eq!(shown(&h, &CLONES), Vec::<String>::new());
        assert!(h.query_by_label("Nothing matches the filter.").is_some());
        h.state_mut().0.filter.text = "someone".into();
        h.run();
        assert_eq!(shown(&h, &CLONES), ["someone/react"]);

        // The last field on can't be turned off.
        h.get_by_label("Owner").click();
        h.run();
        assert!(h.state().0.filter.fields.owner);

        // Description only.
        h.get_by_label("Description").click();
        h.run();
        h.get_by_label("Owner").click();
        h.run();
        h.state_mut().0.filter.text = "native".into();
        h.run();
        assert_eq!(shown(&h, &CLONES), ["facebook/react"]);
    }

    #[test]
    fn local_clones_filter_by_language_and_archived() {
        let mut h = harness(Harness::builder());
        if let Some(Ok(scan)) = &mut h.state_mut().0.result {
            scan.clones[0].repo_id = Some(20); // facebook/react, JavaScript
            scan.clones[4].repo_id = Some(11); // as if it were myorg/api: Rust
        }
        h.state_mut().0.filter.language = "javascript, Rust".into();
        h.run();
        assert_eq!(shown(&h, &CLONES), ["facebook/react", "serde-rs/serde"]);
        h.state_mut().0.filter.language = "Cobol".into();
        h.run();
        assert!(h.query_by_label("No repo is in Cobol").is_some());

        h.state_mut().0.filter.language.clear();
        h.state_mut()
            .0
            .catalog
            .as_mut()
            .unwrap()
            .summaries
            .get_mut(&20)
            .unwrap()
            .is_archived = true;
        h.run();
        assert_eq!(shown(&h, &CLONES), CLONES);
        h.get_by_label("Hide archived").click();
        h.run();
        assert_eq!(
            shown(&h, &CLONES),
            ["someone/react", "thing", "notes", "serde-rs/serde"]
        );
    }

    #[test]
    fn language_menu_picks_languages() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.view = View::RemoteList;
        h.run();
        menu_after(&h, "Language").click();
        h.run();
        let checkbox = |h: &Harness<'_, State>, label: &str| {
            h.get_by_role_and_label(egui::accesskit::Role::CheckBox, label)
                .click();
        };
        checkbox(&h, "Rust");
        h.run();
        checkbox(&h, "Shell");
        h.run();
        assert_eq!(h.state().0.filter.language, "Rust, Shell");
        checkbox(&h, "Rust");
        h.run();
        assert_eq!(h.state().0.filter.language, "Shell");
    }

    #[test]
    fn local_clones_filter_by_branch() {
        let mut h = harness(Harness::builder());
        if let Some(Ok(scan)) = &mut h.state_mut().0.result {
            if let Ok(status) = &mut scan.clones[1].status {
                status.branch = Some("feature/hooks".into());
            }
            if let Ok(status) = &mut scan.clones[2].status {
                status.branch = None; // detached
            }
        }
        for (branch, expected) in [
            ("feat", &["someone/react"][..]),
            ("!main", &["someone/react", "thing"][..]),
            ("!main, !hooks", &["thing"][..]),
            (
                "hooks, MAIN",
                &["facebook/react", "someone/react", "notes", "serde-rs/serde"][..],
            ),
        ] {
            h.state_mut().0.branch = branch.into();
            h.run();
            assert_eq!(shown(&h, &CLONES), expected, "{branch}");
        }

        // The menu lists branches, most used first.
        h.state_mut().0.branch.clear();
        h.run();
        menu_after(&h, "Branch").click();
        h.run();
        h.get_all_by_label("not").next().unwrap().click();
        h.run();
        assert_eq!(h.state().0.branch, "!main");
    }

    #[test]
    fn clear_filters_empties_the_fields() {
        let mut h = harness(Harness::builder());
        h.run();
        assert!(h.query_by_label("Clear filters").is_none());
        {
            let page = &mut h.state_mut().0;
            page.filter.text = "nothing like it".into();
            page.branch = "dev".into();
            page.show = Show::Stashes;
            page.filter.hide_archived = true;
        }
        h.run();
        assert_eq!(shown(&h, &CLONES), Vec::<String>::new());
        h.get_by_label("Clear filters").click();
        h.run();
        assert_eq!(shown(&h, &CLONES), CLONES);
        assert!(h.state().0.filter.hide_archived, "toggles stay");
        assert!(h.query_by_label("Clear filters").is_none());
    }

    #[test]
    fn remote_list_filters_by_source() {
        let all = ["me/dotfiles", "myorg/api", "myorg/website", "me/react"];
        let mut h = harness(Harness::builder());
        h.state_mut().0.view = View::RemoteList;
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Source")
            .click();
        h.run();
        h.get_by_label("Org member").click();
        h.run();
        assert_eq!(shown(&h, &all), ["myorg/api", "myorg/website"]);
        h.state_mut().0.source = SourceFilter::Contributed;
        h.run();
        assert_eq!(shown(&h, &all), ["myorg/website"]);
    }

    #[test]
    fn forks_filter_by_show_and_search() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.view = View::Forks;
        h.run();
        assert!(h.query_by_label("me/react").is_some());
        assert!(h.query_by_label("me/serde").is_some());
        h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Show")
            .click();
        h.run();
        h.get_by_label("Separate forks").click();
        h.run();
        assert!(h.query_by_label("me/react").is_none());
        assert!(h.query_by_label("me/serde").is_some());

        // The search looks at the upstream too.
        h.state_mut().0.fork_show = ForkShow::All;
        h.state_mut().0.filter.text = "facebook".into();
        h.run();
        assert!(h.query_by_label("me/react").is_some());
        assert!(h.query_by_label("me/serde").is_none());

        // Language goes by the fork (me/react is JavaScript).
        h.state_mut().0.filter.text.clear();
        h.state_mut().0.filter.language = "javascript".into();
        h.run();
        assert!(h.query_by_label("me/react").is_some());
        assert!(h.query_by_label("me/serde").is_none());
    }

    #[test]
    fn problems_show_one_section() {
        let mut h = harness(Harness::builder());
        h.state_mut().0.view = View::Problems;
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Show")
            .click();
        h.run();
        h.get_by_label("No remotes").click();
        h.run();
        assert!(h.query_by_label("No remotes (1)").is_some());
        assert!(h.query_by_label("Missing folders (1)").is_none());
        assert!(h.query_by_label("Not git repos (1)").is_none());

        // Folders without a repo match by name.
        h.state_mut().0.problem_show = ProblemShow::All;
        h.state_mut().0.filter.text = "photos".into();
        h.run();
        assert!(h.query_by_label("Not git repos (1)").is_some());
        assert!(h.query_by_label("No remotes (1)").is_none());
        h.state_mut().0.filter.text = "zzz".into();
        h.run();
        assert!(h.query_by_label("Nothing matches the filter.").is_some());
    }

    #[test]
    fn filters_sit_between_the_tabs_and_the_list() {
        let mut h = harness(Harness::builder());
        h.run();
        let tabs = h.get_by_label("Local Clones (5)").rect();
        let search = h
            .get_by_role_and_label(egui::accesskit::Role::TextInput, "Search")
            .rect();
        let branch = h
            .get_by_role_and_label(egui::accesskit::Role::TextInput, "Branch")
            .rect();
        let header = h.get_by_label("Repository ⏶").rect();
        assert!(tabs.bottom() < search.top());
        assert!(
            search.bottom() < branch.top(),
            "view filters get a row of their own"
        );
        assert!(branch.bottom() < header.top());

        // Import has none.
        h.get_by_label("Import").click();
        h.run();
        assert!(h.query_by_label("Search").is_none());
    }

    #[test]
    fn branch_specs() {
        assert!(branch_matches("", None));
        assert!(branch_matches(" , ! ", Some("main")));
        assert!(branch_matches("Main", Some("main")));
        assert!(!branch_matches("main", None));
        assert!(branch_matches("!main", None));
        assert!(branch_matches("dev, main", Some("main")));
        assert!(!branch_matches("dev, !main", Some("main")));
        assert!(!branch_matches("dev, !old", Some("dev-old")));
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
        h.state_mut().0.branch = "!main".into();
        h.state_mut().0.filter.language = "Cobol".into();
        h.set_size(egui::vec2(700.0, 560.0));
        h.run();
        h.render()
            .unwrap()
            .save(dir.join("clones_narrow.png"))
            .unwrap();
        h.state_mut().0.clear_filters();
        h.set_size(egui::vec2(1280.0, 560.0));
        h.run();
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
        h.get_by_label("Hide ignored").click();
        h.run();
        h.render()
            .unwrap()
            .save(dir.join("remote_list_ignored.png"))
            .unwrap();
        h.get_by_label("Hide ignored").click();
        h.run();
        h.get_by_label("Forks (2)").click();
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
