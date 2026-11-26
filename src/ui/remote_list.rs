//! The Remote List, Ignored and Forks views of the Repositories tab.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::db::github::ForkRow;
use crate::db::{Catalog, ForkDecision, RepoRow, Source};
use crate::git::scan::CloneInfo;
use crate::tags::TagIndex;
use crate::tags::query::Query;
use crate::ui::filter::{Item, Matcher};
use crate::ui::repos::ReposAction;
use crate::ui::selection::{Extras, PanelContext, Selection, SelectionPanel, Targets};
use crate::ui::tags::tag_summary;
use crate::ui::thousands;
use crate::ui::{cmp_text, present_first, sort_header};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Remote,
    Ignored,
}

/// The columns a list can be sorted by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortKey {
    Name,
    Sources,
    Language,
    Stars,
    Pushed,
    Tags,
    Description,
}

impl SortKey {
    /// Counts and dates read best biggest or newest first.
    fn starts_descending(self) -> bool {
        matches!(self, SortKey::Stars | SortKey::Pushed)
    }
}

/// Which source a repo needs to show in the Remote List or Ignored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceFilter {
    Any,
    Owned,
    OrgMember,
    Collaborator,
    Contributed,
    ByHand,
}

impl SourceFilter {
    pub const ALL: [SourceFilter; 6] = [
        SourceFilter::Any,
        SourceFilter::Owned,
        SourceFilter::OrgMember,
        SourceFilter::Collaborator,
        SourceFilter::Contributed,
        SourceFilter::ByHand,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SourceFilter::Any => "Any source",
            SourceFilter::Owned => "Owned",
            SourceFilter::OrgMember => "Org member",
            SourceFilter::Collaborator => "Collaborator",
            SourceFilter::Contributed => "Contributed",
            SourceFilter::ByHand => "Added by hand",
        }
    }

    fn matches(self, sources: &[Source]) -> bool {
        let has = |source| sources.contains(&source);
        match self {
            SourceFilter::Any => true,
            SourceFilter::Owned => has(Source::Owned),
            SourceFilter::OrgMember => has(Source::OrgMember),
            SourceFilter::Collaborator => has(Source::Collaborator),
            SourceFilter::Contributed => has(Source::Contributed),
            SourceFilter::ByHand => has(Source::ManualRepo) || has(Source::ManualOwner),
        }
    }
}

/// What the filter sees of a repo in the Remote List or Ignored.
fn repo_item(row: &RepoRow) -> Item<'_> {
    Item {
        owner: Some(&row.owner),
        name: &row.name,
        folder: None,
        description: row.description.as_deref(),
        language: row.language.as_deref(),
        ignored: row.ignored,
        archived: row.is_archived,
    }
}

/// Selection and sort order for one list (Remote List or Ignored).
pub struct ListState {
    pub selection: Selection,
    sort: SortKey,
    descending: bool,
}

impl ListState {
    pub fn new() -> Self {
        Self {
            selection: Selection::new(),
            sort: SortKey::Name,
            descending: false,
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, label: &str, key: SortKey) {
        sort_header(
            ui,
            label,
            key,
            &mut self.sort,
            &mut self.descending,
            key.starts_descending(),
        );
    }

    /// Drops selected IDs that are no longer in the list.
    pub fn retain<'a>(&mut self, rows: impl IntoIterator<Item = &'a RepoRow>) {
        let present: Vec<i64> = rows.into_iter().map(|r| r.id).collect();
        self.selection.retain(&present);
    }
}

fn by_name(a: &RepoRow, b: &RepoRow) -> Ordering {
    cmp_text(&a.owner, &b.owner).then_with(|| cmp_text(&a.name, &b.name))
}

/// Text worth sorting on: not missing and not blank.
fn text(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.trim().is_empty())
}

/// Sorts by `key`. Rows without that value (no language, no tags, stars not fetched yet, …)
/// go last in both directions; ties go by name.
fn sort_rows(rows: &mut [&RepoRow], key: SortKey, descending: bool, tags: &TagIndex) {
    // Joined once up front rather than on every comparison.
    let tag_text: HashMap<i64, String> = if key == SortKey::Tags {
        rows.iter()
            .map(|r| {
                (
                    r.id,
                    tags.tags_of(r.id).into_iter().collect::<Vec<_>>().join(" "),
                )
            })
            .collect()
    } else {
        HashMap::new()
    };
    let joined = |r: &RepoRow| {
        tag_text
            .get(&r.id)
            .map(String::as_str)
            .filter(|t| !t.is_empty())
    };
    rows.sort_by(|a, b| {
        let order = match key {
            SortKey::Name => present_first(Some(a), Some(b), descending, |x, y| by_name(x, y)),
            SortKey::Sources => present_first(
                Some(&a.sources).filter(|s| !s.is_empty()),
                Some(&b.sources).filter(|s| !s.is_empty()),
                descending,
                Ord::cmp,
            ),
            SortKey::Language => {
                present_first(text(&a.language), text(&b.language), descending, |x, y| {
                    cmp_text(x, y)
                })
            }
            SortKey::Stars => present_first(a.stars, b.stars, descending, Ord::cmp),
            SortKey::Pushed => present_first(
                a.pushed_at.as_deref(),
                b.pushed_at.as_deref(),
                descending,
                Ord::cmp,
            ),
            SortKey::Tags => present_first(joined(a), joined(b), descending, Ord::cmp),
            SortKey::Description => present_first(
                text(&a.description),
                text(&b.description),
                descending,
                |x, y| cmp_text(x, y),
            ),
        };
        order.then_with(|| by_name(a, b))
    });
}

/// What the Remote List and Ignored lists need besides their rows.
pub struct ListContext<'a> {
    /// The shared filters.
    pub matcher: &'a Matcher,
    pub source: SourceFilter,
    /// The tag filter, when one is typed and valid.
    pub tag_query: Option<&'a Query>,
    /// The Remote List lists ignored repos too ("Hide ignored" is off).
    pub show_ignored: bool,
    /// Tags of repos that aren't cloned, by repo ID.
    pub tags: &'a TagIndex,
    /// Repo IDs with a clone run in progress.
    pub cloning: &'a HashSet<i64>,
    /// The scanned clones, to open or run on the clones of the selected repos.
    pub clones: &'a [CloneInfo],
    /// Owners added by hand, listed above the Remote List.
    pub owners: &'a [String],
}

/// The Remote List or Ignored list: select rows, then Clone / Ignore / Un-ignore, open, run on
/// or tag them from the panel. `ignored_rows` are the ignored repos the Remote List shows when
/// "Hide ignored" is off.
pub fn repo_list(
    ui: &mut egui::Ui,
    kind: ListKind,
    (rows, ignored_rows): (&[RepoRow], &[RepoRow]),
    state: &mut ListState,
    cx: &ListContext<'_>,
    (panel, panel_cx): (&mut SelectionPanel, &PanelContext<'_>),
    action: &mut Option<ReposAction>,
) {
    let show_ignored = kind == ListKind::Remote && cx.show_ignored;
    let mut shown: Vec<&RepoRow> = rows
        .iter()
        .chain(if show_ignored { ignored_rows } else { &[] })
        .filter(|r| {
            cx.matcher.matches(&repo_item(r))
                && cx.source.matches(&r.sources)
                && cx
                    .tag_query
                    .is_none_or(|q| q.matches(&cx.tags.tags_of(r.id)))
        })
        .collect();
    sort_rows(&mut shown, state.sort, state.descending, cx.tags);
    let order: Vec<i64> = shown.iter().map(|r| r.id).collect();
    panel.update(&mut state.selection, &order);

    let ids = state.selection.in_order(&order);
    let targets = Targets::repos(ids.clone(), cx.clones);
    let any_ignored = ignored_rows.iter().any(|r| state.selection.contains(r.id));
    let extras = Extras {
        buttons: Some(Box::new(|ui, action| {
            *action = list_buttons(ui, kind, &ids, (show_ignored, any_ignored));
        })),
        ..Extras::default()
    };
    panel.show(
        ui,
        (&mut state.selection, &order),
        &targets,
        panel_cx,
        extras,
        action,
    );
    if kind == ListKind::Remote {
        manual_owners(ui, cx.owners, action);
    }

    if shown.is_empty() {
        ui.weak(match (kind, rows.is_empty()) {
            (ListKind::Remote, true) => "Nothing waiting: every listed repo is cloned or ignored.",
            (ListKind::Ignored, true) => "Nothing ignored.",
            (_, false) => "Nothing matches the filter.",
        });
        return;
    }

    ui.style_mut().interaction.selectable_labels = false;
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
    egui::ScrollArea::horizontal()
        .id_salt(("list_scroll", kind == ListKind::Remote))
        .auto_shrink(false)
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt(("repo_list", kind == ListKind::Remote))
                .striped(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(
                    Column::initial(280.0)
                        .at_least(140.0)
                        .clip(true)
                        .resizable(true),
                )
                .column(
                    Column::initial(150.0)
                        .at_least(80.0)
                        .clip(true)
                        .resizable(true),
                )
                .column(
                    Column::initial(100.0)
                        .at_least(60.0)
                        .clip(true)
                        .resizable(true),
                )
                .column(Column::initial(75.0).at_least(65.0))
                .column(Column::initial(100.0).at_least(90.0))
                .column(
                    Column::initial(150.0)
                        .at_least(60.0)
                        .clip(true)
                        .resizable(true),
                )
                .column(Column::remainder().at_least(120.0).clip(true))
                .header(24.0, |mut header| {
                    for (title, key) in [
                        ("Repository", SortKey::Name),
                        ("Sources", SortKey::Sources),
                        ("Language", SortKey::Language),
                        ("Stars", SortKey::Stars),
                        ("Last push", SortKey::Pushed),
                        ("Tags", SortKey::Tags),
                        ("Description", SortKey::Description),
                    ] {
                        header.col(|ui| state.header(ui, title, key));
                    }
                })
                .body(|body| {
                    body.rows(row_height, shown.len(), |mut row| {
                        let repo = shown[row.index()];
                        row.set_selected(state.selection.contains(repo.id));
                        row.col(|ui| {
                            name_cell(ui, repo, cx.cloning.contains(&repo.id), show_ignored);
                        });
                        row.col(|ui| {
                            let labels: Vec<&str> =
                                repo.sources.iter().map(|s| s.label()).collect();
                            ui.weak(labels.join(" · "));
                        });
                        row.col(|ui| {
                            ui.label(repo.language.as_deref().unwrap_or(""));
                        });
                        row.col(|ui| match repo.stars {
                            Some(stars) => {
                                ui.label(thousands(u64::from(stars)));
                            }
                            None => {
                                ui.weak("—")
                                    .on_hover_text("Sync with GitHub to fetch the star count");
                            }
                        });
                        row.col(|ui| {
                            let date = repo
                                .pushed_at
                                .as_deref()
                                .map_or("", |d| &d[..d.len().min(10)]);
                            ui.weak(date);
                        });
                        row.col(|ui| {
                            if let Some(tagging) = cx.tags.by_clone.get(&repo.id) {
                                tag_summary(ui, tagging);
                            }
                        });
                        row.col(|ui| {
                            let description = repo.description.as_deref().unwrap_or("");
                            ui.weak(description).on_hover_text(description);
                        });
                        state.selection.handle(&row.response(), &order, row.index());
                    });
                });
        });
}

/// Clone, Ignore and Un-ignore for the selected repos (`ids`). `show_ignored`: the Remote List
/// lists ignored repos too; `any_ignored`: some of the selected ones are.
fn list_buttons(
    ui: &mut egui::Ui,
    kind: ListKind,
    ids: &[i64],
    (show_ignored, any_ignored): (bool, bool),
) -> Option<ReposAction> {
    let count = ids.len();
    let set_ignored = |ignored| ReposAction::SetIgnored {
        repo_ids: ids.to_vec(),
        ignored,
    };
    let mut action = None;
    match kind {
        ListKind::Remote => {
            if ui
                .add_enabled(count > 0, egui::Button::new("Clone"))
                .on_hover_text("Clone over SSH into the clone root's layout")
                .clicked()
            {
                action = Some(ReposAction::Clone(ids.to_vec()));
            }
            if ui
                .add_enabled(count > 0, egui::Button::new("Ignore"))
                .on_hover_text("Move to the Ignored list; you can bring them back later")
                .clicked()
            {
                action = Some(set_ignored(true));
            }
            if show_ignored
                && ui
                    .add_enabled(any_ignored, egui::Button::new("Un-ignore"))
                    .on_disabled_hover_text("Select ignored repos first")
                    .clicked()
            {
                action = Some(set_ignored(false));
            }
        }
        ListKind::Ignored => {
            if ui
                .add_enabled(count > 0, egui::Button::new("Un-ignore"))
                .clicked()
            {
                action = Some(set_ignored(false));
            }
        }
    }
    action
}

/// `mark_ignored`: say so when the repo is ignored (where that isn't a given).
fn name_cell(ui: &mut egui::Ui, repo: &RepoRow, cloning: bool, mark_ignored: bool) {
    ui.label(repo.full_name());
    if cloning {
        ui.spinner();
        ui.weak("cloning");
    }
    if mark_ignored && repo.ignored {
        ui.weak("ignored")
            .on_hover_text("In the Ignored list. Un-ignore to keep it on the Remote List.");
    }
    if let Some((parent, decision)) = &repo.fork_of {
        let text = match decision {
            ForkDecision::Linked => "linked fork",
            ForkDecision::Separate => "separate fork",
        };
        ui.weak(text).on_hover_text(format!(
            "Fork of {parent} ({}). Change it in the Forks view.",
            decision.label().to_lowercase()
        ));
    }
    if repo.is_archived {
        ui.weak("archived");
    }
    if repo.is_private {
        ui.weak("private");
    }
}

/// The owners added by hand, each with a button to stop listing their repos. Shows nothing
/// when there are none.
fn manual_owners(ui: &mut egui::Ui, owners: &[String], action: &mut Option<ReposAction>) {
    if owners.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.weak("Owners added:");
        for owner in owners {
            ui.label(owner);
            if ui
                .small_button("×")
                .on_hover_text(format!("Stop listing every repo of {owner}"))
                .clicked()
            {
                *action = Some(ReposAction::RemoveManualOwner(owner.clone()));
            }
        }
    });
    ui.add_space(4.0);
}

/// The columns the Forks table can be sorted by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ForkSort {
    Fork,
    Upstream,
    Decision,
    Notes,
}

/// Which forks the Forks view lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ForkShow {
    All,
    Linked,
    Separate,
    Cloned,
    MissingUpstream,
    Chosen,
}

impl ForkShow {
    pub const ALL: [ForkShow; 6] = [
        ForkShow::All,
        ForkShow::Linked,
        ForkShow::Separate,
        ForkShow::Cloned,
        ForkShow::MissingUpstream,
        ForkShow::Chosen,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ForkShow::All => "All forks",
            ForkShow::Linked => "Linked forks",
            ForkShow::Separate => "Separate forks",
            ForkShow::Cloned => "Cloned forks",
            ForkShow::MissingUpstream => "Linked, but no upstream remote",
            ForkShow::Chosen => "Set by you",
        }
    }

    fn matches(self, fork: &ForkRow) -> bool {
        match self {
            ForkShow::All => true,
            ForkShow::Linked => fork.decision == ForkDecision::Linked,
            ForkShow::Separate => fork.decision == ForkDecision::Separate,
            ForkShow::Cloned => fork.fork_cloned,
            ForkShow::MissingUpstream => {
                fork.decision == ForkDecision::Linked
                    && fork.fork_cloned
                    && !fork.has_upstream_remote
            }
            ForkShow::Chosen => fork.chosen,
        }
    }
}

/// What the Forks view needs besides the catalog.
pub struct ForksContext<'a> {
    pub matcher: &'a Matcher,
    pub show: ForkShow,
    /// IDs of ignored repos.
    pub ignored: &'a HashSet<i64>,
    /// The scanned clones, to open or run on the clones of the selected forks.
    pub clones: &'a [CloneInfo],
}

/// The fork's side of a row: hiding and the language go by the fork. The search looks at the
/// upstream too.
fn fork_items<'a>(
    fork: &'a ForkRow,
    catalog: &'a Catalog,
    ignored: &HashSet<i64>,
) -> [Item<'a>; 2] {
    let item = |id: i64, full_name: &'a str| {
        let (owner, name) = match full_name.split_once('/') {
            Some((owner, name)) => (Some(owner), name),
            None => (None, full_name),
        };
        let summary = catalog.summaries.get(&id);
        Item {
            owner,
            name,
            folder: None,
            description: summary.and_then(|s| s.description.as_deref()),
            language: summary.and_then(|s| s.language.as_deref()),
            ignored: ignored.contains(&id),
            archived: summary.is_some_and(|s| s.is_archived),
        }
    };
    [
        item(fork.fork_id, &fork.fork_name),
        item(fork.parent_id, &fork.parent_name),
    ]
}

/// Selection (by fork ID) and sort order of the Forks table.
pub struct ForksState {
    pub selection: Selection,
    sort: ForkSort,
    descending: bool,
}

impl ForksState {
    pub fn new() -> Self {
        Self {
            selection: Selection::new(),
            sort: ForkSort::Fork,
            descending: false,
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, label: &str, key: ForkSort) {
        sort_header(ui, label, key, &mut self.sort, &mut self.descending, false);
    }
}

/// One entry in a fork's Notes column.
struct Note {
    text: &'static str,
    hover: &'static str,
    /// Something to fix rather than just to know.
    warn: bool,
}

impl Note {
    fn new(text: &'static str, hover: &'static str) -> Self {
        Self {
            text,
            hover,
            warn: false,
        }
    }
}

fn fork_notes(fork: &ForkRow) -> Vec<Note> {
    let mut notes = Vec::new();
    if fork.has_upstream_remote {
        notes.push(Note::new(
            "cloned with upstream remote",
            "A clone of the fork has a remote for the upstream.",
        ));
    } else if fork.fork_cloned {
        notes.push(match fork.decision {
            ForkDecision::Linked => Note {
                text: "cloned without upstream remote",
                hover: "Linked, but the fork's clone has no remote for the upstream. Add it \
                        from Problems, or make the fork Separate.",
                warn: true,
            },
            ForkDecision::Separate => Note::new(
                "cloned without upstream remote",
                "The fork's clone is set up on its own, with no remote for the upstream.",
            ),
        });
    }
    if fork.parent_cloned {
        notes.push(Note::new(
            "upstream cloned",
            "The upstream has a clone of its own.",
        ));
    }
    notes.push(if fork.parent_listed {
        Note::new(
            "upstream listed",
            "One of your sources lists the upstream too. While the fork is linked, the \
             upstream stays out of the Remote List.",
        )
    } else {
        Note::new(
            "upstream not in your sources",
            "No source lists the upstream; it's known only as this fork's parent.",
        )
    });
    notes.push(if fork.chosen {
        Note::new("set by you", "Syncs and scans won't change it.")
    } else {
        Note::new(
            "default",
            "Linked, unless the fork has a clone and none of its clones has a remote for \
             the upstream. Syncs and scans keep it current until you click the button.",
        )
    });
    notes
}

fn notes_text(notes: &[Note]) -> String {
    notes.iter().map(|n| n.text).collect::<Vec<_>>().join(" · ")
}

/// Sorts by `key`, ties by fork then upstream name.
fn sort_forks(rows: &mut [(&ForkRow, String)], key: ForkSort, descending: bool) {
    rows.sort_by(|(a, a_notes), (b, b_notes)| {
        let order = match key {
            ForkSort::Fork => cmp_text(&a.fork_name, &b.fork_name),
            ForkSort::Upstream => cmp_text(&a.parent_name, &b.parent_name),
            ForkSort::Decision => a.decision.as_str().cmp(b.decision.as_str()),
            ForkSort::Notes => cmp_text(a_notes, b_notes),
        };
        let order = if descending { order.reverse() } else { order };
        order
            .then_with(|| cmp_text(&a.fork_name, &b.fork_name))
            .then_with(|| cmp_text(&a.parent_name, &b.parent_name))
    });
}

/// Every fork with its upstream, and whether the two are linked. Selecting rows selects the
/// forks, to open, run on or tag their clones from the panel.
pub fn forks_view(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    cx: &ForksContext<'_>,
    state: &mut ForksState,
    (panel, panel_cx): (&mut SelectionPanel, &PanelContext<'_>),
    action: &mut Option<ReposAction>,
) {
    let mut shown: Vec<(&ForkRow, String)> = catalog
        .forks
        .iter()
        .filter(|f| {
            let [fork, parent] = fork_items(f, catalog, cx.ignored);
            cx.show.matches(f)
                && cx.matcher.keeps(&fork)
                && cx.matcher.text_matches(&[fork, parent])
        })
        .map(|f| (f, notes_text(&fork_notes(f))))
        .collect();
    sort_forks(&mut shown, state.sort, state.descending);
    let order: Vec<i64> = shown.iter().map(|(f, _)| f.fork_id).collect();
    panel.update(&mut state.selection, &order);
    let targets = Targets::repos(state.selection.in_order(&order), cx.clones);
    panel.show(
        ui,
        (&mut state.selection, &order),
        &targets,
        panel_cx,
        Extras::default(),
        action,
    );

    ui.weak(
        "Linked: a fork you contribute from. Its clone gets the upstream as a remote, and the \
         upstream leaves the Remote List. Separate: a hard fork; fork and upstream are handled \
         on their own, with no upstream remote. Forks are linked by default, unless your clone \
         of the fork has no remote for the upstream. Click a button to switch it.",
    );
    ui.add_space(4.0);

    if shown.is_empty() {
        ui.weak(if catalog.forks.is_empty() {
            "No forks yet. They show up here after a sync."
        } else {
            "Nothing matches the filter."
        });
        return;
    }

    ui.style_mut().interaction.selectable_labels = false;
    let row_height = ui.spacing().interact_size.y + 6.0;
    egui::ScrollArea::horizontal()
        .id_salt("forks_scroll")
        .auto_shrink(false)
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt("forks_table")
                .striped(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(
                    Column::initial(260.0)
                        .at_least(140.0)
                        .clip(true)
                        .resizable(true),
                )
                .column(
                    Column::initial(260.0)
                        .at_least(140.0)
                        .clip(true)
                        .resizable(true),
                )
                .column(Column::initial(125.0).at_least(115.0))
                .column(Column::remainder().at_least(160.0).clip(true))
                .header(24.0, |mut header| {
                    for (title, key) in [
                        ("Fork Repository", ForkSort::Fork),
                        ("Upstream Repository", ForkSort::Upstream),
                        ("Linked / Separate", ForkSort::Decision),
                        ("Notes", ForkSort::Notes),
                    ] {
                        header.col(|ui| state.header(ui, title, key));
                    }
                })
                .body(|body| {
                    body.rows(row_height, shown.len(), |mut row| {
                        let (fork, _) = shown[row.index()];
                        row.set_selected(state.selection.contains(fork.fork_id));
                        row.col(|ui| {
                            ui.label(&fork.fork_name);
                        });
                        row.col(|ui| {
                            ui.label(&fork.parent_name);
                        });
                        row.col(|ui| {
                            let other = fork.decision.toggled();
                            let hover = match fork.decision {
                                ForkDecision::Linked => {
                                    "Linked: the fork's clone gets the upstream as a remote, and \
                                     the upstream leaves the Remote List."
                                }
                                ForkDecision::Separate => {
                                    "Separate: fork and upstream are handled on their own."
                                }
                            };
                            let button = egui::Button::new(fork.decision.label())
                                .min_size(egui::vec2(80.0, 0.0));
                            if ui
                                .add(button)
                                .on_hover_text(format!(
                                    "{hover}\nClick to make it {}.",
                                    other.label()
                                ))
                                .clicked()
                            {
                                *action = Some(ReposAction::SetForkDecision {
                                    fork_id: fork.fork_id,
                                    parent_id: fork.parent_id,
                                    decision: other,
                                });
                            }
                        });
                        row.col(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for (i, note) in fork_notes(fork).into_iter().enumerate() {
                                if i > 0 {
                                    ui.weak(" · ");
                                }
                                let text = if note.warn {
                                    egui::RichText::new(note.text).color(ui.visuals().warn_fg_color)
                                } else {
                                    egui::RichText::new(note.text)
                                        .color(ui.visuals().weak_text_color())
                                };
                                ui.label(text).on_hover_text(note.hover);
                            }
                        });
                        state.selection.handle(&row.response(), &order, row.index());
                    });
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, owner: &str, name: &str) -> RepoRow {
        RepoRow {
            id,
            owner: owner.into(),
            name: name.into(),
            description: Some("A JavaScript library".into()),
            language: Some("Rust".into()),
            topics: vec!["cli".into()],
            is_fork: false,
            is_archived: false,
            is_private: false,
            pushed_at: Some("2026-10-01T00:00:00Z".into()),
            stars: Some(10),
            sources: vec![Source::Owned],
            fork_of: None,
            ignored: false,
        }
    }

    #[test]
    fn source_filter_groups_the_by_hand_sources() {
        let sources = [Source::Contributed, Source::ManualOwner];
        for (filter, expected) in [
            (SourceFilter::Any, true),
            (SourceFilter::Contributed, true),
            (SourceFilter::ByHand, true),
            (SourceFilter::Owned, false),
        ] {
            assert_eq!(filter.matches(&sources), expected, "{filter:?}");
        }
        assert!(SourceFilter::Any.matches(&[]));
        assert!(!SourceFilter::ByHand.matches(&[]));
    }

    #[test]
    fn fork_show_options() {
        let mut fork = fork_row(1, "me/a", "up/a", ForkDecision::Linked);
        let shown = |fork: &ForkRow| {
            ForkShow::ALL
                .into_iter()
                .filter(|show| show.matches(fork))
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(&fork), [ForkShow::All, ForkShow::Linked]);
        fork.fork_cloned = true;
        fork.chosen = true;
        assert_eq!(
            shown(&fork),
            [
                ForkShow::All,
                ForkShow::Linked,
                ForkShow::Cloned,
                ForkShow::MissingUpstream,
                ForkShow::Chosen
            ]
        );
        fork.has_upstream_remote = true;
        fork.decision = ForkDecision::Separate;
        fork.chosen = false;
        assert_eq!(
            shown(&fork),
            [ForkShow::All, ForkShow::Separate, ForkShow::Cloned]
        );
    }

    #[test]
    fn sorting_puts_missing_values_last_both_ways() {
        let mut big = row(1, "me", "big");
        big.stars = Some(5000);
        big.language = None;
        let mut unsynced = row(2, "Me", "Alpha");
        unsynced.stars = None;
        unsynced.language = Some("go".into());
        let small = row(3, "me", "small");
        let rows = [&big, &unsynced, &small];
        let order = |key, descending| {
            let mut sorted = rows.to_vec();
            sort_rows(&mut sorted, key, descending, &TagIndex::default());
            sorted.iter().map(|r| r.id).collect::<Vec<_>>()
        };
        assert_eq!(order(SortKey::Stars, true), [1, 3, 2]);
        assert_eq!(order(SortKey::Stars, false), [3, 1, 2]);
        assert_eq!(order(SortKey::Language, false), [2, 3, 1]);
        assert_eq!(order(SortKey::Language, true), [3, 2, 1]);
        // Case-insensitive, owner then name.
        assert_eq!(order(SortKey::Name, false), [2, 1, 3]);
        assert_eq!(order(SortKey::Name, true), [3, 1, 2]);
        // Equal descriptions fall back to the name.
        assert_eq!(order(SortKey::Description, true), [2, 1, 3]);
    }

    fn fork_row(id: i64, fork: &str, parent: &str, decision: ForkDecision) -> ForkRow {
        ForkRow {
            fork_id: id,
            fork_name: fork.into(),
            parent_id: 100 + id,
            parent_name: parent.into(),
            decision,
            chosen: false,
            fork_cloned: false,
            parent_cloned: false,
            parent_listed: true,
            has_upstream_remote: false,
        }
    }

    #[test]
    fn forks_sort_by_each_column_with_ties_by_name() {
        let mut unlisted = fork_row(1, "me/zed", "Acme/zed", ForkDecision::Linked);
        unlisted.parent_listed = false;
        let separate = fork_row(2, "Me/alpha", "other/alpha", ForkDecision::Separate);
        let mut chosen = fork_row(3, "me/beta", "acme/beta", ForkDecision::Linked);
        chosen.chosen = true;
        let rows = [&unlisted, &separate, &chosen];
        let order = |key, descending| {
            let mut sorted: Vec<_> = rows
                .iter()
                .map(|f| (*f, notes_text(&fork_notes(f))))
                .collect();
            sort_forks(&mut sorted, key, descending);
            sorted.iter().map(|(f, _)| f.fork_id).collect::<Vec<_>>()
        };
        assert_eq!(order(ForkSort::Fork, false), [2, 3, 1], "case-insensitive");
        assert_eq!(order(ForkSort::Fork, true), [1, 3, 2]);
        assert_eq!(order(ForkSort::Upstream, false), [3, 1, 2]);
        // Linked before Separate; ties by fork name in either direction.
        assert_eq!(order(ForkSort::Decision, false), [3, 1, 2]);
        assert_eq!(order(ForkSort::Decision, true), [2, 3, 1]);
        // "upstream listed · default" < "upstream listed · set by you" < "upstream not …"
        assert_eq!(order(ForkSort::Notes, false), [2, 3, 1]);
    }

    #[test]
    fn fork_notes_flag_a_linked_fork_whose_clone_lacks_the_upstream() {
        let mut fork = fork_row(1, "me/a", "up/a", ForkDecision::Linked);
        fork.fork_cloned = true;
        fork.chosen = true;
        let notes = fork_notes(&fork);
        assert_eq!(
            notes_text(&notes),
            "cloned without upstream remote · upstream listed · set by you"
        );
        assert!(notes[0].warn);
        fork.decision = ForkDecision::Separate;
        assert!(!fork_notes(&fork)[0].warn);
        fork.has_upstream_remote = true;
        fork.parent_cloned = true;
        fork.chosen = false;
        assert_eq!(
            notes_text(&fork_notes(&fork)),
            "cloned with upstream remote · upstream cloned · upstream listed · default"
        );
    }

    #[test]
    fn retain_drops_vanished_selections() {
        let mut state = ListState::new();
        state.selection.select_all(&[1, 2, 3]);
        state.retain(&[row(2, "a", "b")]);
        assert_eq!(state.selection.in_order(&[1, 2, 3]), [2]);
    }
}
