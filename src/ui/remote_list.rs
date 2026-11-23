//! The Remote List, Ignored and Forks views of the Repositories tab.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::db::{Catalog, ForkDecision, RepoRow};
use crate::tags::query::Query;
use crate::tags::{self, TagIndex};
use crate::ui::repos::ReposAction;
use crate::ui::tags::{TagsAction, tag_summary};
use crate::ui::{sort_header, thousands};

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

/// Selection and options for one list (Remote List or Ignored).
pub struct ListState {
    pub selected: HashSet<i64>,
    sort: SortKey,
    descending: bool,
    hide_archived: bool,
    /// The "Tag" field for tagging the selected repos.
    tag_name: String,
}

impl ListState {
    pub fn new() -> Self {
        Self {
            selected: HashSet::new(),
            sort: SortKey::Name,
            descending: false,
            hide_archived: false,
            tag_name: String::new(),
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
    pub fn retain(&mut self, rows: &[RepoRow]) {
        let present: HashSet<i64> = rows.iter().map(|r| r.id).collect();
        self.selected.retain(|id| present.contains(id));
    }
}

fn matches(row: &RepoRow, needle: &str) -> bool {
    needle.is_empty()
        || row.full_name().to_lowercase().contains(needle)
        || row
            .description
            .as_deref()
            .is_some_and(|d| d.to_lowercase().contains(needle))
        || row
            .language
            .as_deref()
            .is_some_and(|l| l.to_lowercase() == needle)
        || row.topics.iter().any(|t| t.to_lowercase() == needle)
        || row.sources.iter().any(|s| s.label() == needle)
}

/// Case-insensitive, without allocating.
fn cmp_text(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

fn by_name(a: &RepoRow, b: &RepoRow) -> Ordering {
    cmp_text(&a.owner, &b.owner).then_with(|| cmp_text(&a.name, &b.name))
}

/// Text worth sorting on: not missing and not blank.
fn text(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.trim().is_empty())
}

/// Orders present values with `cmp` (reversed when `descending`) and puts missing ones last
/// either way.
fn present_first<T>(
    a: Option<T>,
    b: Option<T>,
    descending: bool,
    cmp: impl FnOnce(&T, &T) -> Ordering,
) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            let order = cmp(&a, &b);
            if descending { order.reverse() } else { order }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
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
    /// The text filter.
    pub filter: &'a str,
    /// The tag filter, when one is typed and valid.
    pub tag_query: Option<&'a Query>,
    /// Tags of repos that aren't cloned, by repo ID.
    pub tags: &'a TagIndex,
    /// Repo IDs with a clone run in progress.
    pub cloning: &'a HashSet<i64>,
}

/// The Remote List or Ignored list: select rows, then Clone / Ignore / Un-ignore or tag them.
pub fn repo_list(
    ui: &mut egui::Ui,
    kind: ListKind,
    rows: &[RepoRow],
    state: &mut ListState,
    cx: &ListContext<'_>,
    action: &mut Option<ReposAction>,
) {
    let needle = cx.filter.trim().to_lowercase();
    let mut shown: Vec<&RepoRow> = rows
        .iter()
        .filter(|r| {
            matches(r, &needle)
                && !(state.hide_archived && r.is_archived)
                && cx
                    .tag_query
                    .is_none_or(|q| q.matches(&cx.tags.tags_of(r.id)))
        })
        .collect();
    sort_rows(&mut shown, state.sort, state.descending, cx.tags);

    ui.horizontal(|ui| {
        if ui
            .button(format!("Select all shown ({})", shown.len()))
            .clicked()
        {
            state.selected.extend(shown.iter().map(|r| r.id));
        }
        if ui
            .add_enabled(
                !state.selected.is_empty(),
                egui::Button::new("Clear selection"),
            )
            .clicked()
        {
            state.selected.clear();
        }
        ui.separator();
        let count = state.selected.len();
        ui.label(format!("{count} selected"));
        let ids = |state: &ListState| state.selected.iter().copied().collect::<Vec<i64>>();
        match kind {
            ListKind::Remote => {
                if ui
                    .add_enabled(count > 0, egui::Button::new("Clone"))
                    .on_hover_text("Clone over SSH into the clone root's layout")
                    .clicked()
                {
                    *action = Some(ReposAction::Clone(ids(state)));
                }
                if ui
                    .add_enabled(count > 0, egui::Button::new("Ignore"))
                    .on_hover_text("Move to the Ignored list; you can bring them back later")
                    .clicked()
                {
                    *action = Some(ReposAction::SetIgnored {
                        repo_ids: ids(state),
                        ignored: true,
                    });
                }
            }
            ListKind::Ignored => {
                if ui
                    .add_enabled(count > 0, egui::Button::new("Un-ignore"))
                    .clicked()
                {
                    *action = Some(ReposAction::SetIgnored {
                        repo_ids: ids(state),
                        ignored: false,
                    });
                }
            }
        }
        // These rows leave the list, so nothing stays selected.
        if action.is_some() {
            state.selected.clear();
        }

        // Tagging keeps the selection, so several tags can go on the same repos.
        ui.separator();
        let label = ui.label("Tag");
        let field = ui
            .add(
                egui::TextEdit::singleline(&mut state.tag_name)
                    .hint_text("tag name")
                    .desired_width(120.0),
            )
            .labelled_by(label.id);
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let valid = tags::normalize_tag(&state.tag_name);
        let enabled = count > 0 && valid.is_ok();
        let why_not = match &valid {
            _ if count == 0 => "Select repos first".to_owned(),
            Err(e) => e.clone(),
            Ok(_) => String::new(),
        };
        let add = ui
            .add_enabled(enabled, egui::Button::new("Add tag"))
            .on_hover_text("Tag the selected repos; a repo keeps its tags once it's cloned")
            .on_disabled_hover_text(&why_not)
            .clicked();
        if (add || entered) && enabled {
            *action = Some(ReposAction::Tags(TagsAction::AddTag {
                clone_ids: Vec::new(),
                repo_ids: ids(state),
                tag: std::mem::take(&mut state.tag_name),
            }));
        }
        if ui
            .add_enabled(enabled, egui::Button::new("Remove tag"))
            .on_hover_text("Removes manual tags only; tags from rules stay")
            .on_disabled_hover_text(&why_not)
            .clicked()
        {
            *action = Some(ReposAction::Tags(TagsAction::RemoveTag {
                clone_ids: Vec::new(),
                repo_ids: ids(state),
                tag: std::mem::take(&mut state.tag_name),
            }));
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.checkbox(&mut state.hide_archived, "Hide archived");
        });
    });
    ui.add_space(4.0);

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
                        let selected = state.selected.contains(&repo.id);
                        row.set_selected(selected);
                        row.col(|ui| name_cell(ui, repo, cx.cloning.contains(&repo.id)));
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
                        if row.response().clicked() {
                            if selected {
                                state.selected.remove(&repo.id);
                            } else {
                                state.selected.insert(repo.id);
                            }
                        }
                    });
                });
        });
}

fn name_cell(ui: &mut egui::Ui, repo: &RepoRow, cloning: bool) {
    ui.label(repo.full_name());
    if cloning {
        ui.spinner();
        ui.weak("cloning");
    }
    if let Some((parent, decision)) = &repo.fork_of {
        let text = match decision {
            ForkDecision::Undecided => "fork".to_owned(),
            ForkDecision::Linked => "linked fork".to_owned(),
            ForkDecision::Separate => "hard fork".to_owned(),
        };
        let color = if *decision == ForkDecision::Undecided {
            ui.visuals().warn_fg_color
        } else {
            ui.visuals().weak_text_color()
        };
        ui.colored_label(color, text).on_hover_text(format!(
            "Fork of {parent} ({}). Decide in the Forks view.",
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

/// "Add by hand" and the list of owners added that way.
pub fn add_by_hand(
    ui: &mut egui::Ui,
    text: &mut String,
    owners: &[String],
    action: &mut Option<ReposAction>,
) {
    ui.horizontal_wrapped(|ui| {
        let label = ui.label("Add by hand");
        let field = ui
            .add(
                egui::TextEdit::singleline(text)
                    .hint_text("owner/name, a GitHub URL, or an org or user")
                    .desired_width(300.0),
            )
            .labelled_by(label.id);
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let can_add = !text.trim().is_empty();
        if (ui.add_enabled(can_add, egui::Button::new("Add")).clicked() || entered) && can_add {
            *action = Some(ReposAction::AddManual(
                std::mem::take(text).trim().to_owned(),
            ));
        }
        if !owners.is_empty() {
            ui.separator();
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
        }
    });
}

/// Fork families and a decision for each fork.
pub fn forks_view(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    filter: &str,
    only_undecided: &mut bool,
    action: &mut Option<ReposAction>,
) {
    ui.weak(
        "Linked: a fork you contribute from. Its clone gets the parent as an upstream remote, \
         and the parent leaves the Remote List. Separate: a hard fork; fork and parent are handled \
         on their own, with no upstream remote.",
    );
    ui.checkbox(only_undecided, "Only undecided");
    ui.add_space(4.0);

    let needle = filter.trim().to_lowercase();
    let mut shown = 0;
    egui::ScrollArea::vertical()
        .id_salt("forks_scroll")
        .auto_shrink(false)
        .show(ui, |ui| {
            for family in &catalog.forks {
                let forks: Vec<_> = family
                    .forks
                    .iter()
                    .filter(|f| !*only_undecided || f.decision == ForkDecision::Undecided)
                    .filter(|f| {
                        needle.is_empty()
                            || f.name.to_lowercase().contains(&needle)
                            || family.parent_name.to_lowercase().contains(&needle)
                    })
                    .collect();
                if forks.is_empty() {
                    continue;
                }
                shown += 1;
                ui.horizontal(|ui| {
                    ui.strong(&family.parent_name);
                    if family.parent_cloned {
                        ui.weak("· cloned");
                    }
                    if family.parent_listed {
                        ui.weak("· listed");
                    } else {
                        ui.weak("· not in your sources");
                    }
                });
                ui.indent(("family", family.parent_id), |ui| {
                    for fork in forks {
                        ui.horizontal(|ui| {
                            ui.label(&fork.name);
                            if fork.cloned {
                                ui.weak("cloned");
                            }
                            let mut decision = fork.decision;
                            for option in ForkDecision::ALL {
                                ui.radio_value(&mut decision, option, option.label());
                            }
                            if decision != fork.decision {
                                *action = Some(ReposAction::SetForkDecision {
                                    fork_id: fork.fork_id,
                                    parent_id: family.parent_id,
                                    decision,
                                });
                            }
                            if fork.suggest_linked {
                                ui.weak("· suggested: Linked (a clone already has both remotes)");
                            }
                        });
                    }
                });
                ui.add_space(6.0);
            }
            if shown == 0 {
                ui.weak(if catalog.forks.is_empty() {
                    "No forks yet. They show up here after a sync."
                } else {
                    "Nothing matches."
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Source;

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
        }
    }

    #[test]
    fn filter_matches_name_description_language_topic_and_source() {
        let repo = row(1, "me", "thing");
        for needle in ["", "me/th", "javascript", "rust", "cli", "owned"] {
            assert!(matches(&repo, needle), "{needle}");
        }
        for needle in ["rus", "cl", "org"] {
            assert!(!matches(&repo, needle), "{needle}");
        }
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

    #[test]
    fn retain_drops_vanished_selections() {
        let mut state = ListState::new();
        state.selected.extend([1, 2, 3]);
        state.retain(&[row(2, "a", "b")]);
        assert_eq!(state.selected, HashSet::from([2]));
    }
}
