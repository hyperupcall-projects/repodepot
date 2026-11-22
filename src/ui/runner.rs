//! The Runner tab: run a shell command in many clones and watch the output.

use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::config::Config;
use crate::db::Snippet;
use crate::git::scan::CloneInfo;
use crate::jobs::output::Stream;
use crate::jobs::{Run, Summary, Target, TargetStatus};
use crate::tags::TagIndex;

#[derive(Debug)]
pub enum RunnerAction {
    Start {
        command: String,
        targets: Vec<Target>,
        parallel: usize,
    },
}

pub struct RunnerContext<'a> {
    /// The latest scan's clones, if there is one.
    pub clones: Option<&'a [CloneInfo]>,
    pub config: &'a Config,
    /// For each target's `DEPO_TAGS`.
    pub tags: &'a TagIndex,
}

struct RunTab {
    run: Arc<Run>,
    /// `None` shows every repo's output.
    selected: Option<usize>,
}

pub struct RunnerPage {
    command: String,
    filter: String,
    /// Overrides the setting for the next run.
    parallel: Option<u32>,
    snippets: Vec<Snippet>,
    tabs: Vec<RunTab>,
    active: usize,
    /// A working set sent from the Tags tab: (label, clone IDs). Replaces the filter.
    working_set: Option<(String, Vec<i64>)>,
}

impl RunnerPage {
    pub fn new() -> Self {
        Self {
            command: String::new(),
            filter: String::new(),
            parallel: None,
            snippets: Vec::new(),
            tabs: Vec::new(),
            active: 0,
            working_set: None,
        }
    }

    /// Runs the next command in exactly these clones.
    pub fn set_working_set(&mut self, label: String, clone_ids: Vec<i64>) {
        self.working_set = Some((label, clone_ids));
    }

    pub fn set_snippets(&mut self, snippets: Vec<Snippet>) {
        self.snippets = snippets;
    }

    pub fn run_started(&mut self, run: Arc<Run>) {
        self.tabs.push(RunTab {
            run,
            selected: None,
        });
        self.active = self.tabs.len() - 1;
    }

    pub fn running_count(&self) -> usize {
        self.tabs.iter().filter(|t| !t.run.is_finished()).count()
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: RunnerContext<'_>) -> Option<RunnerAction> {
        let action = self.command_bar(ui, &cx);
        ui.add_space(4.0);
        self.tab_strip(ui);
        ui.separator();

        match self.tabs.get_mut(self.active) {
            Some(tab) => run_view(ui, tab),
            None => {
                ui.add_space(8.0);
                ui.weak("Run a command to see its output here.");
            }
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

        let mut start = false;
        ui.horizontal(|ui| {
            ui.menu_button("Snippets", |ui| {
                if self.snippets.is_empty() {
                    ui.weak("No saved commands");
                }
                for snippet in &self.snippets {
                    if ui
                        .button(&snippet.name)
                        .on_hover_text(&snippet.command)
                        .clicked()
                    {
                        self.command = snippet.command.clone();
                        ui.close();
                    }
                }
            });
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
        });

        start.then(|| RunnerAction::Start {
            command: self.command.trim().to_owned(),
            targets: targets
                .iter()
                .map(|clone| Target {
                    name: clone.display_name(),
                    path: clone.abs_path.clone(),
                    tags: cx.tags.tags_of(clone.id).into_iter().collect(),
                    env: Vec::new(),
                })
                .collect(),
            parallel: parallel as usize,
        })
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        if self.tabs.is_empty() {
            return;
        }
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (index, tab) in self.tabs.iter().enumerate() {
                let summary = tab.run.summary();
                let marker = if !summary.is_finished() {
                    "⟳"
                } else if summary.failed > 0 {
                    "✖"
                } else {
                    "✔"
                };
                let label = format!("{marker} {}", shorten(&one_line(&tab.run.title), 32));
                if ui
                    .selectable_label(self.active == index, label)
                    .on_hover_text(&tab.run.command)
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
        });
        if let Some(index) = close {
            let tab = self.tabs.remove(index);
            tab.run.cancel();
            if self.active >= index && self.active > 0 {
                self.active -= 1;
            }
        }
    }
}

fn run_view(ui: &mut egui::Ui, tab: &mut RunTab) {
    let run = tab.run.clone();
    let summary = run.summary();

    ui.horizontal(|ui| {
        let title = egui::RichText::new(shorten(&one_line(&run.title), 100)).strong();
        let title = if run.title == run.command {
            title.monospace()
        } else {
            title
        };
        ui.label(title).on_hover_text(&run.command);
        ui.weak(format!(
            "· {} · {} at once · {}",
            plural(run.targets.len(), "repo", "repos"),
            run.parallel,
            short_duration(run.elapsed())
        ))
        .on_hover_text(format!("Run with {} -c", run.shell));
        summary_chips(ui, &summary);
        if !summary.is_finished() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Cancel all").clicked() {
                    run.cancel();
                }
            });
        }
    });
    ui.add_space(4.0);

    egui::Panel::left(("run_targets", run.id))
        .resizable(true)
        .default_size(340.0)
        .min_size(220.0)
        .show(ui, |ui| targets_list(ui, tab));

    match tab.selected {
        Some(index) => single_output(ui, &run, index),
        None => combined_output(ui, &run),
    }
}

fn summary_chips(ui: &mut egui::Ui, summary: &Summary) {
    if summary.succeeded > 0 {
        ui.colored_label(success_color(ui), format!("✔ {}", summary.succeeded));
    }
    if summary.failed > 0 {
        ui.colored_label(ui.visuals().error_fg_color, format!("✖ {}", summary.failed));
    }
    if summary.running > 0 {
        ui.spinner();
        ui.label(format!("{} running", summary.running));
    }
    if summary.queued > 0 {
        ui.weak(format!("{} queued", summary.queued));
    }
    if summary.cancelled > 0 {
        ui.weak(format!("{} cancelled", summary.cancelled));
    }
}

fn targets_list(ui: &mut egui::Ui, tab: &mut RunTab) {
    let run = tab.run.clone();
    if ui
        .selectable_label(tab.selected.is_none(), "All repos")
        .on_hover_text("Show every repo's output, one after another")
        .clicked()
    {
        tab.selected = None;
    }
    ui.add_space(2.0);

    ui.style_mut().interaction.selectable_labels = false;
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    TableBuilder::new(ui)
        .id_salt(("run_table", run.id))
        .striped(true)
        .sense(egui::Sense::click())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::exact(64.0))
        .column(Column::remainder().at_least(80.0).clip(true))
        .column(Column::exact(54.0))
        .column(Column::exact(44.0))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.strong("Status");
            });
            header.col(|ui| {
                ui.strong("Repository");
            });
            header.col(|ui| {
                ui.strong("Time");
            });
            header.col(|ui| {
                ui.strong("Lines");
            });
        })
        .body(|body| {
            body.rows(row_height, run.targets.len(), |mut row| {
                let index = row.index();
                let (status, lines) = {
                    let slot = run.slot(index);
                    (slot.status, slot.output.len() + slot.output.dropped())
                };
                row.set_selected(tab.selected == Some(index));
                row.col(|ui| status_cell(ui, status));
                row.col(|ui| {
                    ui.label(&run.targets[index].name);
                });
                row.col(|ui| {
                    if let Some(took) = status.took() {
                        ui.weak(short_duration(took));
                    }
                });
                row.col(|ui| {
                    if lines > 0 {
                        ui.weak(lines.to_string());
                    }
                });
                if row.response().clicked() {
                    tab.selected = if tab.selected == Some(index) {
                        None
                    } else {
                        Some(index)
                    };
                }
            });
        });
}

fn status_cell(ui: &mut egui::Ui, status: TargetStatus) {
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

fn single_output(ui: &mut egui::Ui, run: &Run, index: usize) {
    let target = &run.targets[index];
    let status = run.status(index);
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
            run.cancel_target(index);
        }
    });
    ui.separator();

    let (len, dropped, empty) = {
        let slot = run.slot(index);
        (
            slot.output.len(),
            slot.output.dropped(),
            slot.output.is_empty(),
        )
    };
    let extra = usize::from(dropped > 0);
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    if empty {
        ui.weak(if status.is_finished() {
            "(no output)"
        } else {
            "(no output yet)"
        });
        return;
    }
    egui::ScrollArea::both()
        .id_salt(("single_output", run.id, index))
        .auto_shrink(false)
        .stick_to_bottom(!status.is_finished())
        .show_rows(ui, row_height, len + extra, |ui, rows| {
            let slot = run.slot(index);
            for row in rows {
                if row < extra {
                    dropped_line(ui, dropped);
                } else if let Some(line) = slot.output.get(row - extra) {
                    output_line(ui, line.stream, &line.text);
                }
            }
        });
}

/// One repo's block in the combined view: header, maybe a "dropped" note, lines, spacer.
struct Segment {
    index: usize,
    start: usize,
    dropped: usize,
    lines: usize,
}

impl Segment {
    fn rows(&self) -> usize {
        1 + usize::from(self.dropped > 0) + self.lines.max(1) + 1
    }
}

fn combined_output(ui: &mut egui::Ui, run: &Run) {
    let mut segments = Vec::new();
    let mut total = 0;
    for index in 0..run.targets.len() {
        let slot = run.slot(index);
        if matches!(
            slot.status,
            TargetStatus::Queued | TargetStatus::Cancelled { took: None }
        ) {
            continue;
        }
        let segment = Segment {
            index,
            start: total,
            dropped: slot.output.dropped(),
            lines: slot.output.len(),
        };
        total += segment.rows();
        segments.push(segment);
    }
    if segments.is_empty() {
        ui.weak("Nothing has started yet.");
        return;
    }

    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    egui::ScrollArea::both()
        .id_salt(("combined_output", run.id))
        .auto_shrink(false)
        .show_rows(ui, row_height, total, |ui, rows| {
            for row in rows {
                let at = segments.partition_point(|s| s.start <= row) - 1;
                let segment = &segments[at];
                let offset = row - segment.start;
                let slot = run.slot(segment.index);
                let has_dropped = usize::from(segment.dropped > 0);
                if offset == 0 {
                    segment_header(ui, run, segment.index, slot.status);
                } else if offset == 1 && has_dropped == 1 {
                    dropped_line(ui, segment.dropped);
                } else if offset == segment.rows() - 1 {
                    ui.label(" ");
                } else {
                    let line_index = offset - 1 - has_dropped;
                    match slot.output.get(line_index) {
                        Some(line) => output_line(ui, line.stream, &line.text),
                        None => {
                            ui.weak(egui::RichText::new("(no output)").monospace());
                        }
                    }
                }
            }
        });
}

fn segment_header(ui: &mut egui::Ui, run: &Run, index: usize, status: TargetStatus) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("── {}", run.targets[index].name))
                .monospace()
                .strong(),
        );
        status_cell(ui, status);
        if let Some(took) = status.took() {
            ui.weak(egui::RichText::new(short_duration(took)).monospace());
        }
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

fn output_line(ui: &mut egui::Ui, stream: Stream, text: &str) {
    let mut rich = egui::RichText::new(text).monospace();
    if stream == Stream::Stderr {
        rich = rich.color(ui.visuals().error_fg_color.gamma_multiply(0.85));
    }
    ui.add(egui::Label::new(rich).extend());
}

fn success_color(ui: &egui::Ui) -> egui::Color32 {
    if ui.visuals().dark_mode {
        egui::Color32::from_rgb(110, 200, 120)
    } else {
        egui::Color32::from_rgb(20, 130, 50)
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
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::git::scan::MainRemote;
    use crate::jobs::{self, RunSpec};

    #[test]
    fn shortening() {
        assert_eq!(shorten("git status", 32), "git status");
        assert_eq!(shorten("abcdefghij", 5), "abcd…");
        assert_eq!(
            one_line("case x in\n  a) b ;;\nesac"),
            "case x in a) b ;; esac"
        );
        assert_eq!(short_duration(Duration::from_millis(40)), "40 ms");
        assert_eq!(short_duration(Duration::from_millis(2500)), "2.5 s");
        assert_eq!(short_duration(Duration::from_secs(125)), "2m 05s");
    }

    /// Temporary repo folders, removed on drop.
    struct Folders(PathBuf);

    impl Drop for Folders {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fake_clones(test: &str, names: &[&str]) -> (Folders, Vec<CloneInfo>) {
        let root =
            std::env::temp_dir().join(format!("depository-runner-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let clones = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let rel_path = format!("{}/{name}", &name[..1]);
                let abs_path = root.join(&rel_path);
                fs::create_dir_all(&abs_path).unwrap();
                CloneInfo {
                    id: i as i64 + 1,
                    repo_id: None,
                    rel_path,
                    abs_path,
                    remotes: Vec::new(),
                    main: MainRemote::NoRemotes,
                    status: Ok(Default::default()),
                    problems: Vec::new(),
                }
            })
            .collect();
        (Folders(root), clones)
    }

    /// Runs a command for real and waits for it, so the UI can be tested on a finished run.
    fn finished_run(command: &str, clones: &[CloneInfo]) -> Arc<Run> {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let spec = RunSpec {
                title: command.into(),
                command: command.into(),
                shell: "/bin/sh".into(),
                targets: clones
                    .iter()
                    .map(|c| Target {
                        name: c.display_name(),
                        path: c.abs_path.clone(),
                        tags: Vec::new(),
                        env: Vec::new(),
                    })
                    .collect(),
                parallel: 4,
                timeout: None,
                line_limit: 100,
            };
            let (run, handle) = jobs::start(7, spec, Arc::new(|| {}));
            handle.await.unwrap();
            run
        })
    }

    type State = (RunnerPage, Vec<CloneInfo>, Config, Vec<RunnerAction>);

    fn harness<'a>(
        builder: egui_kittest::HarnessBuilder<State>,
        state: State,
    ) -> Harness<'a, State> {
        builder.with_size([1280.0, 560.0]).build_ui_state(
            |ui, (page, clones, config, actions): &mut State| {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    // Clone 1 is tagged "rust".
                    let facts: Vec<crate::tags::CloneFacts> = clones
                        .iter()
                        .map(|c| crate::tags::CloneFacts::from_clone(c, None))
                        .collect();
                    let manual = std::collections::HashMap::from([(
                        1,
                        std::collections::BTreeSet::from(["rust".to_owned()]),
                    )]);
                    let tags = TagIndex::build(&facts, &manual, &[]);
                    let cx = RunnerContext {
                        clones: Some(clones),
                        config,
                        tags: &tags,
                    };
                    if let Some(action) = page.show(ui, cx) {
                        actions.push(action);
                    }
                });
            },
            state,
        )
    }

    #[test]
    fn run_button_uses_the_filtered_clones() {
        let (_folders, clones) = fake_clones("start", &["alpha", "beta", "gamma"]);
        let mut page = RunnerPage::new();
        page.command = "git status --short".into();
        page.filter = "et".into(); // only "beta" matches
        let mut h = harness(
            Harness::builder(),
            (page, clones, Config::default(), Vec::new()),
        );
        h.run();
        assert!(h.query_by_label_contains("1 of 3").is_some());
        h.get_by_label("Run").click();
        h.run();
        let RunnerAction::Start {
            command,
            targets,
            parallel,
        } = &h.state().3[0];
        assert_eq!(command, "git status --short");
        assert_eq!(parallel, &8);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "beta");
    }

    #[test]
    fn working_set_replaces_the_filter_and_fills_tags() {
        let (_folders, clones) = fake_clones("set", &["alpha", "beta", "gamma"]);
        let mut page = RunnerPage::new();
        page.command = "true".into();
        page.set_working_set("rust".into(), vec![1, 3]);
        let mut h = harness(
            Harness::builder(),
            (page, clones, Config::default(), Vec::new()),
        );
        h.run();
        assert!(h.query_by_label("working set \"rust\"").is_some());
        assert!(h.query_by_label("2 clones").is_some());
        h.get_by_label("Run").click();
        h.run();
        let RunnerAction::Start { targets, .. } = &h.state().3[0];
        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["alpha", "gamma"]);
        assert_eq!(targets[0].tags, ["rust"]);
        assert!(targets[1].tags.is_empty());

        h.get_by_label("×").click();
        h.run();
        assert!(h.query_by_label("3 clones").is_some(), "back to all clones");
    }

    #[test]
    fn finished_run_shows_status_and_output() {
        let (_folders, clones) = fake_clones("view", &["alpha", "beta"]);
        let run = finished_run(
            r#"echo "hello from $DEPO_REPO"; [ "$DEPO_REPO" = beta ] && { echo broke >&2; exit 4; }; true"#,
            &clones,
        );
        let mut page = RunnerPage::new();
        page.run_started(run);
        let mut h = harness(
            Harness::builder(),
            (page, clones, Config::default(), Vec::new()),
        );
        h.run();
        // Combined view shows both.
        assert!(h.query_by_label("hello from alpha").is_some());
        assert!(h.query_by_label("hello from beta").is_some());
        // Failed status shows in the repo list and in that repo's output header.
        assert_eq!(h.query_all_by_label("✖ 4").count(), 2);

        // Selecting one repo shows only its output.
        h.get_by_label("beta").click();
        h.run();
        assert!(h.query_by_label("broke").is_some());
        assert!(h.query_by_label("hello from alpha").is_none());

        // Closing the tab.
        h.get_by_label("×").click();
        h.run();
        assert!(h.state().0.tabs.is_empty());
    }

    /// Renders the Runner to PNGs when `DEPOSITORY_UI_SHOTS` names a folder.
    #[test]
    fn ui_shots() {
        let Some(dir) = std::env::var_os("DEPOSITORY_UI_SHOTS").map(PathBuf::from) else {
            return;
        };
        fs::create_dir_all(&dir).unwrap();
        let (_folders, clones) =
            fake_clones("shots", &["facebook-react", "serde", "tokio", "notes"]);
        let run = finished_run(
            r#"case "$DEPO_REPO" in
                 serde) printf ' M src/de.rs\n?? scratch.md\n' ;;
                 tokio) echo 'fatal: not a git repository' >&2; exit 128 ;;
                 notes) seq 1 150 ;;
               esac"#,
            &clones,
        );
        let mut page = RunnerPage::new();
        page.command = "git status --short".into();
        page.set_snippets(vec![]);
        page.run_started(run);
        let mut h = harness(
            Harness::builder().wgpu(),
            (page, clones, Config::default(), Vec::new()),
        );
        h.run();
        h.render()
            .unwrap()
            .save(dir.join("runner-all.png"))
            .unwrap();
        h.get_by_label("tokio").click();
        h.run();
        h.render()
            .unwrap()
            .save(dir.join("runner-one.png"))
            .unwrap();
    }
}
