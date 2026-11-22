//! Settings stored in `~/.config/depository/config.toml`, plus the app's file locations.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Where the app keeps its files.
#[derive(Clone, Debug)]
pub struct Paths {
    pub config_file: PathBuf,
    pub db_file: PathBuf,
    pub workspaces_dir: PathBuf,
}

impl Paths {
    /// XDG locations: `~/.config/depository/` and `~/.local/share/depository/`.
    pub fn from_system() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "depository")
            .context("couldn't find the home directory")?;
        Ok(Self {
            config_file: dirs.config_dir().join("config.toml"),
            db_file: dirs.data_dir().join("depository.db"),
            workspaces_dir: dirs.data_dir().join("workspaces"),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Folder that holds the `<letter>/<repo>` layout. `None` until the user picks one.
    pub clone_root: Option<PathBuf>,
    pub remotes: Remotes,
    pub github: Github,
    pub clone: CloneDefaults,
    pub runner: Runner,
    pub launch: Launch,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Remotes {
    /// Name of the remote a clone is made from (`git clone -o <name>`).
    pub default_name: String,
    /// Name of the remote that points at a linked fork's parent.
    pub upstream_name: String,
}

impl Default for Remotes {
    fn default() -> Self {
        Self {
            default_name: "me".into(),
            upstream_name: "upstream".into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Github {
    pub sync_on_startup: bool,
    /// SSH host aliases (from `~/.ssh/config`) that mean github.com, e.g. `github-work`.
    pub ssh_host_aliases: Vec<String>,
    pub sources: Sources,
}

/// Which GitHub sources count as "relevant". Repos and owners added by hand are always included.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sources {
    pub owned: bool,
    pub org_member: bool,
    pub collaborator: bool,
    pub contributed: bool,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            owned: true,
            org_member: true,
            collaborator: true,
            contributed: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CloneDefaults {
    /// `--filter=blob:none`
    pub partial: bool,
    /// `--recurse-submodules`
    pub recurse_submodules: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Runner {
    /// Shell used to run commands. `None` means `$SHELL`, falling back to `/bin/sh`.
    pub shell: Option<String>,
    /// How many commands run at once.
    pub parallel: u32,
    /// Per-repo timeout in seconds. 0 means no timeout.
    pub timeout_secs: u64,
    /// Lines kept per repo before older ones are dropped.
    pub line_limit: u32,
}

impl Default for Runner {
    fn default() -> Self {
        Self {
            shell: None,
            parallel: 8,
            timeout_secs: 0,
            line_limit: 10_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Launch {
    /// Ask before opening more than this many windows at once.
    pub confirm_over: u32,
    /// Launchers added in Settings, listed before the built-in ones.
    pub custom: Vec<crate::launch::LauncherSpec>,
}

impl Default for Launch {
    fn default() -> Self {
        Self {
            confirm_over: 10,
            custom: Vec::new(),
        }
    }
}

/// Loads the config file. A missing file is created with defaults.
/// On any problem the defaults are returned along with a message for the UI.
pub fn load(path: &Path) -> (Config, Option<String>) {
    match fs::read_to_string(path) {
        Ok(text) => match toml::from_str(&text) {
            Ok(config) => (config, None),
            Err(e) => (
                Config::default(),
                Some(format!(
                    "{} couldn't be read, so defaults are in use. Saving will overwrite it.\n{e}",
                    path.display()
                )),
            ),
        },
        Err(e) if e.kind() == ErrorKind::NotFound => {
            let config = Config::default();
            let message = save(path, &config)
                .err()
                .map(|e| format!("Couldn't create {}: {e:#}", path.display()));
            (config, message)
        }
        Err(e) => (
            Config::default(),
            Some(format!(
                "Couldn't open {}, so defaults are in use: {e}",
                path.display()
            )),
        ),
    }
}

/// Writes the config file atomically (write to a temp file, then rename).
pub fn save(path: &Path, config: &Config) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    }
    let text = toml::to_string_pretty(config).context("couldn't serialize the settings")?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).with_context(|| format!("couldn't write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("couldn't replace {}", path.display()))?;
    Ok(())
}

/// Expands a leading `~` or `~/` to the home directory.
pub fn expand_tilde(input: &str) -> PathBuf {
    let rest = if input == "~" {
        Some("")
    } else {
        input.strip_prefix("~/")
    };
    if let Some(rest) = rest
        && let Some(dirs) = directories::BaseDirs::new()
    {
        let home = dirs.home_dir();
        return if rest.is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        };
    }
    PathBuf::from(input)
}

/// Checks a git remote name. Kept stricter than git itself so names stay shell-friendly.
pub fn validate_remote_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("can't be empty".into());
    }
    if name.starts_with('-') || name.starts_with('.') {
        return Err("can't start with '-' or '.'".into());
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(format!(
            "can't contain '{bad}' (use letters, digits, '-', '_', '.')"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_through_toml() {
        let config = Config::default();
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(config, back);
    }

    #[test]
    fn config_with_clone_root_round_trips() {
        let config = Config {
            clone_root: Some(PathBuf::from("/home/someone/code")),
            github: Github {
                ssh_host_aliases: vec!["github-work".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(config, back);
    }

    #[test]
    fn saved_config_loads_back_unchanged() {
        let dir = std::env::temp_dir().join(format!("depository-test-{}", std::process::id()));
        let path = dir.join("nested").join("config.toml");
        let config = Config {
            clone_root: Some(PathBuf::from("/srv/code")),
            remotes: Remotes {
                default_name: "mine".into(),
                upstream_name: "up".into(),
            },
            ..Default::default()
        };
        save(&path, &config).unwrap();
        let (loaded, error) = load(&path);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(error, None);
        assert_eq!(loaded, config);
    }

    #[test]
    fn broken_file_falls_back_to_defaults_with_a_message() {
        let dir = std::env::temp_dir().join(format!("depository-test-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "clone_root = [this is not toml").unwrap();
        let (loaded, error) = load(&path);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(loaded, Config::default());
        assert!(error.is_some());
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let config: Config = toml::from_str("[remotes]\ndefault_name = \"origin\"\n").unwrap();
        assert_eq!(config.remotes.default_name, "origin");
        assert_eq!(config.remotes.upstream_name, "upstream");
        assert_eq!(config.runner.parallel, 8);
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/code"), home.join("code"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_tilde("~other/x"), PathBuf::from("~other/x"));
    }

    #[test]
    fn remote_names_are_checked() {
        assert!(validate_remote_name("me").is_ok());
        assert!(validate_remote_name("up-stream_2.x").is_ok());
        assert!(validate_remote_name("").is_err());
        assert!(validate_remote_name("-me").is_err());
        assert!(validate_remote_name("my remote").is_err());
        assert!(validate_remote_name("a/b").is_err());
    }
}
