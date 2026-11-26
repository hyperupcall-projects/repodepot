//! The log. The status bar shows the latest entry; clicking it opens a sidebar over the right
//! edge of the window that lists every entry, newest first.

use eframe::egui::{self, Align, AtomExt, Layout, RichText};

use crate::log::{Entry, Level};
use crate::time::utc_datetime;

/// Older entries are dropped past this many.
const MAX_ENTRIES: usize = 1000;
const SIDEBAR_WIDTH: f32 = 440.0;

pub struct LogView {
    /// Oldest first.
    entries: Vec<Entry>,
    open: bool,
    /// Which levels the sidebar lists, by `Level as usize`.
    shown: [bool; 3],
    /// Entries before this index have been listed in the open sidebar.
    seen: usize,
    /// Where the status bar's buttons were drawn this frame. Clicking them toggles the sidebar
    /// instead of counting as a click outside it.
    trigger: egui::Rect,
}

impl LogView {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            open: false,
            shown: [true; 3],
            seen: 0,
            trigger: egui::Rect::NOTHING,
        }
    }

    pub fn push(&mut self, entry: Entry) {
        entry.trace();
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            let extra = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..extra);
            self.seen = self.seen.saturating_sub(extra);
        }
        if self.open {
            self.seen = self.entries.len();
        }
    }

    #[cfg(test)]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn set_open(&mut self, open: bool) {
        self.open = open;
        if open {
            self.seen = self.entries.len();
        }
    }

    /// Warnings or errors added since the sidebar was last open.
    fn unseen(&self, level: Level) -> usize {
        self.entries[self.seen..]
            .iter()
            .filter(|e| e.level == level)
            .count()
    }

    /// Escape closes the sidebar. Call this before anything else that listens for Escape.
    pub fn handle_escape(&mut self, ui: &egui::Ui) {
        if self.open && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.set_open(false);
        }
    }

    /// The latest entry, then how many warnings and errors came since the sidebar was last
    /// open. Fills the rest of the row, right-aligned.
    pub fn status(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let mut toggle = false;
            let mut trigger = egui::Rect::NOTHING;
            if !self.open {
                for level in [Level::Warning, Level::Error] {
                    let count = self.unseen(level);
                    if count == 0 {
                        continue;
                    }
                    let text = RichText::new(format!("{} {count}", level.icon()))
                        .color(level_color(ui.visuals(), level));
                    let response = ui
                        .add(egui::Button::new(text).frame_when_inactive(false))
                        .on_hover_text(format!(
                            "{count} new {} since the log was last open",
                            match (level, count) {
                                (Level::Error, 1) => "error",
                                (Level::Error, _) => "errors",
                                (_, 1) => "warning",
                                _ => "warnings",
                            }
                        ));
                    toggle |= response.clicked();
                    trigger = trigger.union(response.rect);
                }
            }

            let button = match self.entries.last() {
                Some(entry) => {
                    let color = level_color(ui.visuals(), entry.level);
                    let icon = RichText::new(entry.level.icon()).color(color);
                    let message = RichText::new(&entry.message);
                    let message = match entry.level {
                        Level::Info => message,
                        _ => message.color(color),
                    };
                    egui::Button::new((icon, message.atom_shrink(true)))
                }
                None => egui::Button::new(RichText::new("Nothing logged yet").weak()),
            };
            let response = ui.add(
                button
                    .frame_when_inactive(false)
                    .selected(self.open)
                    .truncate(),
            );
            let hint = if self.open {
                "Click to hide the log"
            } else {
                "Click to show the log"
            };
            let response = match self.entries.last() {
                Some(entry) => response.on_hover_ui(|ui| {
                    ui.label(&entry.message);
                    if let Some(detail) = &entry.detail {
                        ui.weak(detail);
                    }
                    ui.weak(format!("{} UTC · {hint}", utc_datetime(entry.at)));
                }),
                None => response.on_hover_text(hint),
            };
            toggle |= response.clicked();
            self.trigger = trigger.union(response.rect);
            if toggle {
                self.set_open(!self.open);
            }
        });
    }

    /// Draws the sidebar over the right side of `rect` (the window between the tab bar and the
    /// status bar), sliding in and out. Clicking anywhere else closes it.
    pub fn sidebar(&mut self, ui: &egui::Ui, rect: egui::Rect) {
        let ctx = ui.ctx().clone();
        let id = egui::Id::unique("log_sidebar");
        let how_open = ctx.animate_bool_responsive(id.with("open"), self.open);
        if how_open == 0.0 {
            return;
        }
        let width = SIDEBAR_WIDTH.min(rect.width());
        let frame = egui::Frame::side_top_panel(ui.style())
            .stroke(ui.visuals().window_stroke)
            .shadow(ui.visuals().window_shadow);
        let margins = frame.total_margin().sum();
        let area = egui::Area::new(id)
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(rect.right() - width * how_open, rect.top()))
            .constrain(false)
            .show(&ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(width - margins.x);
                    ui.set_height(rect.height() - margins.y);
                    self.contents(ui);
                });
            });

        if self.open && how_open == 1.0 {
            let outside = ctx.input(|i| {
                i.pointer.any_click()
                    && i.pointer.interact_pos().is_some_and(|pos| {
                        !area.response.rect.contains(pos) && !self.trigger.contains(pos)
                    })
            });
            if outside {
                self.set_open(false);
            }
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Log");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Close").on_hover_text("Esc").clicked() {
                    self.set_open(false);
                }
                if ui
                    .add_enabled(!self.entries.is_empty(), egui::Button::new("Clear"))
                    .on_hover_text("Remove every entry")
                    .clicked()
                {
                    self.entries.clear();
                    self.seen = 0;
                }
            });
        });
        ui.horizontal(|ui| {
            for level in Level::ALL {
                let count = self.entries.iter().filter(|e| e.level == level).count();
                let shown = &mut self.shown[level as usize];
                let icon = RichText::new(level.icon()).color(level_color(ui.visuals(), level));
                let label = format!("{} {count}", level.plural());
                if ui
                    .add(egui::Button::selectable(*shown, (icon, label)))
                    .on_hover_text(if *shown {
                        "Shown. Click to hide."
                    } else {
                        "Hidden. Click to show."
                    })
                    .clicked()
                {
                    *shown = !*shown;
                }
            }
        });
        ui.separator();

        let shown = self.shown;
        let mut listed = self
            .entries
            .iter()
            .rev()
            .filter(|e| shown[e.level as usize])
            .peekable();
        if listed.peek().is_none() {
            ui.weak(if self.entries.is_empty() {
                "Nothing logged yet."
            } else {
                "Nothing at the levels shown."
            });
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for (i, entry) in listed.enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    entry_ui(ui, entry);
                }
            });
    }
}

/// Icon, message and detail, with the time on the right.
fn entry_ui(ui: &mut egui::Ui, entry: &Entry) {
    let color = level_color(ui.visuals(), entry.level);
    let at = utc_datetime(entry.at);
    ui.horizontal_top(|ui| {
        ui.label(RichText::new(entry.level.icon()).color(color));
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            ui.label(RichText::new(format!("{} UTC", &at[11..])).small().weak())
                .on_hover_text(format!("{at} UTC"));
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                let message = RichText::new(&entry.message);
                let message = match entry.level {
                    Level::Info => message,
                    _ => message.color(color),
                };
                ui.add(egui::Label::new(message).wrap());
                if let Some(detail) = &entry.detail {
                    ui.add(egui::Label::new(RichText::new(detail).weak()).wrap());
                }
            });
        });
    });
}

fn level_color(visuals: &egui::Visuals, level: Level) -> egui::Color32 {
    match level {
        Level::Info => visuals.weak_text_color(),
        Level::Warning => visuals.warn_fg_color,
        Level::Error => visuals.error_fg_color,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    /// A status bar with the log on its right, and something to click behind the sidebar.
    fn harness<'a>(builder: egui_kittest::HarnessBuilder<LogView>) -> Harness<'a, LogView> {
        builder.with_size([900.0, 420.0]).build_ui_state(
            |ui, log: &mut LogView| {
                egui::Panel::bottom("status").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Signed in as someone");
                        ui.separator();
                        log.status(ui);
                    });
                });
                let rect = ui.available_rect_before_wrap();
                log.handle_escape(ui);
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    let _ = ui.button("Behind");
                });
                log.sidebar(ui, rect);
            },
            LogView::new(),
        )
    }

    /// The latest message in the status bar.
    fn status_message<'h>(h: &'h Harness<'_, LogView>) -> egui_kittest::Node<'h> {
        h.get_all_by_label_contains("Synced with GitHub in")
            .next()
            .unwrap()
    }

    fn sample(log: &mut LogView) {
        log.push(
            Entry::info("Database ready (schema v6)")
                .detail("/home/someone/.local/share/repodepot/repodepot.db"),
        );
        log.push(Entry::info(
            "Last synced with GitHub on 2026-11-21 18:00:00 UTC",
        ));
        log.push(
            Entry::info("Scanned 412 clones in 380 ms").detail("Clone root: /home/someone/code"),
        );
        log.push(Entry::error("Scan failed").detail("permission denied: /home/someone/code/x"));
        log.push(Entry::warning(
            "GitHub sync: myorg: the token needs SSO approval for this org",
        ));
        log.push(
            Entry::info("Synced with GitHub in 2m 05s: 1,203 repos listed, 2 new forks")
                .detail("Owned: 120\nContributed: 1,050\nAdded by hand: 33"),
        );
    }

    #[test]
    fn oldest_entries_are_dropped() {
        let mut log = LogView::new();
        for i in 0..MAX_ENTRIES + 5 {
            log.push(Entry::info(i.to_string()));
        }
        assert_eq!(log.entries().len(), MAX_ENTRIES);
        assert_eq!(log.entries()[0].message, "5");
        assert_eq!(log.unseen(Level::Info), MAX_ENTRIES);
    }

    #[test]
    fn status_shows_the_latest_entry_and_new_problems() {
        let mut h = harness(Harness::builder());
        h.run();
        assert!(h.query_by_label_contains("Nothing logged yet").is_some());

        sample(h.state_mut());
        h.run();
        assert!(
            h.query_by_label_contains("Synced with GitHub in 2m 05s")
                .is_some()
        );
        assert!(h.query_by_label_contains("Scanned 412 clones").is_none());
        assert!(h.query_by_label("⚠ 1").is_some());
        assert!(h.query_by_label("✖ 1").is_some());

        h.get_by_label("✖ 1").click();
        h.run();
        assert!(h.state().is_open());
        // Everything is listed, newest first, and nothing is new any more.
        assert!(h.query_by_label("Scanned 412 clones in 380 ms").is_some());
        assert!(h.query_by_label("Clone root: /home/someone/code").is_some());
        assert!(h.query_by_label("✖ 1").is_none());
        // The status bar has the latest message too; the sidebar's comes after it.
        let synced = h
            .get_all_by_label_contains("Synced with GitHub in")
            .last()
            .unwrap()
            .rect();
        let ready = h.get_by_label("Database ready (schema v6)").rect();
        assert!(synced.top() < ready.top());
    }

    #[test]
    fn escape_close_and_clicking_outside_close_the_log() {
        let mut h = harness(Harness::builder());
        sample(h.state_mut());
        h.run();

        status_message(&h).click();
        h.run();
        assert!(h.state().is_open());
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.state().is_open());

        status_message(&h).click();
        h.run();
        h.get_by_label("Close").click();
        h.run();
        assert!(!h.state().is_open());

        status_message(&h).click();
        h.run();
        // Clicking inside the sidebar keeps it open.
        h.get_by_label("Scan failed").click();
        h.run();
        assert!(h.state().is_open());
        h.get_by_label("Behind").click();
        h.run();
        assert!(!h.state().is_open());

        // The status bar toggles it.
        status_message(&h).click();
        h.run();
        assert!(h.state().is_open());
        status_message(&h).click();
        h.run();
        assert!(!h.state().is_open());
    }

    #[test]
    fn levels_can_be_hidden_and_the_log_cleared() {
        let mut h = harness(Harness::builder());
        sample(h.state_mut());
        h.state_mut().set_open(true);
        h.run();
        h.get_by_label_contains("Info 4").click();
        h.run();
        assert!(h.query_by_label("Database ready (schema v6)").is_none());
        assert!(h.query_by_label("Scan failed").is_some());
        h.get_by_label_contains("Errors 1").click();
        h.get_by_label_contains("Warnings 1").click();
        h.run();
        assert!(h.query_by_label("Nothing at the levels shown.").is_some());

        h.get_by_label("Clear").click();
        h.run();
        assert!(h.state().entries().is_empty());
        assert!(h.query_by_label("Nothing logged yet.").is_some());
    }

    /// Renders the status bar and the open log to PNGs for a visual check. Only runs when
    /// `REPODEPOT_UI_SHOTS` names a folder: `REPODEPOT_UI_SHOTS=/tmp/shots cargo test ui_shots`.
    #[test]
    fn ui_shots() {
        let Some(dir) = std::env::var_os("REPODEPOT_UI_SHOTS").map(PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        for (theme, name) in [(egui::Theme::Dark, "dark"), (egui::Theme::Light, "light")] {
            let mut h = harness(Harness::builder().wgpu().with_theme(theme));
            sample(h.state_mut());
            h.run();
            h.render()
                .unwrap()
                .save(dir.join(format!("log-status-{name}.png")))
                .unwrap();
            status_message(&h).click();
            h.run();
            h.render()
                .unwrap()
                .save(dir.join(format!("log-open-{name}.png")))
                .unwrap();
        }
    }
}
