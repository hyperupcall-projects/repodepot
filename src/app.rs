//! The window: tabs along the top, status bar along the bottom.

use eframe::egui;

use crate::backend::{AuthState, Backend, Command, DbInfo, Event, FolderPurpose};
use crate::config::{Config, Paths};
use crate::ui::repos::{ReposAction, ReposContext, ReposPage};
use crate::ui::runner::{RunnerAction, RunnerContext, RunnerPage};
use crate::ui::settings::{SettingsContext, SettingsPage};
use crate::ui::{placeholder, thousands};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Repositories,
    Tags,
    Runner,
    Settings,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Repositories, Tab::Tags, Tab::Runner, Tab::Settings];

    fn label(self) -> &'static str {
        match self {
            Tab::Repositories => "Repositories",
            Tab::Tags => "Tags",
            Tab::Runner => "Runner",
            Tab::Settings => "Settings",
        }
    }
}

pub struct App {
    paths: Paths,
    backend: Backend,
    tab: Tab,
    config: Config,
    config_error: Option<String>,
    repos: ReposPage,
    runner: RunnerPage,
    settings: SettingsPage,
    auth: AuthState,
    db: Option<Result<DbInfo, String>>,
}

impl App {
    pub fn new(
        paths: Paths,
        config: Config,
        config_error: Option<String>,
        backend: Backend,
    ) -> Self {
        backend.send(Command::CheckAuth);
        // Nothing works without a clone root, so start on Settings until there is one.
        let tab = if config.clone_root.is_none() || config_error.is_some() {
            Tab::Settings
        } else {
            Tab::Repositories
        };
        Self {
            repos: ReposPage::new(),
            runner: RunnerPage::new(),
            settings: SettingsPage::new(&config),
            paths,
            backend,
            tab,
            config,
            config_error,
            auth: AuthState::Checking,
            db: None,
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Db(result) => self.db = Some(result),
            Event::Auth(state) => self.auth = state,
            Event::AccountMessage(message) => self.settings.set_account_message(message),
            Event::ScanStarted => self.repos.scan_started(),
            Event::ScanProgress { done, total } => self.repos.scan_progress(done, total),
            Event::Scan(result) => self.repos.scan_finished(result),
            Event::RepoActionFailed(message) => self.repos.action_failed(message),
            Event::Snippets(snippets) => self.runner.set_snippets(snippets),
            Event::RunStarted(run) => self.runner.run_started(run),
            Event::SyncStarted => self.repos.sync_started(),
            Event::SyncProgress(message) => self.repos.sync_progress(message),
            Event::SyncFinished(result) => self.repos.sync_finished(result),
            Event::Catalog(catalog) => self.repos.set_catalog(catalog),
            Event::ClonesFinished(repo_ids) => self.repos.clones_finished(&repo_ids),
            Event::RepoNotice(message) => self.repos.notice(message),
            Event::FolderPicked { purpose, path } => match purpose {
                FolderPurpose::Import => {
                    self.repos.import_folder_picked(&path);
                    self.backend.send(Command::PreviewImport {
                        folder: Some(path),
                        paths: Vec::new(),
                    });
                }
                FolderPurpose::CloneRoot => self.settings.set_clone_root(&path),
            },
            Event::ImportSearching => self.repos.import_searching(),
            Event::ImportPreview(result) => self.repos.import_preview(result),
        }
    }

    fn handle_repos_action(&mut self, action: ReposAction) {
        match action {
            ReposAction::Rescan => self.backend.send(Command::Scan),
            ReposAction::SetMainRemote { clone_id, remote } => self
                .backend
                .send(Command::SetMainRemote { clone_id, remote }),
            ReposAction::ForgetClone(clone_id) => {
                self.backend.send(Command::ForgetClone(clone_id));
            }
            ReposAction::OpenSettings => self.tab = Tab::Settings,
            ReposAction::OpenRunner => self.tab = Tab::Runner,
            ReposAction::Sync => self.backend.send(Command::Sync),
            ReposAction::Clone(repo_ids) => self.backend.send(Command::Clone(repo_ids)),
            ReposAction::SetIgnored { repo_ids, ignored } => {
                self.backend.send(Command::SetIgnored { repo_ids, ignored })
            }
            ReposAction::SetForkDecision {
                fork_id,
                parent_id,
                decision,
            } => self.backend.send(Command::SetForkDecision {
                fork_id,
                parent_id,
                decision,
            }),
            ReposAction::AddManual(text) => self.backend.send(Command::AddManual(text)),
            ReposAction::RemoveManualOwner(login) => {
                self.backend.send(Command::RemoveManualOwner(login));
            }
            ReposAction::PickImportFolder => {
                self.backend
                    .send(Command::PickFolder(FolderPurpose::Import));
            }
            ReposAction::PreviewImport { folder, paths } => {
                self.backend.send(Command::PreviewImport { folder, paths });
            }
            ReposAction::Import(moves) => self.backend.send(Command::Import(moves)),
            ReposAction::SetRemote {
                path,
                name,
                url,
                add,
            } => self.backend.send(Command::SetRemote {
                path,
                name,
                url,
                add,
            }),
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match &self.auth {
                AuthState::Checking => {
                    ui.spinner();
                    ui.label("Checking GitHub sign-in…");
                }
                AuthState::NoToken { .. } => {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Not signed in to GitHub. Add a token in Settings.",
                    );
                }
                AuthState::SignedIn { viewer, rate, .. } => {
                    ui.label(format!("Signed in as {}", viewer.login));
                    ui.weak("·");
                    ui.weak(format!(
                        "{} of {} API points left",
                        thousands(rate.remaining),
                        thousands(rate.limit)
                    ))
                    .on_hover_text(format!("Resets at {}", rate.reset_at));
                }
                AuthState::Failed { error, .. } => {
                    ui.colored_label(ui.visuals().error_fg_color, "GitHub sign-in failed")
                        .on_hover_text(error);
                }
            }

            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| match &self.db {
                    None => {
                        ui.weak("Opening database…");
                    }
                    Some(Ok(info)) => {
                        ui.weak(format!("Database ready (schema v{})", info.schema_version))
                            .on_hover_text(info.path.display().to_string());
                    }
                    Some(Err(e)) => {
                        ui.colored_label(ui.visuals().error_fg_color, "Database error")
                            .on_hover_text(e);
                    }
                },
            );
        });
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Some(event) = self.backend.try_recv() {
            self.handle_event(event);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("depository");
                ui.separator();
                for tab in Tab::ALL {
                    let running = self.runner.running_count();
                    let label = if tab == Tab::Runner && running > 0 {
                        format!("{} ({running} running)", tab.label())
                    } else {
                        tab.label().to_owned()
                    };
                    ui.selectable_value(&mut self.tab, tab, label);
                }
            });
        });

        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));

        egui::CentralPanel::default_margins().show(ui, |ui| match self.tab {
            Tab::Repositories => {
                let cx = ReposContext {
                    has_clone_root: self.config.clone_root.is_some(),
                    default_remote: &self.config.remotes.default_name,
                    upstream_name: &self.config.remotes.upstream_name,
                };
                if let Some(action) = self.repos.show(ui, cx) {
                    self.handle_repos_action(action);
                }
            }
            Tab::Tags => placeholder(ui, "Tags", "Tags, rules and groups come in build step 6."),
            Tab::Runner => {
                let cx = RunnerContext {
                    clones: self.repos.clones(),
                    config: &self.config,
                };
                if let Some(RunnerAction::Start {
                    command,
                    targets,
                    parallel,
                }) = self.runner.show(ui, cx)
                {
                    self.backend.send(Command::StartRun {
                        command,
                        targets,
                        parallel,
                    });
                }
            }
            Tab::Settings => {
                let saved = self.settings.show(
                    ui,
                    SettingsContext {
                        config: &mut self.config,
                        config_error: &mut self.config_error,
                        paths: &self.paths,
                        auth: &self.auth,
                        db: self.db.as_ref(),
                        backend: &self.backend,
                    },
                );
                if saved {
                    self.backend
                        .send(Command::ConfigChanged(self.config.clone()));
                }
            }
        });
    }
}
