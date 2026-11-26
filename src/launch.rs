//! Opening repos in editors and other apps: presets, detection, command lines and
//! workspace files.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// How an app opens several folders in one window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    /// Folders are passed straight on the command line (`{paths}`), or it can't.
    #[default]
    None,
    /// A `.code-workspace` file (VS Code and its forks).
    VsCode,
    /// A `.sublime-project` file.
    Sublime,
}

impl WorkspaceKind {
    pub const ALL: [WorkspaceKind; 3] = [
        WorkspaceKind::None,
        WorkspaceKind::VsCode,
        WorkspaceKind::Sublime,
    ];

    pub fn label(self) -> &'static str {
        match self {
            WorkspaceKind::None => "no file",
            WorkspaceKind::VsCode => ".code-workspace",
            WorkspaceKind::Sublime => ".sublime-project",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            WorkspaceKind::None => "",
            WorkspaceKind::VsCode => "code-workspace",
            WorkspaceKind::Sublime => "sublime-project",
        }
    }
}

/// An app that can open repos. Arguments use `{path}` (one folder), `{paths}` (every folder,
/// as separate arguments) and `{workspace}` (a workspace file the app writes first).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LauncherSpec {
    pub name: String,
    /// Programs to look for on PATH; the first one found is used.
    pub programs: Vec<String>,
    /// Flatpak app ID to fall back to when none of `programs` is on PATH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flatpak: Option<String>,
    /// Other programs that must be on PATH too (e.g. `nvim` for "Neovim in Konsole").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// Arguments to open one repo in its own window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub each: Option<String>,
    /// Arguments to open several repos in one window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub together: Option<String>,
    #[serde(default)]
    pub workspace: WorkspaceKind,
}

fn preset(
    name: &str,
    programs: &[&str],
    flatpak: Option<&str>,
    each: Option<&str>,
    together: Option<(&str, WorkspaceKind)>,
) -> LauncherSpec {
    LauncherSpec {
        name: name.to_owned(),
        programs: programs.iter().map(|p| (*p).to_owned()).collect(),
        flatpak: flatpak.map(str::to_owned),
        requires: Vec::new(),
        each: each.map(str::to_owned),
        together: together.map(|(args, _)| args.to_owned()),
        workspace: together.map_or(WorkspaceKind::None, |(_, kind)| kind),
    }
}

/// Built-in launchers. Flags checked against each app's `--help`.
pub fn presets() -> Vec<LauncherSpec> {
    use WorkspaceKind::{Sublime, VsCode};
    let mut list = vec![
        preset(
            "VS Code",
            &["code"],
            Some("com.visualstudio.code"),
            Some("-n {path}"),
            Some(("-n {workspace}", VsCode)),
        ),
        preset(
            "VSCodium",
            &["codium"],
            Some("com.vscodium.codium"),
            Some("-n {path}"),
            Some(("-n {workspace}", VsCode)),
        ),
        preset(
            "Cursor",
            &["cursor"],
            None,
            Some("-n {path}"),
            Some(("-n {workspace}", VsCode)),
        ),
        preset(
            "Zed",
            &["zed", "zeditor", "zedit"],
            Some("dev.zed.Zed"),
            Some("-n {path}"),
            Some(("-n {paths}", WorkspaceKind::None)),
        ),
        preset(
            "Sublime Text",
            &["subl"],
            Some("com.sublimetext.three"),
            Some("-n {path}"),
            Some(("--project {workspace}", Sublime)),
        ),
        preset(
            "IntelliJ IDEA",
            &["idea", "intellij-idea-ultimate", "intellij-idea-community"],
            Some("com.jetbrains.IntelliJ-IDEA-Community"),
            Some("{path}"),
            None,
        ),
        preset(
            "RustRover",
            &["rustrover"],
            Some("com.jetbrains.RustRover"),
            Some("{path}"),
            None,
        ),
        preset(
            "PyCharm",
            &["pycharm", "pycharm-community", "charm"],
            Some("com.jetbrains.PyCharm-Community"),
            Some("{path}"),
            None,
        ),
        preset(
            "CLion",
            &["clion"],
            Some("com.jetbrains.CLion"),
            Some("{path}"),
            None,
        ),
        preset(
            "GoLand",
            &["goland"],
            Some("com.jetbrains.GoLand"),
            Some("{path}"),
            None,
        ),
        preset(
            "WebStorm",
            &["webstorm"],
            Some("com.jetbrains.WebStorm"),
            Some("{path}"),
            None,
        ),
        preset(
            "Kate",
            &["kate"],
            Some("org.kde.kate"),
            Some("-n {path}"),
            None,
        ),
        preset(
            "Emacs",
            &["emacs"],
            Some("org.gnu.emacs"),
            Some("{path}"),
            None,
        ),
        preset(
            "Dolphin",
            &["dolphin"],
            Some("org.kde.dolphin"),
            Some("--new-window {path}"),
            None,
        ),
        preset(
            "Konsole",
            &["konsole"],
            None,
            Some("--workdir {path}"),
            None,
        ),
        preset("kitty", &["kitty"], None, Some("--directory {path}"), None),
    ];
    for (editor, program) in [("Neovim", "nvim"), ("Helix", "hx")] {
        for (terminal, args) in [
            ("Konsole", format!("--workdir {{path}} -e {program} .")),
            ("kitty", format!("--directory {{path}} {program} .")),
        ] {
            let mut spec = preset(
                &format!("{editor} in {terminal}"),
                &[&terminal.to_lowercase()],
                None,
                Some(&args),
                None,
            );
            spec.requires = vec![program.to_owned()];
            list.push(spec);
        }
    }
    list
}

/// A launcher whose program was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launcher {
    pub spec: LauncherSpec,
    /// The program, or `flatpak run <id>`.
    pub program: Vec<String>,
}

impl Launcher {
    pub fn can_open_each(&self) -> bool {
        self.spec.each.is_some()
    }

    pub fn can_open_together(&self) -> bool {
        self.spec.together.is_some()
    }

    /// The command line for one repo.
    pub fn each_command(&self, path: &Path) -> Result<Vec<String>> {
        let args = self
            .spec
            .each
            .as_deref()
            .context("this app can't open one repo")?;
        self.command(args, &[path.to_path_buf()], None)
    }

    /// The command line for several repos in one window. Apps that use a workspace file
    /// need `workspace` (see [`write_workspace`]).
    pub fn together_command(
        &self,
        paths: &[PathBuf],
        workspace: Option<&Path>,
    ) -> Result<Vec<String>> {
        let args = self
            .spec
            .together
            .as_deref()
            .context("this app can't open several repos in one window")?;
        self.command(args, paths, workspace)
    }

    fn command(
        &self,
        args: &str,
        paths: &[PathBuf],
        workspace: Option<&Path>,
    ) -> Result<Vec<String>> {
        let mut command = self.program.clone();
        let text = |p: &Path| p.display().to_string();
        for arg in split_args(args)? {
            if arg == "{paths}" {
                command.extend(paths.iter().map(|p| text(p)));
                continue;
            }
            let mut arg = arg;
            if arg.contains("{path}") {
                let first = paths.first().context("no folder to open")?;
                arg = arg.replace("{path}", &text(first));
            }
            if arg.contains("{workspace}") {
                let file = workspace.context("this app needs a workspace file")?;
                arg = arg.replace("{workspace}", &text(file));
            }
            command.push(arg);
        }
        Ok(command)
    }
}

/// Finds which launchers are installed, keeping their order.
pub fn detect(specs: &[LauncherSpec]) -> Vec<Launcher> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path_var).collect();
    let flatpak_dirs: Vec<PathBuf> = [
        Some(PathBuf::from("/var/lib/flatpak/app")),
        directories::BaseDirs::new().map(|d| d.data_dir().join("flatpak/app")),
    ]
    .into_iter()
    .flatten()
    .collect();
    detect_in(specs, &dirs, &flatpak_dirs)
}

fn detect_in(
    specs: &[LauncherSpec],
    path_dirs: &[PathBuf],
    flatpak_dirs: &[PathBuf],
) -> Vec<Launcher> {
    let on_path = |program: &str| {
        if program.contains('/') {
            is_executable(Path::new(program))
        } else {
            path_dirs.iter().any(|d| is_executable(&d.join(program)))
        }
    };
    let has_flatpak =
        |id: &str| flatpak_dirs.iter().any(|d| d.join(id).is_dir()) && on_path("flatpak");
    specs
        .iter()
        .filter(|spec| spec.requires.iter().all(|r| on_path(r)))
        .filter_map(|spec| {
            let program = match spec.programs.iter().find(|p| on_path(p)) {
                Some(program) => vec![program.clone()],
                None => {
                    let id = spec.flatpak.as_deref().filter(|id| has_flatpak(id))?;
                    vec!["flatpak".to_owned(), "run".to_owned(), id.to_owned()]
                }
            };
            Some(Launcher {
                spec: spec.clone(),
                program,
            })
        })
        .collect()
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Splits arguments on spaces, keeping 'single' or "double" quoted parts together.
pub fn split_args(text: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        bail!("a quote isn't closed in: {text}");
    }
    if started {
        args.push(current);
    }
    Ok(args)
}

/// Checks a launcher typed into Settings. Returns what's wrong, if anything.
pub fn validate(spec: &LauncherSpec) -> Result<(), String> {
    if spec.name.trim().is_empty() {
        return Err("needs a name".into());
    }
    if spec.programs.iter().all(|p| p.trim().is_empty()) {
        return Err("needs a program".into());
    }
    if spec.each.is_none() && spec.together.is_none() {
        return Err("needs arguments for opening each repo, or several together".into());
    }
    if let Some(each) = &spec.each {
        split_args(each).map_err(|e| e.to_string())?;
        if !each.contains("{path}") {
            return Err("its \"each\" arguments need {path}".into());
        }
    }
    if let Some(together) = &spec.together {
        split_args(together).map_err(|e| e.to_string())?;
        match spec.workspace {
            WorkspaceKind::None if !together.contains("{paths}") => {
                return Err("its \"together\" arguments need {paths}".into());
            }
            WorkspaceKind::VsCode | WorkspaceKind::Sublime if !together.contains("{workspace}") => {
                return Err("its \"together\" arguments need {workspace}".into());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Writes (or refreshes) the workspace file for a group of repos and returns its path.
/// The name is stable for the same group, so the editor brings back its layout.
pub fn write_workspace(
    dir: &Path,
    kind: WorkspaceKind,
    name: &str,
    folders: &[(String, PathBuf)],
) -> Result<PathBuf> {
    if kind == WorkspaceKind::None {
        bail!("this app doesn't use workspace files");
    }
    fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    let folders: Vec<serde_json::Value> = folders
        .iter()
        .map(|(name, path)| serde_json::json!({ "name": name, "path": path }))
        .collect();
    let content = serde_json::to_string_pretty(&serde_json::json!({ "folders": folders }))?;
    let path = dir.join(format!("{}.{}", slug(name), kind.extension()));
    if fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, &content).with_context(|| format!("couldn't write {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("couldn't write {}", path.display()))?;
    }
    Ok(path)
}

/// A file-name-safe version of a tag, query or set name.
pub fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_') {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(60).collect();
    if slug.is_empty() || slug.starts_with('.') {
        format!("set{slug}")
    } else {
        slug
    }
}

/// The workspace name for a hand-picked set of clones: `set-<hash of their IDs>`.
pub fn set_name(clone_ids: &[i64]) -> String {
    let mut ids = clone_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    // FNV-1a, so the name stays the same across Rust versions.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in ids.iter().flat_map(|id| id.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("set-{hash:016x}")
}

/// Removes every workspace file the app wrote. Returns how many.
pub fn delete_workspaces(dir: &Path) -> Result<usize> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(0);
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = path
            .extension()
            .is_some_and(|e| e == "code-workspace" || e == "sublime-project");
        if ours && path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("couldn't remove {}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("repodepot-launch-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_program(dir: &Path, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn argument_splitting() {
        assert_eq!(split_args("-n {path}").unwrap(), ["-n", "{path}"]);
        assert_eq!(
            split_args("  -e 'nvim .'  \"a b\" ''").unwrap(),
            ["-e", "nvim .", "a b", ""]
        );
        assert!(split_args("-e 'oops").is_err());
    }

    #[test]
    fn detection_prefers_path_then_flatpak_and_checks_requirements() {
        let base = temp("detect");
        let bin = base.join("bin");
        let flatpaks = base.join("flatpak");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(flatpaks.join("org.kde.kate")).unwrap();
        fs::create_dir_all(flatpaks.join("com.visualstudio.code")).unwrap();
        for program in ["zeditor", "konsole", "flatpak", "code"] {
            fake_program(&bin, program);
        }
        fs::write(bin.join("subl"), "not executable").unwrap();

        let found = detect_in(
            &presets(),
            std::slice::from_ref(&bin),
            std::slice::from_ref(&flatpaks),
        );
        let names: Vec<&str> = found.iter().map(|l| l.spec.name.as_str()).collect();
        assert_eq!(names, ["VS Code", "Zed", "Kate", "Konsole"]);
        assert_eq!(found[0].program, ["code"], "PATH wins over Flatpak");
        assert_eq!(found[1].program, ["zeditor"], "alternative program names");
        assert_eq!(found[2].program, ["flatpak", "run", "org.kde.kate"]);
        // A full path works too.
        let mine = preset(
            "Mine",
            &[bin.join("code").to_str().unwrap()],
            None,
            Some("{path}"),
            None,
        );
        assert_eq!(detect_in(&[mine], &[], &[]).len(), 1);
        // "Neovim in Konsole" needs nvim too.
        fake_program(&bin, "nvim");
        let found = detect_in(&presets(), &[bin], &[flatpaks]);
        assert!(found.iter().any(|l| l.spec.name == "Neovim in Konsole"));
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn command_lines() {
        let specs = presets();
        let launcher = |name: &str, program: &str| Launcher {
            spec: specs.iter().find(|s| s.name == name).unwrap().clone(),
            program: vec![program.into()],
        };
        let paths = [
            PathBuf::from("/code/r/react"),
            PathBuf::from("/code/s/serde"),
        ];
        assert_eq!(
            launcher("VS Code", "code").each_command(&paths[0]).unwrap(),
            ["code", "-n", "/code/r/react"]
        );
        assert_eq!(
            launcher("Zed", "zed")
                .together_command(&paths, None)
                .unwrap(),
            ["zed", "-n", "/code/r/react", "/code/s/serde"]
        );
        let ws = PathBuf::from("/ws/rust.code-workspace");
        assert_eq!(
            launcher("VS Code", "code")
                .together_command(&paths, Some(&ws))
                .unwrap(),
            ["code", "-n", "/ws/rust.code-workspace"]
        );
        assert!(
            launcher("VS Code", "code")
                .together_command(&paths, None)
                .is_err()
        );
        assert!(
            launcher("Kate", "kate")
                .together_command(&paths, None)
                .is_err()
        );
        assert_eq!(
            launcher("Neovim in Konsole", "konsole")
                .each_command(&paths[0])
                .unwrap(),
            ["konsole", "--workdir", "/code/r/react", "-e", "nvim", "."]
        );
    }

    #[test]
    fn workspace_files() {
        let dir = temp("workspace");
        let folders = vec![
            ("facebook/react".to_owned(), PathBuf::from("/code/r/react")),
            ("serde-rs/serde".to_owned(), PathBuf::from("/code/s/serde")),
        ];
        let path =
            write_workspace(&dir, WorkspaceKind::VsCode, "lang:rust AND work", &folders).unwrap();
        assert_eq!(path, dir.join("lang-rust-and-work.code-workspace"));
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["folders"][0]["name"], "facebook/react");
        assert_eq!(json["folders"][1]["path"], "/code/s/serde");
        let sublime = write_workspace(&dir, WorkspaceKind::Sublime, "*", &folders).unwrap();
        assert_eq!(sublime, dir.join("set.sublime-project"));
        assert!(write_workspace(&dir, WorkspaceKind::None, "x", &folders).is_err());

        fs::write(dir.join("keep.txt"), "").unwrap();
        assert_eq!(delete_workspaces(&dir).unwrap(), 2);
        assert!(dir.join("keep.txt").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_for_sets() {
        assert_eq!(set_name(&[3, 1, 2]), set_name(&[1, 2, 3, 3]));
        assert_ne!(set_name(&[1, 2]), set_name(&[1, 3]));
        assert!(set_name(&[1]).starts_with("set-"));
        assert_eq!(slug("  Work / Rust "), "work-rust");
        assert_eq!(slug("..."), "set...");
    }

    #[test]
    fn custom_launchers_are_checked() {
        let mut spec = preset("Mine", &["mine"], None, Some("{path}"), None);
        assert!(validate(&spec).is_ok());
        spec.each = Some("--open".into());
        assert!(validate(&spec).unwrap_err().contains("{path}"));
        spec.each = None;
        assert!(validate(&spec).is_err());
        spec.together = Some("{workspace}".into());
        assert!(validate(&spec).unwrap_err().contains("{paths}"));
        spec.workspace = WorkspaceKind::VsCode;
        assert!(validate(&spec).is_ok());
        spec.name = " ".into();
        assert!(validate(&spec).is_err());
    }
}
