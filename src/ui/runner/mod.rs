//! The Runner tab: run a shell command in many clones and work through the output.

mod tab;
mod text;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use crate::config::Config;
use crate::db::{HistoryEntry, Snippet};
use crate::git::scan::CloneInfo;
use crate::jobs::{Run, Target};
use crate::tags::TagIndex;
use tab::{RunTab, TabAction};

#[derive(Debug, PartialEq, Eq)]
pub enum RunnerAction {
    Start {
        /// Comes back with the run, so it lands in the right tab.
        request: u64,
        command: String,
        targets: Vec<Target>,
        parallel: usize,
        colors: bool,
    },
    SaveSnippet {
        name: String,
        command: String,
    },
    DeleteSnippet(i64),
}

pub struct RunnerContext<'a> {
    /// The latest scan's clones, if there is one.
    pub clones: Option<&'a [CloneInfo]>,
    pub config: &'a Config,
    /// For each target's `DEPO_TAGS`.
    pub tags: &'a TagIndex,
}

/// Where a requested run's results go.
enum Destination {
    NewTab {
        breadcrumb: Option<String>,
    },
    /// Replace these rows of an existing tab (re-run).
    Replace {
        tab: u64,
        rows: Vec<usize>,
    },
}

pub struct RunnerPage {
    command: String,
    filter: String,
    /// Overrides the setting for the next run.
    parallel: Option<u32>,
    colors: bool,
    snippets: Vec<Snippet>,
    history: Vec<HistoryEntry>,
    snippet_name: String,
    tabs: Vec<RunTab>,
    active: usize,
    /// A working set sent from the Tags tab: (label, clone IDs). Replaces the filter.
    working_set: Option<(String, Vec<i64>)>,
    last_request: u64,
    last_tab: u64,
    pending: HashMap<u64, Destination>,
}

impl RunnerPage {
    pub fn new() -> Self {
        Self {
            command: String::new(),
            filter: String::new(),
            parallel: None,
            colors: true,
            snippets: Vec::new(),
            history: Vec::new(),
            snippet_name: String::new(),
            tabs: Vec::new(),
            active: 0,
            working_set: None,
            last_request: 0,
            last_tab: 0,
            pending: HashMap::new(),
        }
    }

    /// Runs the next command in exactly these clones.
    pub fn set_working_set(&mut self, label: String, clone_ids: Vec<i64>) {
        self.working_set = Some((label, clone_ids));
    }

    pub fn set_snippets(&mut self, snippets: Vec<Snippet>) {
        self.snippets = snippets;
    }

    pub fn set_history(&mut self, history: Vec<HistoryEntry>) {
        self.history = history;
    }

    /// A run started: open a tab for it, or put its results into the rows it re-runs.
    pub fn run_started(&mut self, run: Arc<Run>, request: Option<u64>) {
        match request.and_then(|id| self.pending.remove(&id)) {
            Some(Destination::Replace { tab, rows }) => {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab) {
                    tab.replace_rows(run, &rows);
                    return;
                }
                self.open_tab(run, None);
            }
            Some(Destination::NewTab { breadcrumb }) => self.open_tab(run, breadcrumb),
            None => self.open_tab(run, None),
        }
    }

    fn open_tab(&mut self, run: Arc<Run>, breadcrumb: Option<String>) {
        self.last_tab += 1;
        self.tabs.push(RunTab::new(self.last_tab, run, breadcrumb));
        self.active = self.tabs.len() - 1;
    }

    pub fn running_count(&self) -> usize {
        self.tabs.iter().filter(|t| !t.is_finished()).count()
    }

    fn request(
        &mut self,
        destination: Destination,
        command: String,
        targets: Vec<Target>,
        parallel: usize,
        colors: bool,
    ) -> RunnerAction {
        self.last_request += 1;
        self.pending.insert(self.last_request, destination);
        RunnerAction::Start {
            request: self.last_request,
            command,
            targets,
            parallel,
            colors,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: RunnerContext<'_>) -> Option<RunnerAction> {
        let mut action = self.command_bar(ui, &cx);
        ui.add_space(4.0);
        self.tab_strip(ui);
        ui.separator();

        let Some(tab) = self.tabs.get_mut(self.active) else {
            ui.add_space(8.0);
            ui.weak("Run a command to see its output here.");
            return action;
        };
        match tab.show(ui) {
            Some(TabAction::Rerun(rows)) => {
                let targets = rows.iter().map(|r| tab.rows[*r].target().clone()).collect();
                let (command, parallel, colors, id) =
                    (tab.command.clone(), tab.parallel, tab.colors, tab.id);
                action = Some(self.request(
                    Destination::Replace { tab: id, rows },
                    command,
                    targets,
                    parallel,
                    colors,
                ));
            }
            Some(TabAction::RunOnSelection(rows)) => {
                let targets: Vec<Target> =
                    rows.iter().map(|r| tab.rows[*r].target().clone()).collect();
                let here = tab.label();
                let breadcrumb = Some(match &tab.breadcrumb {
                    Some(parent) => format!("{parent} › {here}"),
                    None => here,
                });
                // The command typed above, or this tab's command if the field is empty.
                let command = match self.command.trim() {
                    "" => tab.command.clone(),
                    typed => typed.to_owned(),
                };
                let parallel = self.parallel.unwrap_or(cx.config.runner.parallel) as usize;
                let colors = self.colors;
                action = Some(self.request(
                    Destination::NewTab { breadcrumb },
                    command,
                    targets,
                    parallel,
                    colors,
                ));
            }
            None => {}
        }
        action
    }

    fn command_bar(&mut self, ui: &mut egui::Ui, cx: &RunnerContext<'_>) -> Option<RunnerAction> {
        let needle = self.filter.trim().to_lowercase();
        let targets: Vec<&CloneInfo> = cx
            .clones
            .unwrap_or_default()
            .iter()
            .filter(|c| match &self.working_set {
                Some((_, ids)) => ids.contains(&c.id),
                None => {
                    needle.is_empty()
                        || c.display_name().to_lowercase().contains(&needle)
                        || c.rel_path.to_lowercase().contains(&needle)
                }
            })
            .collect();

        let mut action = None;
        let mut start = false;
        ui.horizontal(|ui| {
            self.snippets_menu(ui, &mut action);
            self.history_menu(ui);
            let can_run = !self.command.trim().is_empty() && !targets.is_empty();
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.command)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("git status --short")
                    .desired_width(ui.available_width() - 70.0),
            );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add_enabled(can_run, egui::Button::new("Run")).clicked() || (entered && can_run) {
                start = true;
            }
        });

        let mut parallel = self.parallel.unwrap_or(cx.config.runner.parallel);
        ui.horizontal(|ui| {
            let label = ui.label("Run in");
            if let Some((name, _)) = &self.working_set {
                ui.strong(format!("working set \"{name}\""));
                ui.weak(plural(targets.len(), "clone", "clones"));
                if ui
                    .small_button("×")
                    .on_hover_text("Stop using the working set; run in all clones instead")
                    .clicked()
                {
                    self.working_set = None;
                }
            } else {
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("all clones (type to narrow)")
                        .desired_width(220.0),
                )
                .labelled_by(label.id);
                match cx.clones {
                    None => ui.weak("No scan yet"),
                    Some(all) if targets.len() == all.len() => {
                        ui.weak(plural(targets.len(), "clone", "clones"))
                    }
                    Some(all) => ui.weak(format!("{} of {}", targets.len(), all.len())),
                };
            }
            ui.separator();
            let label = ui.label("At once");
            if ui
                .add(egui::DragValue::new(&mut parallel).range(1..=64))
                .labelled_by(label.id)
                .changed()
            {
                self.parallel = Some(parallel);
            }
            ui.separator();
            let timeout = match cx.config.runner.timeout_secs {
                0 => "No timeout".to_owned(),
                secs => format!("Timeout {secs} s"),
            };
            ui.weak(timeout)
                .on_hover_text("Change the timeout in Settings");
            ui.separator();
            ui.checkbox(&mut self.colors, "Colors")
                .on_hover_text("Ask tools to keep their colors (CLICOLOR_FORCE and friends)");
            ui.separator();
            let label = ui.label("Save as snippet");
            ui.add(
                egui::TextEdit::singleline(&mut self.snippet_name)
                    .hint_text("name")
                    .desired_width(100.0),
            )
            .labelled_by(label.id);
            let can_save = !self.snippet_name.trim().is_empty() && !self.command.trim().is_empty();
            if ui
                .add_enabled(can_save, egui::Button::new("Save"))
                .clicked()
            {
                action = Some(RunnerAction::SaveSnippet {
                    name: std::mem::take(&mut self.snippet_name).trim().to_owned(),
                    command: self.command.trim().to_owned(),
                });
            }
        });

        if start {
            let targets = targets
                .iter()
                .map(|clone| Target {
                    name: clone.display_name(),
                    path: clone.abs_path.clone(),
                    tags: cx.tags.tags_of(clone.id).into_iter().collect(),
                    env: Vec::new(),
                })
                .collect();
            let command = self.command.trim().to_owned();
            let colors = self.colors;
            action = Some(self.request(
                Destination::NewTab { breadcrumb: None },
                command,
                targets,
                parallel as usize,
                colors,
            ));
        }
        action
    }

    fn snippets_menu(&mut self, ui: &mut egui::Ui, action: &mut Option<RunnerAction>) {
        ui.menu_button("Snippets", |ui| {
            if self.snippets.is_empty() {
                ui.weak("No saved commands");
            }
            for snippet in &self.snippets {
                ui.horizontal(|ui| {
                    if ui
                        .button(&snippet.name)
                        .on_hover_text(&snippet.command)
                        .clicked()
                    {
                        self.command = snippet.command.clone();
                        ui.close();
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text(format!("Delete the snippet \"{}\"", snippet.name))
                        .clicked()
                    {
                        *action = Some(RunnerAction::DeleteSnippet(snippet.id));
                    }
                });
            }
        });
    }

    fn history_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("History", |ui| {
            if self.history.is_empty() {
                ui.weak("Nothing run yet");
            }
            let mut seen = Vec::new();
            for entry in &self.history {
                if seen.contains(&&entry.command) || seen.len() >= 20 {
                    continue;
                }
                seen.push(&entry.command);
                let hover = format!(
                    "{} UTC · {} · {} ok, {} failed",
                    entry.started_at,
                    plural(entry.repo_count, "repo", "repos"),
                    entry.ok_count,
                    entry.failed_count
                );
                if ui
                    .button(egui::RichText::new(shorten(&one_line(&entry.command), 60)).monospace())
                    .on_hover_text(hover)
                    .clicked()
                {
                    self.command = entry.command.clone();
                    ui.close();
                }
            }
        });
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        if self.tabs.is_empty() {
            return;
        }
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (index, tab) in self.tabs.iter().enumerate() {
                let marker = if !tab.is_finished() {
                    "⟳"
                } else if tab.has_failures() {
                    "✖"
                } else {
                    "✔"
                };
                let label = format!("{marker} {}", shorten(&one_line(&tab.title), 32));
                if ui
                    .selectable_label(self.active == index, label)
                    .on_hover_text(&tab.command)
                    .clicked()
                {
                    self.active = index;
                }
                if ui
                    .small_button("×")
                    .on_hover_text("Close (cancels it if still running)")
                    .clicked()
                {
                    close = Some(index);
                }
                ui.add_space(6.0);
            }
            if ui
                .button("Clear all outputs")
                .on_hover_text("Forget the output of every tab")
                .clicked()
            {
                for tab in &self.tabs {
                    tab.clear_output();
                }
            }
        });
        if let Some(index) = close {
            let tab = self.tabs.remove(index);
            tab.cancel();
            if self.active >= index && self.active > 0 {
                self.active -= 1;
            }
        }
    }
}

/// Collapses newlines and runs of whitespace, for showing a command on one line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

fn short_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f32();
    if secs >= 60.0 {
        format!(
            "{}m {:02}s",
            duration.as_secs() / 60,
            duration.as_secs() % 60
        )
    } else if secs >= 1.0 {
        format!("{secs:.1} s")
    } else {
        format!("{} ms", duration.as_millis())
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests;
