//! The Settings tab: GitHub account, then the preferences saved to config.toml.

use eframe::egui;

use crate::backend::{AuthState, Backend, Command, DbInfo, FolderPurpose, LauncherInfo};
use crate::config::{self, Config, Paths};
use crate::launch::{self, LauncherSpec, WorkspaceKind};
use crate::token::TokenSource;
use crate::ui::thousands;

pub struct SettingsContext<'a> {
    pub config: &'a mut Config,
    pub config_error: &'a mut Option<String>,
    pub paths: &'a Paths,
    pub auth: &'a AuthState,
    pub db: Option<&'a Result<DbInfo, String>>,
    pub backend: &'a Backend,
    /// Launchers found on this computer.
    pub launchers: &'a [LauncherInfo],
}

/// Edits happen on a draft; nothing is written until "Save settings".
pub struct SettingsPage {
    draft: Config,
    // Fields edited as text and parsed when building the config.
    clone_root: String,
    aliases: String,
    shell: String,
    create_folder_error: Option<String>,

    token_input: String,
    account_message: Option<Result<String, String>>,
    save_message: Option<Result<String, String>>,
    workspaces_message: Option<Result<String, String>>,
}

impl SettingsPage {
    pub fn new(config: &Config) -> Self {
        let mut page = Self {
            draft: config.clone(),
            clone_root: String::new(),
            aliases: String::new(),
            shell: String::new(),
            create_folder_error: None,
            token_input: String::new(),
            account_message: None,
            save_message: None,
            workspaces_message: None,
        };
        page.reset(config);
        page
    }

    fn reset(&mut self, config: &Config) {
        self.draft = config.clone();
        self.clone_root = config
            .clone_root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        self.aliases = config.github.ssh_host_aliases.join(", ");
        self.shell = config.runner.shell.clone().unwrap_or_default();
    }

    /// A folder was picked for the clone root. It still needs saving.
    pub fn set_clone_root(&mut self, path: &std::path::Path) {
        self.clone_root = path.display().to_string();
        self.create_folder_error = None;
    }

    pub fn set_account_message(&mut self, message: Result<String, String>) {
        self.account_message = Some(message);
    }

    /// Turns the draft into a config, or lists what needs fixing.
    fn build(&self) -> Result<Config, Vec<String>> {
        let mut config = self.draft.clone();
        let mut problems = Vec::new();

        let root = self.clone_root.trim();
        config.clone_root = if root.is_empty() {
            None
        } else {
            let path = config::expand_tilde(root);
            if !path.is_absolute() {
                problems.push("Clone root must be an absolute path".to_owned());
            }
            Some(path)
        };

        config.remotes.default_name = config.remotes.default_name.trim().to_owned();
        config.remotes.upstream_name = config.remotes.upstream_name.trim().to_owned();
        if let Err(e) = config::validate_remote_name(&config.remotes.default_name) {
            problems.push(format!("Default remote name {e}"));
        }
        if let Err(e) = config::validate_remote_name(&config.remotes.upstream_name) {
            problems.push(format!("Upstream remote name {e}"));
        }
        if config.remotes.default_name == config.remotes.upstream_name {
            problems.push("Default and upstream remote names must differ".to_owned());
        }

        let mut aliases: Vec<String> = Vec::new();
        for alias in self
            .aliases
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|a| !a.is_empty())
        {
            if !aliases.iter().any(|a| a == alias) {
                aliases.push(alias.to_owned());
            }
        }
        config.github.ssh_host_aliases = aliases;

        let shell = self.shell.trim();
        config.runner.shell = (!shell.is_empty()).then(|| shell.to_owned());

        for (i, spec) in config.launch.custom.iter().enumerate() {
            if let Err(e) = launch::validate(spec) {
                let name = if spec.name.trim().is_empty() {
                    format!("Launcher {}", i + 1)
                } else {
                    spec.name.trim().to_owned()
                };
                problems.push(format!("{name} {e}"));
            }
        }

        if problems.is_empty() {
            Ok(config)
        } else {
            Err(problems)
        }
    }

    /// Returns true on the frame the settings were saved.
    pub fn show(&mut self, ui: &mut egui::Ui, cx: SettingsContext<'_>) -> bool {
        let mut saved = false;
        let built = self.build();
        let dirty = match &built {
            Ok(config) => config != cx.config,
            Err(_) => true,
        };

        egui::Panel::bottom("settings_actions").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let can_save = dirty && built.is_ok();
                if ui
                    .add_enabled(can_save, egui::Button::new("Save settings"))
                    .clicked()
                    && let Ok(new) = &built
                {
                    match config::save(&cx.paths.config_file, new) {
                        Ok(()) => {
                            *cx.config = new.clone();
                            *cx.config_error = None;
                            self.reset(new);
                            self.save_message = Some(Ok("Settings saved.".to_owned()));
                            saved = true;
                        }
                        Err(e) => self.save_message = Some(Err(format!("{e:#}"))),
                    }
                }
                if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                    self.reset(cx.config);
                    self.save_message = None;
                }
                match &built {
                    Err(problems) => {
                        ui.colored_label(ui.visuals().error_fg_color, problems.join(" · "));
                    }
                    Ok(_) if dirty => {
                        ui.label("Unsaved changes");
                    }
                    Ok(_) => match &self.save_message {
                        Some(Ok(text)) => {
                            ui.label(format!("✔ {text}"));
                        }
                        Some(Err(text)) => {
                            ui.colored_label(ui.visuals().error_fg_color, text);
                        }
                        None => {}
                    },
                }
            });
            ui.add_space(4.0);
        });

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.heading("GitHub account");
                ui.add_space(4.0);
                self.account_section(ui, cx.auth, cx.backend);

                ui.add_space(20.0);
                ui.heading("Preferences");
                ui.weak(format!("Saved to {}", cx.paths.config_file.display()));
                if let Some(error) = cx.config_error.as_deref() {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                self.preferences(ui, cx.backend);
                self.launchers(ui, &cx);

                ui.add_space(20.0);
                ui.heading("Storage");
                section_grid(ui, "storage", |ui| {
                    ui.label("Config file");
                    ui.weak(cx.paths.config_file.display().to_string());
                    ui.end_row();

                    ui.label("Database");
                    match cx.db {
                        None => ui.weak("Opening…"),
                        Some(Ok(info)) => ui.weak(format!(
                            "{} (schema v{})",
                            info.path.display(),
                            info.schema_version
                        )),
                        Some(Err(e)) => ui.colored_label(ui.visuals().error_fg_color, e),
                    };
                    ui.end_row();

                    ui.label("Workspace files");
                    ui.weak(cx.paths.workspaces_dir.display().to_string());
                    ui.end_row();
                });
                ui.add_space(12.0);
            });
        saved
    }

    fn account_section(&mut self, ui: &mut egui::Ui, auth: &AuthState, backend: &Backend) {
        let checking = matches!(auth, AuthState::Checking);
        match auth {
            AuthState::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking…");
                });
            }
            AuthState::NoToken { .. } => {
                ui.label(
                    "No token found. Paste one below, set GITHUB_TOKEN, or sign in with `gh auth login`.",
                );
            }
            AuthState::SignedIn {
                viewer,
                rate,
                source,
                ..
            } => {
                ui.horizontal(|ui| {
                    ui.label("Signed in as");
                    ui.strong(&viewer.login);
                    if let Some(name) = viewer.name.as_deref().filter(|n| !n.is_empty()) {
                        ui.weak(format!("({name})"));
                    }
                });
                ui.weak(format!("Token from {}", source.describe()));
                ui.label(format!(
                    "{} of {} API points left this hour",
                    thousands(rate.remaining),
                    thousands(rate.limit)
                ))
                .on_hover_text(format!("Resets at {}", rate.reset_at));
            }
            AuthState::Failed { source, error, .. } => {
                let text = match source {
                    Some(source) => {
                        format!("The token from {} didn't work: {error}", source.describe())
                    }
                    None => format!("Couldn't check the token: {error}"),
                };
                ui.colored_label(ui.visuals().error_fg_color, text);
            }
        }
        if let Some(error) = auth.keyring_error() {
            ui.colored_label(ui.visuals().warn_fg_color, error);
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!checking, egui::Button::new("Check again"))
                .clicked()
            {
                self.account_message = None;
                backend.send(Command::CheckAuth);
            }
            if auth.source() == Some(TokenSource::Keyring)
                && ui
                    .add_enabled(!checking, egui::Button::new("Forget saved token"))
                    .clicked()
            {
                self.account_message = None;
                backend.send(Command::ForgetToken);
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.token_input)
                    .password(true)
                    .desired_width(320.0)
                    .hint_text("Paste a token to save it in the keyring"),
            );
            let has_input = !self.token_input.trim().is_empty();
            if ui
                .add_enabled(has_input && !checking, egui::Button::new("Save to keyring"))
                .clicked()
            {
                self.account_message = None;
                backend.send(Command::SaveToken(std::mem::take(&mut self.token_input)));
            }
        });
        ui.weak(
            "Use a classic token with the repo and read:org scopes, or gh's token. \
             Fine-grained tokens only cover one owner, so they miss contributions to other orgs. \
             The keyring is checked first, then GITHUB_TOKEN, then the gh CLI.",
        );
        match &self.account_message {
            Some(Ok(text)) => {
                ui.label(format!("✔ {text}"));
            }
            Some(Err(text)) => {
                ui.colored_label(ui.visuals().error_fg_color, text);
            }
            None => {}
        }
    }

    fn preferences(&mut self, ui: &mut egui::Ui, backend: &Backend) {
        subheading(ui, "Folders");
        section_grid(ui, "folders", |ui| {
            let label = ui.label("Clone root");
            ui.vertical(|ui| {
                let edited = ui
                    .horizontal(|ui| {
                        let field = ui
                            .add(
                                egui::TextEdit::singleline(&mut self.clone_root)
                                    .desired_width(360.0)
                                    .hint_text("~/code"),
                            )
                            .labelled_by(label.id);
                        if ui.button("Browse…").clicked() {
                            backend.send(Command::PickFolder(FolderPurpose::CloneRoot));
                        }
                        field.changed()
                    })
                    .inner;
                if edited {
                    self.create_folder_error = None;
                }
                if clone_root_status(ui, &self.clone_root) == RootAction::Create {
                    let path = config::expand_tilde(self.clone_root.trim());
                    self.create_folder_error = std::fs::create_dir_all(&path)
                        .err()
                        .map(|e| format!("Couldn't create {}: {e}", path.display()));
                }
                if let Some(error) = &self.create_folder_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
            ui.end_row();
        });

        subheading(ui, "Git remotes");
        section_grid(ui, "remotes", |ui| {
            ui.label("Default remote name");
            ui.vertical(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft.remotes.default_name)
                        .desired_width(160.0),
                );
                ui.weak("Name of the remote a clone is made from (git clone -o <name>).");
            });
            ui.end_row();

            ui.label("Upstream remote name");
            ui.vertical(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft.remotes.upstream_name)
                        .desired_width(160.0),
                );
                ui.weak("Points at a linked fork's parent.");
            });
            ui.end_row();

            ui.label("SSH host aliases for github.com");
            ui.vertical(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.aliases)
                        .desired_width(360.0)
                        .hint_text("github-work, github-personal"),
                );
                ui.weak(
                    "Hosts from ~/.ssh/config that point at github.com. Comma or space separated.",
                );
            });
            ui.end_row();
        });

        subheading(ui, "GitHub sync");
        section_grid(ui, "sync", |ui| {
            ui.label("On startup");
            ui.checkbox(
                &mut self.draft.github.sync_on_startup,
                "Sync with GitHub when the app opens",
            );
            ui.end_row();

            ui.label("Sources");
            ui.vertical(|ui| {
                let sources = &mut self.draft.github.sources;
                ui.checkbox(&mut sources.owned, "Repos I own");
                ui.checkbox(&mut sources.org_member, "Repos of orgs I'm a member of");
                ui.checkbox(&mut sources.collaborator, "Repos I'm a collaborator on");
                ui.checkbox(&mut sources.contributed, "Repos I've contributed to");
                ui.weak("Repos and orgs added by hand are always included.");
            });
            ui.end_row();
        });

        subheading(ui, "Cloning");
        section_grid(ui, "cloning", |ui| {
            ui.label("Partial clone");
            ui.checkbox(
                &mut self.draft.clone.partial,
                "Download file contents only when needed (--filter=blob:none)",
            );
            ui.end_row();

            ui.label("Submodules");
            ui.checkbox(
                &mut self.draft.clone.recurse_submodules,
                "Clone submodules too (--recurse-submodules)",
            );
            ui.end_row();
        });

        subheading(ui, "Command runner");
        section_grid(ui, "runner", |ui| {
            ui.label("Shell");
            let env_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
            ui.add(
                egui::TextEdit::singleline(&mut self.shell)
                    .desired_width(240.0)
                    .hint_text(format!("{env_shell} (from $SHELL)")),
            );
            ui.end_row();

            ui.label("Commands at once");
            ui.add(egui::DragValue::new(&mut self.draft.runner.parallel).range(1..=64));
            ui.end_row();

            ui.label("Timeout per repo");
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.draft.runner.timeout_secs)
                        .range(0..=86_400)
                        .suffix(" s"),
                );
                ui.weak("0 means no timeout");
            });
            ui.end_row();

            ui.label("Lines kept per repo");
            ui.add(
                egui::DragValue::new(&mut self.draft.runner.line_limit)
                    .range(100..=1_000_000)
                    .speed(100),
            );
            ui.end_row();
        });
    }

    fn launchers(&mut self, ui: &mut egui::Ui, cx: &SettingsContext<'_>) {
        subheading(ui, "Opening repos");
        section_grid(ui, "launch", |ui| {
            ui.label("Ask before opening more than");
            ui.add(
                egui::DragValue::new(&mut self.draft.launch.confirm_over)
                    .range(1..=200)
                    .suffix(" windows"),
            );
            ui.end_row();

            ui.label("Found on this computer");
            if cx.launchers.is_empty() {
                ui.weak("None of the known editors");
            } else {
                let names: Vec<&str> = cx.launchers.iter().map(|l| l.name.as_str()).collect();
                ui.label(names.join(", "));
            }
            ui.end_row();

            ui.label("Workspace files");
            ui.horizontal(|ui| {
                if ui
                    .button("Delete workspace files")
                    .on_hover_text(cx.paths.workspaces_dir.display().to_string())
                    .clicked()
                {
                    self.workspaces_message = Some(
                        launch::delete_workspaces(&cx.paths.workspaces_dir)
                            .map(|n| format!("Deleted {n}."))
                            .map_err(|e| format!("{e:#}")),
                    );
                }
                match &self.workspaces_message {
                    Some(Ok(text)) => {
                        ui.weak(text);
                    }
                    Some(Err(text)) => {
                        ui.colored_label(ui.visuals().error_fg_color, text);
                    }
                    None => {
                        ui.weak("They're recreated the next time a group is opened together.");
                    }
                }
            });
            ui.end_row();
        });

        ui.add_space(8.0);
        ui.strong("Your own launchers");
        ui.weak(
            "Arguments use {path} for one folder, {paths} for every folder, and {workspace} for \
             a workspace file (pick its kind). A launcher with the same name as a built-in one \
             replaces it.",
        );
        let mut remove = None;
        for (index, spec) in self.draft.launch.custom.iter_mut().enumerate() {
            ui.push_id(("custom_launcher", index), |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::Grid::new("fields")
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            let label = ui.label("Name");
                            ui.add(egui::TextEdit::singleline(&mut spec.name).desired_width(200.0))
                                .labelled_by(label.id);
                            ui.end_row();

                            let label = ui.label("Program");
                            if spec.programs.is_empty() {
                                spec.programs.push(String::new());
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut spec.programs[0])
                                    .hint_text("helix, or a full path")
                                    .desired_width(200.0),
                            )
                            .labelled_by(label.id);
                            ui.end_row();

                            optional_args(ui, "Open each", &mut spec.each, "{path}");
                            optional_args(ui, "Open together", &mut spec.together, "{paths}");

                            let label = ui.label("Workspace file");
                            egui::ComboBox::from_id_salt("workspace_kind")
                                .selected_text(spec.workspace.label())
                                .show_ui(ui, |ui| {
                                    for kind in WorkspaceKind::ALL {
                                        ui.selectable_value(
                                            &mut spec.workspace,
                                            kind,
                                            kind.label(),
                                        );
                                    }
                                })
                                .response
                                .labelled_by(label.id);
                            ui.end_row();
                        });
                    if let Err(e) = launch::validate(spec) {
                        ui.colored_label(ui.visuals().warn_fg_color, format!("This launcher {e}."));
                    }
                    if ui.small_button("Remove launcher").clicked() {
                        remove = Some(index);
                    }
                });
            });
        }
        if let Some(index) = remove {
            self.draft.launch.custom.remove(index);
        }
        if ui.button("Add a launcher").clicked() {
            self.draft.launch.custom.push(LauncherSpec {
                name: String::new(),
                programs: vec![String::new()],
                flatpak: None,
                requires: Vec::new(),
                each: Some("{path}".to_owned()),
                together: None,
                workspace: WorkspaceKind::None,
            });
        }
    }
}

/// An optional argument string: empty means "this app can't do that".
fn optional_args(ui: &mut egui::Ui, title: &str, value: &mut Option<String>, hint: &str) {
    let label = ui.label(title);
    let mut text = value.clone().unwrap_or_default();
    if ui
        .add(
            egui::TextEdit::singleline(&mut text)
                .font(egui::TextStyle::Monospace)
                .hint_text(format!("e.g. -n {hint}; empty if it can't"))
                .desired_width(260.0),
        )
        .labelled_by(label.id)
        .changed()
    {
        *value = (!text.trim().is_empty()).then_some(text);
    }
    ui.end_row();
}

fn subheading(ui: &mut egui::Ui, title: &str) {
    ui.add_space(12.0);
    ui.strong(title);
    ui.separator();
}

fn section_grid(ui: &mut egui::Ui, id: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([24.0, 10.0])
        .min_col_width(200.0)
        .show(ui, add_contents);
}

#[derive(PartialEq, Eq)]
enum RootAction {
    None,
    Create,
}

/// Shows whether the clone root exists. Returns `Create` when "Create folder" was clicked.
fn clone_root_status(ui: &mut egui::Ui, text: &str) -> RootAction {
    let text = text.trim();
    if text.is_empty() {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "Not set. It's needed before anything can be scanned or cloned.",
        );
        return RootAction::None;
    }
    let path = config::expand_tilde(text);
    if !path.is_absolute() {
        ui.colored_label(ui.visuals().error_fg_color, "Must be an absolute path.");
    } else if path.is_dir() {
        ui.weak("✔ Folder exists");
    } else if path.exists() {
        ui.colored_label(ui.visuals().error_fg_color, "That's a file, not a folder.");
    } else {
        let mut action = RootAction::None;
        ui.horizontal(|ui| {
            ui.colored_label(ui.visuals().warn_fg_color, "Folder doesn't exist yet.");
            if ui.button("Create folder").clicked() {
                action = RootAction::Create;
            }
        });
        return action;
    }
    RootAction::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_draft_builds_the_same_config() {
        let config = Config {
            clone_root: Some("/home/someone/code".into()),
            github: crate::config::Github {
                ssh_host_aliases: vec!["github-work".into(), "gh2".into()],
                ..Default::default()
            },
            runner: crate::config::Runner {
                shell: Some("/bin/zsh".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let page = SettingsPage::new(&config);
        assert_eq!(page.build().unwrap(), config);
    }

    #[test]
    fn aliases_are_split_trimmed_and_deduplicated() {
        let mut page = SettingsPage::new(&Config::default());
        page.aliases = " github-work,gh2  github-work ,".into();
        let config = page.build().unwrap();
        assert_eq!(config.github.ssh_host_aliases, vec!["github-work", "gh2"]);
    }

    #[test]
    fn bad_values_are_reported() {
        let mut page = SettingsPage::new(&Config::default());
        page.clone_root = "relative/path".into();
        page.draft.remotes.default_name = "upstream".into();
        let problems = page.build().unwrap_err();
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn custom_launchers_must_be_valid() {
        let mut page = SettingsPage::new(&Config::default());
        page.draft.launch.custom.push(LauncherSpec {
            name: "Helix".into(),
            programs: vec!["hx".into()],
            flatpak: None,
            requires: Vec::new(),
            each: Some("--open".into()),
            together: None,
            workspace: WorkspaceKind::None,
        });
        let problems = page.build().unwrap_err();
        assert_eq!(problems, ["Helix its \"each\" arguments need {path}"]);
        page.draft.launch.custom[0].each = Some("{path}".into());
        assert!(page.build().is_ok());
    }

    #[test]
    fn empty_text_fields_become_none() {
        let mut page = SettingsPage::new(&Config::default());
        page.clone_root = "   ".into();
        page.shell = "".into();
        let config = page.build().unwrap();
        assert_eq!(config.clone_root, None);
        assert_eq!(config.runner.shell, None);
    }
}
