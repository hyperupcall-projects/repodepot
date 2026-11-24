//! Selecting rows in the Repositories views, and the bar above each list for acting on the
//! selection.
//!
//! A click selects just the row clicked. Ctrl-click (Cmd-click on macOS) adds a row to the
//! selection or takes it out, and Shift-click selects every row from the one last clicked to
//! this one; with Ctrl (Cmd) held too, it adds that range to what's already selected.

use std::collections::{HashMap, HashSet};

use eframe::egui;

use crate::backend::LauncherInfo;
use crate::git::scan::CloneInfo;
use crate::launch;
use crate::tags;
use crate::ui::filter::keep_together;
use crate::ui::plural;
use crate::ui::repos::ReposAction;
use crate::ui::tags::TagsAction;

/// The selected rows of one view, by ID (a clone's or a repo's, depending on the view).
#[derive(Debug, Default)]
pub struct Selection {
    ids: HashSet<i64>,
    /// Where Shift-click ranges start: the row last clicked without Shift.
    anchor: Option<i64>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, id: i64) -> bool {
        self.ids.contains(&id)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn clear(&mut self) {
        self.ids.clear();
        self.anchor = None;
    }

    pub fn select_only(&mut self, id: i64) {
        self.ids.clear();
        self.ids.insert(id);
        self.anchor = Some(id);
    }

    pub fn select_all(&mut self, ids: &[i64]) {
        self.ids.extend(ids.iter().copied());
    }

    /// The selected ID, when exactly one row is selected.
    pub fn single(&self) -> Option<i64> {
        match self.ids.len() {
            1 => self.ids.iter().next().copied(),
            _ => None,
        }
    }

    /// Keeps only the rows in `shown`, so what's selected is always on screen and an action
    /// never reaches a row the filters hide.
    pub fn retain(&mut self, shown: &[i64]) {
        if self.is_empty() {
            return;
        }
        let shown: HashSet<i64> = shown.iter().copied().collect();
        self.ids.retain(|id| shown.contains(id));
    }

    /// The selected IDs in the order of `order` (the rows top to bottom), each once.
    pub fn in_order(&self, order: &[i64]) -> Vec<i64> {
        let mut seen = HashSet::new();
        order
            .iter()
            .copied()
            .filter(|id| self.ids.contains(id) && seen.insert(*id))
            .collect()
    }

    /// A click on row `index` of `order`, the IDs of the rows shown from top to bottom. (An ID
    /// can be in `order` more than once, as a clone with two problems is in Problems.)
    pub fn click(&mut self, order: &[i64], index: usize, modifiers: egui::Modifiers) {
        let Some(&id) = order.get(index) else { return };
        // Ctrl on Linux and Windows, Cmd on macOS.
        let adding = modifiers.command;
        if modifiers.shift
            && let Some(start) = self.anchor.and_then(|anchor| nearest(order, anchor, index))
        {
            if !adding {
                self.ids.clear();
            }
            let (first, last) = (start.min(index), start.max(index));
            self.ids.extend(order[first..=last].iter().copied());
            // The anchor stays, so another Shift-click changes the range from the same row.
            return;
        }
        if adding {
            if !self.ids.remove(&id) {
                self.ids.insert(id);
            }
        } else {
            self.ids.clear();
            self.ids.insert(id);
        }
        self.anchor = Some(id);
    }

    /// Applies a click on the row at `index`, if `response` was clicked.
    pub fn handle(&mut self, response: &egui::Response, order: &[i64], index: usize) {
        if response.clicked() {
            // The keys held at the click itself, rather than by the end of the frame.
            let modifiers = response.ctx.input(|i| {
                i.events
                    .iter()
                    .rev()
                    .find_map(|event| match event {
                        egui::Event::PointerButton {
                            button: egui::PointerButton::Primary,
                            modifiers,
                            ..
                        } => Some(*modifiers),
                        _ => None,
                    })
                    .unwrap_or(i.modifiers)
            });
            self.click(order, index, modifiers);
        }
    }
}

/// Where `id` is in `order`, taking the place closest to `index` if it's there more than once.
fn nearest(order: &[i64], id: i64, index: usize) -> Option<usize> {
    order
        .iter()
        .enumerate()
        .filter(|(_, other)| **other == id)
        .map(|(i, _)| i)
        .min_by_key(|i| i.abs_diff(index))
}

/// A row outside a table that can be selected by clicking it: the row's own buttons still get
/// their clicks, and a click anywhere else on the row goes to the returned response.
pub fn selectable_row(
    ui: &mut egui::Ui,
    selected: bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    // Painted once the row's size is known, but underneath it.
    let background = ui.painter().add(egui::Shape::Noop);
    let response = ui
        .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
            ui.horizontal(|ui| {
                ui.set_min_width(ui.available_width());
                if selected {
                    // Like a selected table row.
                    let color = ui.visuals().selection.stroke.color;
                    ui.visuals_mut().override_text_color = Some(color);
                }
                add_contents(ui);
            });
        })
        .response;
    let visuals = ui.visuals();
    let fill = if selected {
        Some(visuals.selection.bg_fill)
    } else if response.contains_pointer() {
        Some(visuals.widgets.hovered.weak_bg_fill)
    } else {
        None
    };
    if let Some(fill) = fill {
        let rect = response.rect.expand2(0.5 * ui.spacing().item_spacing);
        ui.painter()
            .set(background, egui::Shape::rect_filled(rect, 2.0, fill));
    }
    response
}

/// What the selected rows stand for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Targets {
    /// The clones to open or run a command in: the selected rows in Local Clones and Problems,
    /// and the clones of the selected repos in the views that list repos.
    pub clone_ids: Vec<i64>,
    /// The selected repos, in the views that list repos (Remote List, Ignored, Forks).
    pub repo_ids: Vec<i64>,
    /// Selected repos without a clone, which can be tagged but not opened or run on.
    pub not_cloned: usize,
}

impl Targets {
    /// The rows are clones.
    pub fn clones(clone_ids: Vec<i64>) -> Self {
        Self {
            clone_ids,
            ..Self::default()
        }
    }

    /// The rows are repos; their clones are the ones whose main remote is the repo.
    pub fn repos(repo_ids: Vec<i64>, clones: &[CloneInfo]) -> Self {
        if repo_ids.is_empty() {
            return Self::default();
        }
        let mut by_repo: HashMap<i64, Vec<i64>> = HashMap::new();
        for clone in clones {
            if let Some(repo) = clone.repo_id {
                by_repo.entry(repo).or_default().push(clone.id);
            }
        }
        let mut clone_ids = Vec::new();
        let mut not_cloned = 0;
        for repo in &repo_ids {
            match by_repo.get(repo) {
                Some(ids) => clone_ids.extend(ids),
                None => not_cloned += 1,
            }
        }
        Self {
            clone_ids,
            repo_ids,
            not_cloned,
        }
    }

    fn rows_are_repos(&self) -> bool {
        !self.repo_ids.is_empty()
    }
}

/// What the bar needs to know besides the selection.
pub struct BarContext<'a> {
    pub launchers: &'a [LauncherInfo],
    /// Ask before opening more windows than this.
    pub confirm_over: usize,
    /// What the rows are: "clones" or "repos".
    pub rows: &'static str,
    /// The tag query, when it's the only filter narrowing the view. If every row shown is
    /// selected, the selection is that tag group: runs and workspace files are named after it.
    pub tag_group: Option<&'a str>,
    /// The view's title, to name a run when there's no group.
    pub view: &'a str,
}

/// The bar above each list: select all or none, the view's own buttons, then opening,
/// running a command in, and tagging whatever is selected.
pub struct ActionBar {
    /// The "Tag" field.
    pub tag_name: String,
    /// "Open each" over the limit, waiting for a yes: (launcher, clone IDs).
    pending_open: Option<(String, Vec<i64>)>,
}

impl ActionBar {
    pub fn new() -> Self {
        Self {
            tag_name: String::new(),
            pending_open: None,
        }
    }

    /// `shown` is the IDs of the rows shown, top to bottom; `view_buttons` adds the view's own
    /// buttons (Clone, Ignore, …), and the selection is cleared when one of them is used,
    /// since those rows leave the list.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        (selection, shown): (&mut Selection, &[i64]),
        targets: &Targets,
        cx: &BarContext<'_>,
        view_buttons: impl FnOnce(&mut egui::Ui) -> Option<ReposAction>,
        action: &mut Option<ReposAction>,
    ) {
        let count = selection.len();
        let unique = shown.iter().copied().collect::<HashSet<i64>>().len();
        let group = cx.tag_group.filter(|_| count > 0 && count == unique);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    count < unique,
                    egui::Button::new(format!("Select all shown ({unique})")),
                )
                .on_hover_text(
                    "Click a row to select it alone, Ctrl-click to add or remove one, \
                     Shift-click to select a range",
                )
                .clicked()
            {
                selection.select_all(shown);
            }
            if ui
                .add_enabled(count > 0, egui::Button::new("Clear selection"))
                .clicked()
            {
                selection.clear();
            }
            ui.separator();
            ui.label(format!("{count} selected"));

            if let Some(view_action) = view_buttons(ui) {
                *action = Some(view_action);
                selection.clear();
            }

            ui.separator();
            keep_together(ui, 330.0);
            self.open_and_run(ui, targets, (count, group), cx, action);

            ui.separator();
            keep_together(ui, 340.0);
            self.tag(ui, targets, count, cx.rows, action);
        });
        if let Some((launcher, ids)) = self.pending_open.clone() {
            ui.horizontal(|ui| {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Open {} windows in {launcher}?", ids.len()),
                );
                if ui.button("Open them").clicked() {
                    *action = Some(ReposAction::Open {
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

    /// Open each, Open together and Run command, for the selected clones.
    fn open_and_run(
        &mut self,
        ui: &mut egui::Ui,
        targets: &Targets,
        (count, group): (usize, Option<&str>),
        cx: &BarContext<'_>,
        action: &mut Option<ReposAction>,
    ) {
        let clones = &targets.clone_ids;
        let many = cx.rows;
        // Why nothing can be opened or run, if so.
        let blocked = if count == 0 {
            Some(format!("Select {many} first"))
        } else if clones.is_empty() {
            Some(if count == 1 {
                "It isn't cloned".to_owned()
            } else {
                "None of them is cloned".to_owned()
            })
        } else {
            None
        };
        let each: Vec<&LauncherInfo> = cx.launchers.iter().filter(|l| l.each).collect();
        let together: Vec<&LauncherInfo> = cx.launchers.iter().filter(|l| l.together).collect();
        let no_launchers = "No editors found. Add one in Settings.";

        let why_not = blocked
            .clone()
            .or_else(|| each.is_empty().then(|| no_launchers.to_owned()));
        ui.add_enabled_ui(why_not.is_none(), |ui| {
            ui.menu_button("Open each", |ui| {
                for launcher in &each {
                    if ui.button(&launcher.name).clicked() {
                        if clones.len() > cx.confirm_over {
                            self.pending_open = Some((launcher.name.clone(), clones.clone()));
                        } else {
                            *action = Some(ReposAction::Open {
                                launcher: launcher.name.clone(),
                                together: false,
                                clone_ids: clones.clone(),
                                workspace_name: String::new(),
                            });
                        }
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("One window per clone")
            .on_disabled_hover_text(why_not.unwrap_or_default());
        });

        let why_not = blocked.clone().or_else(|| {
            together
                .is_empty()
                .then(|| "No app found that opens several folders in one window".to_owned())
        });
        ui.add_enabled_ui(why_not.is_none(), |ui| {
            ui.menu_button("Open together", |ui| {
                for launcher in &together {
                    if ui.button(&launcher.name).clicked() {
                        let workspace_name = match group {
                            Some(group) => group.to_owned(),
                            None => launch::set_name(clones),
                        };
                        *action = Some(ReposAction::Open {
                            launcher: launcher.name.clone(),
                            together: true,
                            clone_ids: clones.clone(),
                            workspace_name,
                        });
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("Every clone in one window, for apps that support workspaces")
            .on_disabled_hover_text(why_not.unwrap_or_default());
        });

        if ui
            .add_enabled(blocked.is_none(), egui::Button::new("Run command…"))
            .on_hover_text(format!(
                "Open the Runner with the {}",
                plural(clones.len(), "clone", "clones")
            ))
            .on_disabled_hover_text(blocked.unwrap_or_default())
            .clicked()
        {
            let label = match group {
                Some(group) => group.to_owned(),
                None => format!("{} selection", cx.view),
            };
            *action = Some(ReposAction::RunOn {
                label,
                clone_ids: clones.clone(),
            });
        }
        if targets.not_cloned > 0 && !clones.is_empty() {
            ui.weak(format!("· {} not cloned", targets.not_cloned))
                .on_hover_text("Repos that aren't cloned are left out of opening and running");
        }
    }

    /// The tag field, with Add tag and Remove tag for the selected rows.
    fn tag(
        &mut self,
        ui: &mut egui::Ui,
        targets: &Targets,
        count: usize,
        many: &str,
        action: &mut Option<ReposAction>,
    ) {
        let label = ui.label("Tag");
        let field = ui
            .add(
                egui::TextEdit::singleline(&mut self.tag_name)
                    .hint_text("tag name")
                    .desired_width(120.0),
            )
            .labelled_by(label.id);
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let valid = tags::normalize_tag(&self.tag_name);
        let enabled = count > 0 && valid.is_ok();
        let why_not = match &valid {
            _ if count == 0 => format!("Select {many} first"),
            Err(e) => e.clone(),
            Ok(_) => String::new(),
        };
        let add_hover = if targets.rows_are_repos() {
            "Tag the selected repos; a repo keeps its tags once it's cloned"
        } else {
            "Tag the selected clones"
        };
        // Tagging keeps the selection, so several tags can go on the same rows.
        let add = ui
            .add_enabled(enabled, egui::Button::new("Add tag"))
            .on_hover_text(add_hover)
            .on_disabled_hover_text(&why_not)
            .clicked();
        if (add || entered) && enabled {
            // A clone shows its repo's tags as its own, so tagging a repo tags its clones.
            let clone_ids = if targets.rows_are_repos() {
                Vec::new()
            } else {
                targets.clone_ids.clone()
            };
            *action = Some(ReposAction::Tags(TagsAction::AddTag {
                clone_ids,
                repo_ids: targets.repo_ids.clone(),
                tag: std::mem::take(&mut self.tag_name),
            }));
        }
        if ui
            .add_enabled(enabled, egui::Button::new("Remove tag"))
            .on_hover_text("Removes manual tags only; tags from rules stay")
            .on_disabled_hover_text(&why_not)
            .clicked()
        {
            *action = Some(ReposAction::Tags(TagsAction::RemoveTag {
                clone_ids: targets.clone_ids.clone(),
                repo_ids: targets.repo_ids.clone(),
                tag: std::mem::take(&mut self.tag_name),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: egui::Modifiers = egui::Modifiers::NONE;
    const SHIFT: egui::Modifiers = egui::Modifiers::SHIFT;
    const COMMAND: egui::Modifiers = egui::Modifiers::COMMAND;

    fn selected(selection: &Selection, order: &[i64]) -> Vec<i64> {
        selection.in_order(order)
    }

    #[test]
    fn a_click_selects_only_that_row() {
        let order = [10, 20, 30, 40];
        let mut s = Selection::new();
        s.click(&order, 1, PLAIN);
        assert_eq!(selected(&s, &order), [20]);
        s.click(&order, 3, PLAIN);
        assert_eq!(selected(&s, &order), [40]);
        // Clicking the selected row again keeps it selected.
        s.click(&order, 3, PLAIN);
        assert_eq!(selected(&s, &order), [40]);
        assert_eq!(s.single(), Some(40));
    }

    #[test]
    fn command_click_adds_and_removes() {
        let order = [10, 20, 30, 40];
        let mut s = Selection::new();
        s.click(&order, 0, PLAIN);
        s.click(&order, 2, COMMAND);
        assert_eq!(selected(&s, &order), [10, 30]);
        assert_eq!(s.single(), None);
        s.click(&order, 0, COMMAND);
        assert_eq!(selected(&s, &order), [30]);
    }

    #[test]
    fn shift_click_selects_a_range_from_the_anchor() {
        let order = [10, 20, 30, 40, 50];
        let mut s = Selection::new();
        s.click(&order, 1, PLAIN);
        s.click(&order, 3, SHIFT);
        assert_eq!(selected(&s, &order), [20, 30, 40]);
        // Again from the same anchor, upwards this time; the old range goes.
        s.click(&order, 0, SHIFT);
        assert_eq!(selected(&s, &order), [10, 20]);
        // Ctrl-click moves the anchor; Ctrl+Shift adds a range to the selection.
        s.click(&order, 4, COMMAND);
        s.click(&order, 3, SHIFT | COMMAND);
        assert_eq!(selected(&s, &order), [10, 20, 40, 50]);
    }

    #[test]
    fn shift_click_without_an_anchor_on_screen_selects_one_row() {
        let order = [10, 20, 30];
        let mut s = Selection::new();
        s.click(&order, 1, SHIFT);
        assert_eq!(selected(&s, &order), [20]);
        // The anchor was filtered out of view.
        s.click(&[99, 10, 20, 30], 0, PLAIN);
        s.click(&order, 2, SHIFT);
        assert_eq!(selected(&s, &order), [30]);
    }

    #[test]
    fn ranges_use_the_nearest_copy_of_a_repeated_row() {
        // Clone 7 has two problems, so Problems lists it twice.
        let order = [7, 1, 2, 7, 3];
        let mut s = Selection::new();
        s.click(&order, 3, PLAIN);
        s.click(&order, 4, SHIFT);
        assert_eq!(selected(&s, &order), [7, 3]);
    }

    #[test]
    fn retain_drops_rows_no_longer_shown() {
        let mut s = Selection::new();
        s.select_all(&[1, 2, 3]);
        s.retain(&[2, 3, 4]);
        assert_eq!(selected(&s, &[1, 2, 3, 4]), [2, 3]);
    }

    #[test]
    fn targets_of_repos_are_their_clones() {
        use crate::git::scan::MainRemote;
        let clone = |id: i64, repo_id: Option<i64>| CloneInfo {
            id,
            repo_id,
            rel_path: format!("x/{id}"),
            abs_path: format!("/code/x/{id}").into(),
            remotes: Vec::new(),
            main: MainRemote::NoRemotes,
            status: Ok(Default::default()),
            problems: Vec::new(),
        };
        let clones = [clone(1, Some(10)), clone(2, Some(20)), clone(3, Some(20))];
        assert_eq!(
            Targets::repos(vec![20, 30, 10], &clones),
            Targets {
                clone_ids: vec![2, 3, 1],
                repo_ids: vec![20, 30, 10],
                not_cloned: 1,
            }
        );
        assert_eq!(Targets::repos(Vec::new(), &clones), Targets::default());
    }
}
