//! Selecting rows in the Repositories views, and the panel on the right for acting on the
//! selection.
//!
//! A click selects just the row clicked. Ctrl-click (Cmd-click on macOS) adds a row to the
//! selection or takes it out, and Shift-click selects every row from the one last clicked to
//! this one; with Ctrl (Cmd) held too, it adds that range to what's already selected. Ctrl+A
//! selects every row shown, and Escape deselects them all.

use std::collections::{HashMap, HashSet};

use eframe::egui;

use crate::backend::LauncherInfo;
use crate::git::scan::CloneInfo;
use crate::launch;
use crate::tags;
use crate::ui::repos::ReposAction;
use crate::ui::tags::TagsAction;
use crate::ui::{plural, thousands};

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

/// What a view's rows are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rows {
    Clones,
    Repos,
}

impl Rows {
    /// `"1 clone"`, `"3 repos"`.
    fn count(self, n: usize) -> String {
        match self {
            Rows::Clones => plural(n, "clone", "clones"),
            Rows::Repos => plural(n, "repo", "repos"),
        }
    }

    fn many(self) -> &'static str {
        match self {
            Rows::Clones => "clones",
            Rows::Repos => "repos",
        }
    }
}

/// What the panel needs to know besides the selection.
pub struct PanelContext<'a> {
    pub launchers: &'a [LauncherInfo],
    /// Ask before opening more windows than this.
    pub confirm_over: usize,
    pub rows: Rows,
    /// The tag query, when it's the only filter narrowing the view. If every row shown is
    /// selected, the selection is that tag group: runs and workspace files are named after it.
    pub tag_group: Option<&'a str>,
    /// The view's title, to name a run when there's no group.
    pub view: &'a str,
}

/// Fills one part of the panel, and sets the action when one of its buttons is used.
pub type Part<'a> = Box<dyn FnOnce(&mut egui::Ui, &mut Option<ReposAction>) + 'a>;

/// What a view adds to the panel besides opening, running and tagging.
#[derive(Default)]
pub struct Extras<'a> {
    /// The view's own buttons (Clone, Ignore, …), first under Actions. Using one clears the
    /// selection, since those rows leave the list.
    pub buttons: Option<Part<'a>>,
    /// Under Tags, above the Tag field: the tags of the one row selected.
    pub tags: Option<Part<'a>>,
    /// At the bottom: more about the one row selected.
    pub details: Option<Part<'a>>,
}

/// A key that changes the whole selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SelectionKey {
    /// Ctrl+A (Cmd+A on macOS): every row shown.
    All,
    /// Escape: none.
    None,
}

const SELECT_ALL: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::A);
const DESELECT: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Escape);

/// The panel on the right of a Repositories view: what's selected, and everything that can be
/// done with it. It can be hidden and shown again without touching the selection.
pub struct SelectionPanel {
    /// Shown on the right of the view.
    pub open: bool,
    /// The "Tag" field.
    pub tag_name: String,
    /// "Open each" over the limit, waiting for a yes: (launcher, clone IDs).
    pending_open: Option<(String, Vec<i64>)>,
    /// Read by `read_keys` at the start of the frame, applied by `update`.
    key: Option<SelectionKey>,
    /// The text field that had the keyboard when the page was last drawn.
    typing_in: Option<egui::Id>,
}

impl SelectionPanel {
    pub fn new() -> Self {
        Self {
            open: true,
            tag_name: String::new(),
            pending_open: None,
            key: None,
            typing_in: None,
        }
    }

    /// Reads Ctrl+A and Escape for `update` to apply. Call it before anything else is drawn,
    /// so that a text field that had the keyboard keeps them (Ctrl+A selects its text, Escape
    /// leaves it), and an open menu keeps Escape to close itself.
    pub fn read_keys(&mut self, ui: &egui::Ui) {
        let ctx = ui.ctx();
        self.key = None;
        // egui takes the keyboard away from a field on Escape before anything is drawn, so
        // the field is found by what had it at the end of the last frame.
        let typing = ctx.text_edit_focused()
            || self
                .typing_in
                .is_some_and(|id| ctx.memory(|m| m.had_focus_last_frame(id)));
        if typing {
            return;
        }
        let popup_open = egui::Popup::is_any_open(ctx);
        self.key = ui.input_mut(|i| {
            if i.consume_shortcut(&SELECT_ALL) {
                Some(SelectionKey::All)
            } else if !popup_open && i.consume_shortcut(&DESELECT) {
                Some(SelectionKey::None)
            } else {
                None
            }
        });
    }

    /// Call at the end of the frame: notes which text field has the keyboard, for `read_keys`.
    pub fn end_frame(&mut self, ui: &egui::Ui) {
        let ctx = ui.ctx();
        self.typing_in = ctx
            .memory(|m| m.focused())
            .filter(|_| ctx.text_edit_focused());
    }

    /// Keeps only the rows shown (`shown`, top to bottom), then applies Ctrl+A or Escape.
    pub fn update(&mut self, selection: &mut Selection, shown: &[i64]) {
        selection.retain(shown);
        match self.key.take() {
            Some(SelectionKey::All) => selection.select_all(shown),
            Some(SelectionKey::None) => selection.clear(),
            None => {}
        }
    }

    /// The button that shows or hides the panel, with how many rows are selected.
    pub fn toggle(&mut self, ui: &mut egui::Ui, count: usize) {
        ui.toggle_value(
            &mut self.open,
            format!("Selection ({})", thousands(count as u64)),
        )
        .on_hover_text(if self.open {
            "Hide the panel with the selection and its actions. The selection stays."
        } else {
            "Show the panel with the selection and its actions"
        });
    }

    /// The panel, on the right of `ui`, when it's open. Call it before drawing the list, which
    /// gets the rest of the width. `shown` is the IDs of the rows shown, top to bottom.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        (selection, shown): (&mut Selection, &[i64]),
        targets: &Targets,
        cx: &PanelContext<'_>,
        extras: Extras<'_>,
        action: &mut Option<ReposAction>,
    ) {
        let mut open = self.open;
        let mut hide = false;
        egui::Panel::right("selection_panel")
            .resizable(true)
            .default_size(340.0)
            .min_size(260.0)
            .show_collapsible(ui, &mut open, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("selection_panel_scroll")
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        hide = self.contents(ui, (selection, shown), targets, cx, extras, action);
                    });
            });
        // Dragging the edge past the narrowest width hides it too.
        self.open = open && !hide;
    }

    /// Returns true when Hide was clicked.
    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        (selection, shown): (&mut Selection, &[i64]),
        targets: &Targets,
        cx: &PanelContext<'_>,
        extras: Extras<'_>,
        action: &mut Option<ReposAction>,
    ) -> bool {
        let count = selection.len();
        let unique = shown.iter().copied().collect::<HashSet<i64>>().len();
        let group = cx.tag_group.filter(|_| count > 0 && count == unique);
        let shortcut = |shortcut| ui.ctx().format_shortcut(&shortcut);
        let (select_all_key, deselect_key) = (shortcut(SELECT_ALL), shortcut(DESELECT));

        let mut hide = false;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading(match count {
                0 => "Nothing selected".to_owned(),
                n => format!("{} selected", cx.rows.count(n)),
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                hide = ui
                    .button("Hide")
                    .on_hover_text(
                        "Hide this panel; the selection stays. The Selection button above the \
                         list shows it again.",
                    )
                    .clicked();
            });
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    count < unique,
                    egui::Button::new(format!("Select all shown ({})", thousands(unique as u64))),
                )
                .on_hover_text(&select_all_key)
                .clicked()
            {
                selection.select_all(shown);
            }
            if ui
                .add_enabled(count > 0, egui::Button::new("Deselect"))
                .on_hover_text(&deselect_key)
                .clicked()
            {
                selection.clear();
            }
        });
        if count == 0 {
            ui.weak(format!(
                "Click a row to select it, Ctrl-click to add or remove one, Shift-click to \
                 select a range. {select_all_key} selects every row shown, {deselect_key} \
                 deselects."
            ));
        }

        section(ui, "Actions");
        if let Some(buttons) = extras.buttons {
            let mut view_action = None;
            ui.horizontal_wrapped(|ui| buttons(ui, &mut view_action));
            if let Some(view_action) = view_action {
                *action = Some(view_action);
                selection.clear();
            }
        }
        ui.horizontal_wrapped(|ui| self.open_and_run(ui, targets, (count, group), cx, action));
        self.confirm_open(ui, action);

        section(ui, "Tags");
        if let Some(tags) = extras.tags {
            tags(ui, action);
            ui.add_space(4.0);
        }
        ui.horizontal_wrapped(|ui| self.tag(ui, targets, count, cx.rows, action));

        if let Some(details) = extras.details {
            ui.add_space(8.0);
            ui.separator();
            details(ui, action);
        }
        hide
    }

    /// Open each, Open together and Run command, for the selected clones.
    fn open_and_run(
        &mut self,
        ui: &mut egui::Ui,
        targets: &Targets,
        (count, group): (usize, Option<&str>),
        cx: &PanelContext<'_>,
        action: &mut Option<ReposAction>,
    ) {
        let clones = &targets.clone_ids;
        let many = cx.rows.many();
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

    /// "Open each" over the limit asks first.
    fn confirm_open(&mut self, ui: &mut egui::Ui, action: &mut Option<ReposAction>) {
        let Some((launcher, ids)) = self.pending_open.clone() else {
            return;
        };
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("Open {} windows in {launcher}?", ids.len()),
        );
        ui.horizontal(|ui| {
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

    /// The tag field, with Add tag and Remove tag for the selected rows.
    fn tag(
        &mut self,
        ui: &mut egui::Ui,
        targets: &Targets,
        count: usize,
        rows: Rows,
        action: &mut Option<ReposAction>,
    ) {
        let label = ui.label("Tag");
        let field = ui
            .add(
                egui::TextEdit::singleline(&mut self.tag_name)
                    .hint_text("work, lang:rust, …")
                    .desired_width(130.0),
            )
            .labelled_by(label.id);
        let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let valid = tags::normalize_tag(&self.tag_name);
        let enabled = count > 0 && valid.is_ok();
        let why_not = match &valid {
            _ if count == 0 => format!("Select {} first", rows.many()),
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

/// A part of the panel, under a rule.
fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.separator();
    ui.strong(title);
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
