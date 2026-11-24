//! The window: tabs along the top, status bar along the bottom.

use eframe::egui;

use crate::backend::{
    AuthState, Backend, Command, DbInfo, Event, FolderPurpose, LaunchRequest, LauncherInfo, Origin,
};
use crate::config::{Config, Paths, ThemeChoice};
use crate::db::TagStore;
use crate::system_theme::SystemTheme;
use crate::tags::{self, CloneFacts, TagIndex};
use crate::ui::repos::{ReposAction, ReposContext, ReposPage};
use crate::ui::runner::{RunnerAction, RunnerContext, RunnerPage};
use crate::ui::settings::{SettingsContext, SettingsPage};
use crate::ui::tags::{TagsAction, TagsContext, TagsPage};
use crate::ui::thousands;

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
    tags: TagsPage,
    tag_store: TagStore,
    /// What rules see about each scanned clone.
    facts: Vec<CloneFacts>,
    tag_index: TagIndex,
    /// Tags of repos that aren't cloned (the Remote List and Ignored), by repo ID.
    repo_tag_index: TagIndex,
    launchers: Vec<LauncherInfo>,
    auth: AuthState,
    db: Option<Result<DbInfo, String>>,
    system_theme: SystemTheme,
}

impl App {
    pub fn new(
        paths: Paths,
        config: Config,
        config_error: Option<String>,
        backend: Backend,
        system_theme: SystemTheme,
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
            tags: TagsPage::new(),
            tag_store: TagStore::default(),
            facts: Vec::new(),
            tag_index: TagIndex::default(),
            repo_tag_index: TagIndex::default(),
            launchers: Vec::new(),
            paths,
            backend,
            tab,
            config,
            config_error,
            auth: AuthState::Checking,
            db: None,
            system_theme,
        }
    }

    /// Uses the saved theme, or the unsaved choice while Settings is open so it can be previewed.
    fn apply_theme(&self, ctx: &egui::Context) {
        let choice = if self.tab == Tab::Settings {
            self.settings.theme()
        } else {
            self.config.appearance.theme
        };
        let preference = match choice {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
        };
        if ctx.options(|o| o.theme_preference) != preference {
            ctx.set_theme(preference);
            ctx.request_repaint();
        }
    }

    /// Recomputes every clone's and uncloned repo's tags after the clones, the GitHub lists,
    /// tags or rules change.
    fn rebuild_tags(&mut self) {
        let store = &self.tag_store;
        let clones = self.repos.clones().unwrap_or_default();
        self.facts = clones
            .iter()
            .map(|clone| {
                let repo = clone.repo_id.and_then(|id| store.repo_facts.get(&id));
                CloneFacts::from_clone(clone, repo)
            })
            .collect();
        let manual = tags::clone_manual_tags(
            &store.manual,
            &store.repo_manual,
            clones.iter().map(|c| (c.id, c.repo_id)),
        );
        self.tag_index = TagIndex::build(&self.facts, &manual, &store.rules);

        let repos: Vec<CloneFacts> = self
            .repos
            .catalog()
            .map(|c| {
                c.inbox
                    .iter()
                    .chain(&c.ignored)
                    .map(CloneFacts::from_repo)
                    .collect()
            })
            .unwrap_or_default();
        self.repo_tag_index = TagIndex::build_for_repos(&repos, &store.repo_manual, &store.rules);
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Db(result) => self.db = Some(result),
            Event::Auth(state) => self.auth = state,
            Event::AccountMessage(message) => self.settings.set_account_message(message),
            Event::ScanStarted => self.repos.scan_started(),
            Event::ScanProgress { done, total } => self.repos.scan_progress(done, total),
            Event::Scan(result) => {
                self.repos.scan_finished(result);
                self.rebuild_tags();
            }
            Event::Tags(store) => {
                self.tag_store = store;
                self.rebuild_tags();
            }
            Event::TagActionFailed(message) => self.tags.action_failed(message),
            Event::Launchers(launchers) => self.launchers = launchers,
            Event::LaunchFailed { origin, message } => match origin {
                Origin::Repos => self.repos.action_failed(message),
                Origin::Tags => self.tags.action_failed(message),
            },
            Event::RepoActionFailed(message) => self.repos.action_failed(message),
            Event::Snippets(snippets) => self.runner.set_snippets(snippets),
            Event::RunStarted { run, request } => self.runner.run_started(run, request),
            Event::History(history) => self.runner.set_history(history),
            Event::SyncStarted => self.repos.sync_started(),
            Event::SyncProgress(message) => self.repos.sync_progress(message),
            Event::SyncFinished(result) => self.repos.sync_finished(result),
            Event::Catalog(catalog) => {
                self.repos.set_catalog(catalog);
                self.rebuild_tags();
            }
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
            ReposAction::Tags(action) => self.handle_tags_action(action),
            ReposAction::Open { launcher, clone_id } => {
                self.launch(launcher, false, &[clone_id], String::new(), Origin::Repos);
            }
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

    fn handle_tags_action(&mut self, action: TagsAction) {
        let command = match action {
            TagsAction::AddTag {
                clone_ids,
                repo_ids,
                tag,
            } => Command::AddTag {
                clone_ids,
                repo_ids,
                tag,
            },
            TagsAction::RemoveTag {
                clone_ids,
                repo_ids,
                tag,
            } => Command::RemoveTag {
                clone_ids,
                repo_ids,
                tag,
            },
            TagsAction::SaveRule(rule) => Command::SaveRule(rule),
            TagsAction::DeleteRule(id) => Command::DeleteRule(id),
            TagsAction::SetRuleEnabled { rule_id, enabled } => {
                Command::SetRuleEnabled { rule_id, enabled }
            }
            TagsAction::SetRuleException {
                rule_id,
                clone_id,
                excepted,
            } => Command::SetRuleException {
                rule_id,
                clone_id,
                excepted,
            },
            TagsAction::RunOn { label, clone_ids } => {
                self.runner.set_working_set(label, clone_ids);
                self.tab = Tab::Runner;
                return;
            }
            TagsAction::Open {
                launcher,
                together,
                clone_ids,
                workspace_name,
            } => {
                self.launch(launcher, together, &clone_ids, workspace_name, Origin::Tags);
                return;
            }
        };
        self.backend.send(command);
    }

    fn launch(
        &self,
        launcher: String,
        together: bool,
        clone_ids: &[i64],
        workspace_name: String,
        origin: Origin,
    ) {
        let clones = self.repos.clones().unwrap_or_default();
        let targets = clone_ids
            .iter()
            .filter_map(|id| clones.iter().find(|c| c.id == *id))
            .map(|c| (c.display_name(), c.abs_path.clone()))
            .collect();
        self.backend.send(Command::Launch(LaunchRequest {
            launcher,
            together,
            targets,
            workspace_name,
            origin,
        }));
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
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(theme) = self.system_theme.get() {
            raw_input.system_theme = Some(theme);
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Some(event) = self.backend.try_recv() {
            self.handle_event(event);
        }
        self.apply_theme(ctx);
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
                    tags: &self.tag_index,
                    repo_tags: &self.repo_tag_index,
                    rules: &self.tag_store.rules,
                    launchers: &self.launchers,
                };
                if let Some(action) = self.repos.show(ui, cx) {
                    self.handle_repos_action(action);
                }
            }
            Tab::Tags => {
                let cx = TagsContext {
                    clones: self.repos.clones().unwrap_or_default(),
                    repos: self.repos.catalog().map_or(&[][..], |c| &c.inbox),
                    facts: &self.facts,
                    index: &self.tag_index,
                    repo_index: &self.repo_tag_index,
                    rules: &self.tag_store.rules,
                    launchers: &self.launchers,
                    confirm_over: self.config.launch.confirm_over as usize,
                };
                if let Some(action) = self.tags.show(ui, cx) {
                    self.handle_tags_action(action);
                }
            }
            Tab::Runner => {
                let cx = RunnerContext {
                    clones: self.repos.clones(),
                    config: &self.config,
                    tags: &self.tag_index,
                };
                let command = match self.runner.show(ui, cx) {
                    Some(RunnerAction::Start {
                        request,
                        command,
                        targets,
                        parallel,
                        colors,
                    }) => Some(Command::StartRun {
                        request,
                        command,
                        targets,
                        parallel,
                        colors,
                    }),
                    Some(RunnerAction::SaveSnippet { name, command }) => {
                        Some(Command::SaveSnippet { name, command })
                    }
                    Some(RunnerAction::DeleteSnippet(id)) => Some(Command::DeleteSnippet(id)),
                    None => None,
                };
                if let Some(command) = command {
                    self.backend.send(command);
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
                        launchers: &self.launchers,
                    },
                );
                if saved {
                    self.backend
                        .send(Command::ConfigChanged(self.config.clone()));
                }
            }
        });
        // A theme picked or a tab switched this frame shows from the next one.
        self.apply_theme(ui.ctx());
    }
}
