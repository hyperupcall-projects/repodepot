//! The Import view: find repos in a folder, preview where they'll go, then move them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::backend::ImportMove;
use crate::config;
use crate::git::import::ImportPreview;
use crate::ui::repos::ReposAction;

pub struct ImportPage {
    pub folder: String,
    searching: bool,
    preview: Option<Result<ImportPreview, String>>,
    /// Indices into the preview's items.
    selected: HashSet<usize>,
    rename_origin: bool,
}

impl ImportPage {
    pub fn new() -> Self {
        Self {
            folder: String::new(),
            searching: false,
            preview: None,
            selected: HashSet::new(),
            rename_origin: true,
        }
    }

    pub fn searching(&mut self) {
        self.searching = true;
    }

    pub fn set_folder(&mut self, path: &Path) {
        self.folder = path.display().to_string();
    }

    pub fn set_preview(&mut self, preview: Result<ImportPreview, String>) {
        self.searching = false;
        self.selected = match &preview {
            Ok(preview) => (0..preview.items.len()).collect(),
            Err(_) => HashSet::new(),
        };
        self.preview = Some(preview);
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        default_remote: &str,
        action: &mut Option<ReposAction>,
    ) {
        ui.weak(
            "Moves git repos into the clone root's layout. They're moved, not copied, unless \
             they're on another disk.",
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let label = ui.label("Folder");
            let field = ui
                .add(
                    egui::TextEdit::singleline(&mut self.folder)
                        .hint_text("~/old-projects")
                        .desired_width(380.0),
                )
                .labelled_by(label.id);
            if ui.button("Browse…").clicked() {
                *action = Some(ReposAction::PickImportFolder);
            }
            let can_search = !self.folder.trim().is_empty() && !self.searching;
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui
                .add_enabled(can_search, egui::Button::new("Find repos"))
                .clicked()
                || entered)
                && can_search
            {
                *action = Some(ReposAction::PreviewImport {
                    folder: Some(config::expand_tilde(self.folder.trim())),
                    paths: Vec::new(),
                });
            }
        });
        ui.add_space(4.0);

        if self.searching {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking for repos…");
            });
            return;
        }
        let Some(preview) = &self.preview else {
            return;
        };
        let preview = match preview {
            Ok(preview) => preview,
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
                return;
            }
        };

        ui.horizontal(|ui| {
            ui.label(format!("{} to move", preview.items.len()));
            if preview.in_place > 0 {
                ui.weak(format!("· {} already in place", preview.in_place));
            }
            if !preview.skipped.is_empty() {
                let lines: Vec<String> = preview
                    .skipped
                    .iter()
                    .map(|(path, why)| format!("{}: {why}", path.display()))
                    .collect();
                ui.weak(format!("· {} skipped", preview.skipped.len()))
                    .on_hover_text(lines.join("\n"));
            }
            if preview.truncated {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "· stopped early: that folder tree is very big, so pick a smaller one",
                );
            }
        });
        if preview.items.is_empty() {
            ui.weak("Nothing to move.");
            return;
        }

        if preview.items.iter().any(|i| i.can_rename_origin) {
            ui.checkbox(
                &mut self.rename_origin,
                format!("Rename the remote \"origin\" to \"{default_remote}\""),
            );
        }
        ui.horizontal(|ui| {
            if ui.button("Select all").clicked() {
                self.selected = (0..preview.items.len()).collect();
            }
            if ui.button("Select none").clicked() {
                self.selected.clear();
            }
            let count = self.selected.len();
            ui.label(format!("{count} selected"));
            let label = format!("Move {count} into place");
            if ui
                .add_enabled(count > 0, egui::Button::new(label))
                .clicked()
            {
                let mut indices: Vec<usize> = self.selected.iter().copied().collect();
                indices.sort_unstable();
                let moves = indices
                    .into_iter()
                    .map(|i| &preview.items[i])
                    .map(|item| ImportMove {
                        source: item.source.clone(),
                        destination: item.destination.clone(),
                        rename_origin: self.rename_origin && item.can_rename_origin,
                    })
                    .collect();
                *action = Some(ReposAction::Import(moves));
            }
        });
        ui.add_space(4.0);

        let searched = preview.searched.clone();
        let from = |source: &PathBuf| match searched
            .as_deref()
            .and_then(|s| source.strip_prefix(s).ok())
        {
            Some(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
            _ => source.display().to_string(),
        };
        ui.style_mut().interaction.selectable_labels = false;
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        TableBuilder::new(ui)
            .id_salt("import_table")
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(
                Column::initial(240.0)
                    .at_least(120.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(
                Column::initial(240.0)
                    .at_least(100.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(
                Column::initial(160.0)
                    .at_least(80.0)
                    .clip(true)
                    .resizable(true),
            )
            .column(Column::remainder().at_least(120.0).clip(true))
            .header(22.0, |mut header| {
                for title in ["Repository", "From", "To", "Notes"] {
                    header.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(row_height, preview.items.len(), |mut row| {
                    let index = row.index();
                    let item = &preview.items[index];
                    let selected = self.selected.contains(&index);
                    row.set_selected(selected);
                    row.col(|ui| {
                        ui.label(&item.name);
                    });
                    row.col(|ui| {
                        ui.weak(from(&item.source))
                            .on_hover_text(item.source.display().to_string());
                    });
                    row.col(|ui| {
                        ui.label(&item.destination);
                    });
                    row.col(|ui| {
                        let mut notes = item.notes.clone();
                        if self.rename_origin && item.can_rename_origin {
                            notes.push(format!("renames origin to {default_remote}"));
                        }
                        let text = notes.join(" · ");
                        ui.weak(&text).on_hover_text(notes.join("\n"));
                    });
                    if row.response().clicked() {
                        if selected {
                            self.selected.remove(&index);
                        } else {
                            self.selected.insert(index);
                        }
                    }
                });
            });
    }

    /// Clears the preview once its moves have started.
    pub fn moves_started(&mut self) {
        self.preview = None;
        self.selected.clear();
    }
}
