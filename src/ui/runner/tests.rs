use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::tab::{SearchMode, SortKey};
use super::*;
use crate::git::scan::MainRemote;
use crate::jobs::{self, RunSpec};
use crate::tags::CloneFacts;

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
    let root = std::env::temp_dir().join(format!("repodepot-runner-{test}-{}", std::process::id()));
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

fn targets(clones: &[CloneInfo]) -> Vec<Target> {
    clones
        .iter()
        .map(|c| Target {
            name: c.display_name(),
            path: c.abs_path.clone(),
            tags: Vec::new(),
            env: Vec::new(),
        })
        .collect()
}

/// Runs a command for real and waits for it, so the UI can be tested on a finished run.
fn finished_run(command: &str, targets: Vec<Target>, colors: bool) -> Arc<Run> {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let spec = RunSpec {
            title: command.into(),
            command: command.into(),
            shell: "/bin/sh".into(),
            targets,
            parallel: 4,
            timeout: None,
            line_limit: 100,
            colors,
        };
        let (run, handle) = jobs::start(7, spec, Arc::new(|| {}));
        handle.await.unwrap();
        run
    })
}

type State = (RunnerPage, Vec<CloneInfo>, Config, Vec<RunnerAction>);

fn harness<'a>(builder: egui_kittest::HarnessBuilder<State>, state: State) -> Harness<'a, State> {
    builder.with_size([1400.0, 640.0]).build_ui_state(
        |ui, (page, clones, config, actions): &mut State| {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                // Clone 1 is tagged "rust".
                let facts: Vec<CloneFacts> = clones
                    .iter()
                    .map(|c| CloneFacts::from_clone(c, None))
                    .collect();
                let manual = HashMap::from([(1, BTreeSet::from(["rust".to_owned()]))]);
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

/// A page with one finished run of `command` over alpha, beta and gamma.
fn page_with_run<'a>(test: &str, command: &str) -> (Folders, Harness<'a, State>) {
    let (folders, clones) = fake_clones(test, &["alpha", "beta", "gamma"]);
    let run = finished_run(command, targets(&clones), false);
    let mut page = RunnerPage::new();
    page.run_started(run, None);
    let h = harness(
        Harness::builder(),
        (page, clones, Config::default(), Vec::new()),
    );
    (folders, h)
}

const MIXED: &str = r#"case "$REPODEPOT_REPO" in
  beta) echo "hello from beta"; echo broke >&2; exit 4 ;;
  *) echo "Already up to date."; echo "hello there" ;;
esac"#;

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
        request,
        command,
        targets,
        parallel,
        colors,
    } = &h.state().3[0]
    else {
        panic!("expected a run");
    };
    assert_eq!(
        (*request, command.as_str(), *parallel, *colors),
        (1, "git status --short", 8, true)
    );
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
    assert!(h.query_by_label("rust").is_some());
    assert!(h.query_by_label("2 clones").is_some());
    h.get_by_label("Run").click();
    h.run();
    let RunnerAction::Start { targets, .. } = &h.state().3[0] else {
        panic!("expected a run");
    };
    let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["alpha", "gamma"]);
    assert_eq!(targets[0].tags, ["rust"]);
    h.get_by_label("×").click();
    h.run();
    assert!(h.query_by_label("3 clones").is_some(), "back to all clones");
}

#[test]
fn finished_run_shows_status_and_output() {
    let (_folders, mut h) = page_with_run("view", MIXED);
    h.run();
    assert!(h.query_by_label("hello from beta").is_some());
    assert_eq!(h.query_all_by_label("Already up to date.").count(), 2);
    assert_eq!(
        h.query_all_by_label("✖ 4").count(),
        2,
        "in the table and the output header"
    );

    h.state_mut().0.tabs[0].selected = Some(1);
    h.run();
    assert!(h.query_by_label("broke").is_some());
    assert!(h.query_by_label("Already up to date.").is_none());

    h.get_all_by_label("×").next().unwrap().click();
    h.run();
    assert!(h.state().0.tabs.is_empty());
}

#[test]
fn grouping_identical_output() {
    let (_folders, mut h) = page_with_run("group", MIXED);
    h.get_by_label("Group identical").click();
    h.run();
    assert!(h.query_by_label("×2 alpha, …").is_some());
    assert!(h.query_by_label_contains("── ×2 alpha, gamma").is_some());
    assert_eq!(
        h.query_all_by_label("Already up to date.").count(),
        1,
        "shown once"
    );
    // Ticking the group ticks both.
    h.get_by_label("×2 alpha, …").click();
    h.run();
    assert_eq!(h.state().0.tabs[0].ticked, BTreeSet::from([0, 2]));
}

#[test]
fn search_filters_and_jumps_between_matches() {
    let (_folders, mut h) = page_with_run("search", MIXED);
    h.state_mut().0.tabs[0].search = "hello".into();
    h.run();
    assert!(h.query_by_label("3 matching lines in 3 repos").is_some());
    h.get_by_label("⏷ Next").click();
    h.run();
    assert_eq!(h.state().0.tabs[0].selected, Some(0));
    assert!(h.query_by_label("Line 1 of 3 in 3 repos").is_some());
    h.get_by_label("⏶ Previous").click();
    h.run();
    assert_eq!(h.state().0.tabs[0].selected, Some(2), "wraps around");

    h.state_mut().0.tabs[0].search = "up to date".into();
    h.state_mut().0.tabs[0].search_mode = SearchMode::OnlyNotMatching;
    h.state_mut().0.tabs[0].selected = None;
    h.run();
    assert!(h.query_by_label("hello from beta").is_some());
    assert!(
        h.query_by_label("hello there").is_none(),
        "repos with matches are hidden"
    );

    h.state_mut().0.tabs[0].search = "(".into();
    h.state_mut().0.tabs[0].search_mode = SearchMode::Highlight;
    h.get_by_label("Regex").click();
    h.run();
    assert!(h.query_by_label_contains("Not a valid regex").is_some());
}

#[test]
fn sorting_cycles() {
    let (_folders, mut h) = page_with_run("sort", MIXED);
    h.run();
    h.get_by_label("Lines").click();
    h.run();
    assert!(h.state().0.tabs[0].sort == Some((SortKey::Lines, false)));
    h.get_by_label("Lines ⏶").click();
    h.run();
    assert!(h.state().0.tabs[0].sort == Some((SortKey::Lines, true)));
    h.get_by_label("Lines ⏷").click();
    h.run();
    assert!(h.state().0.tabs[0].sort.is_none());
}

#[test]
fn ticked_repos_run_in_a_new_tab() {
    let (_folders, mut h) = page_with_run("ticks", MIXED);
    h.run();
    h.get_by_label("Tick failed").click();
    h.run();
    h.get_by_label("gamma").click();
    h.run();
    h.state_mut().0.command = "git log -1".into();
    h.get_by_label("Run on ticked (2)").click();
    h.run();
    let Some(RunnerAction::Start {
        request,
        command,
        targets: chosen,
        ..
    }) = h.state_mut().3.pop()
    else {
        panic!("expected a run");
    };
    assert_eq!(command, "git log -1");
    let names: Vec<&str> = chosen.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["beta", "gamma"]);

    // The backend answers with the run; it opens in a new tab with a breadcrumb.
    let run = finished_run(&command, chosen, false);
    h.state_mut().0.run_started(run, Some(request));
    h.run();
    assert_eq!(h.state().0.tabs.len(), 2);
    assert!(
        h.query_by_label_contains("From case \"$REPODEPOT_REPO\"")
            .is_some()
    );
}

#[test]
fn rerunning_failed_repos_replaces_their_rows() {
    let (_folders, mut h) = page_with_run("rerun", MIXED);
    h.run();
    h.get_by_label("Re-run failed (1)").click();
    h.run();
    let Some(RunnerAction::Start {
        request,
        targets: chosen,
        ..
    }) = h.state_mut().3.pop()
    else {
        panic!("expected a run");
    };
    assert_eq!(chosen.len(), 1);
    assert_eq!(chosen[0].name, "beta");

    let run = finished_run("echo fixed", chosen, false);
    h.state_mut().0.run_started(run, Some(request));
    h.run();
    assert_eq!(h.state().0.tabs.len(), 1, "same tab");
    assert!(h.query_by_label("fixed").is_some());
    assert!(h.query_by_label("hello from beta").is_none());
    assert!(!h.state().0.tabs[0].has_failures());
}

#[test]
fn clearing_every_output() {
    let (_folders, mut h) = page_with_run("clear", MIXED);
    h.run();
    h.get_by_label("Clear all outputs").click();
    h.run();
    assert!(h.query_by_label("hello from beta").is_none());
    assert_eq!(h.query_all_by_label("(no output)").count(), 3);
}

#[test]
fn history_and_snippets() {
    let (_folders, clones) = fake_clones("history", &["alpha"]);
    let mut page = RunnerPage::new();
    page.set_history(vec![HistoryEntry {
        command: "cargo update".into(),
        started_at: "2026-11-22 10:00:00".into(),
        repo_count: 3,
        ok_count: 3,
        failed_count: 0,
    }]);
    page.set_snippets(vec![Snippet {
        id: 4,
        name: "Last commit".into(),
        command: "git log -1".into(),
    }]);
    let mut h = harness(
        Harness::builder(),
        (page, clones, Config::default(), Vec::new()),
    );
    h.run();
    h.get_by_label("History").click();
    h.run();
    h.get_by_label("cargo update").click();
    h.run();
    assert_eq!(h.state().0.command, "cargo update");

    h.state_mut().0.snippet_name = "Update deps".into();
    h.run();
    h.get_by_label("Save").click();
    h.run();
    h.get_by_label("Snippets").click();
    h.run();
    h.get_by_label("×").click();
    h.run();
    assert_eq!(
        h.state().3,
        vec![
            RunnerAction::SaveSnippet {
                name: "Update deps".into(),
                command: "cargo update".into()
            },
            RunnerAction::DeleteSnippet(4),
        ]
    );
}

/// Renders the Runner to PNGs when `REPODEPOT_UI_SHOTS` names a folder.
#[test]
fn ui_shots() {
    let Some(dir) = std::env::var_os("REPODEPOT_UI_SHOTS").map(PathBuf::from) else {
        return;
    };
    fs::create_dir_all(&dir).unwrap();
    let (_folders, clones) = fake_clones("shots", &["facebook-react", "serde", "tokio", "notes"]);
    let run = finished_run(
        r#"case "$REPODEPOT_REPO" in
             serde) printf ' \033[31mM\033[0m src/de.rs\n\033[32m??\033[0m scratch.md\n' ;;
             tokio) echo 'fatal: not a git repository' >&2; exit 128 ;;
             *) echo 'Already up to date.' ;;
           esac"#,
        targets(&clones),
        true,
    );
    let mut page = RunnerPage::new();
    page.command = "git status --short".into();
    page.run_started(run, None);
    let mut h = harness(
        Harness::builder().wgpu(),
        (page, clones, Config::default(), Vec::new()),
    );
    h.state_mut().0.tabs[0].search = "src".into();
    h.run();
    h.render()
        .unwrap()
        .save(dir.join("runner-all.png"))
        .unwrap();
    h.state_mut().0.tabs[0].search.clear();
    h.state_mut().0.tabs[0].group = true;
    h.run();
    h.render()
        .unwrap()
        .save(dir.join("runner-grouped.png"))
        .unwrap();
}
