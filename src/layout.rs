//! The clone root's folder layout: `<letter>/<repo name>`, with `~2`, `~3`, … added on clashes.

/// The letter folder for a repo name: its first character lowercased, or `_` when that
/// isn't an ASCII letter or digit (`.github` → `_`).
pub fn letter_for(name: &str) -> char {
    match name.chars().next() {
        Some(c) if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
        _ => '_',
    }
}

/// Splits a folder name into the repo name and its clash number: `react~2` → (`react`, Some(2)).
/// GitHub doesn't allow `~` in repo names, so a `~N` ending is always ours.
pub fn split_suffix(folder: &str) -> (&str, Option<u32>) {
    if let Some((base, number)) = folder.rsplit_once('~')
        && !base.is_empty()
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
        && let Ok(number) = number.parse::<u32>()
        && number >= 2
    {
        return (base, Some(number));
    }
    (folder, None)
}

/// Where a repo belongs, ignoring clashes: `react` → `r/react`.
pub fn expected_path(name: &str) -> String {
    format!("{}/{}", letter_for(name), name)
}

/// Whether a clone at `rel` (relative to the clone root) is where the layout puts `name`:
/// `<letter>/<name>`, optionally with a `~N` suffix, ignoring case in the name.
pub fn is_in_place(rel: &str, name: &str) -> bool {
    let Some((letter, folder)) = rel.split_once('/') else {
        return false;
    };
    let (base, _) = split_suffix(folder);
    !folder.contains('/')
        && letter == letter_for(name).to_string()
        && base.eq_ignore_ascii_case(name)
}

/// Picks the folder for a new clone: `r/react`, or `r/react~2`, `r/react~3`, … when taken.
/// `taken` is asked with candidate paths and should compare without regard to case.
pub fn assign_path(name: &str, mut taken: impl FnMut(&str) -> bool) -> String {
    let letter = letter_for(name);
    let first = format!("{letter}/{name}");
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| format!("{letter}/{name}~{n}"))
        .find(|candidate| !taken(candidate))
        .expect("some suffix is free")
}

/// Whether `<root>/<rel>` exists, ignoring case in the last component.
pub fn exists_ignoring_case(root: &std::path::Path, rel: &str) -> bool {
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
        return false;
    };
    entries
        .flatten()
        .any(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
}

/// Whether a folder directly under the clone root is a letter folder (one ASCII letter, digit or `_`).
pub fn is_letter_folder(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(c), None) if c.is_ascii_alphanumeric() || c == '_'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters() {
        assert_eq!(letter_for("react"), 'r');
        assert_eq!(letter_for("React"), 'r');
        assert_eq!(letter_for("3d-engine"), '3');
        assert_eq!(letter_for(".github"), '_');
        assert_eq!(letter_for("_private"), '_');
        assert_eq!(letter_for("-dash"), '_');
        assert_eq!(letter_for(""), '_');
    }

    #[test]
    fn suffixes() {
        assert_eq!(split_suffix("react"), ("react", None));
        assert_eq!(split_suffix("react~2"), ("react", Some(2)));
        assert_eq!(split_suffix("react~13"), ("react", Some(13)));
        assert_eq!(split_suffix("react~1"), ("react~1", None));
        assert_eq!(split_suffix("react~"), ("react~", None));
        assert_eq!(split_suffix("react~x"), ("react~x", None));
        assert_eq!(split_suffix("~2"), ("~2", None));
    }

    #[test]
    fn expected_paths() {
        assert_eq!(expected_path("react"), "r/react");
        assert_eq!(expected_path(".github"), "_/.github");
    }

    #[test]
    fn in_place() {
        assert!(is_in_place("r/react", "react"));
        assert!(is_in_place("r/react~2", "react"));
        assert!(is_in_place("r/React", "react"));
        assert!(is_in_place("_/.github", ".github"));
        assert!(!is_in_place("react", "react"));
        assert!(!is_in_place("R/react", "react"));
        assert!(!is_in_place("x/react", "react"));
        assert!(!is_in_place("r/react-old", "react"));
        assert!(!is_in_place("r/sub/react", "react"));
    }

    #[test]
    fn path_assignment() {
        let taken = |list: &'static [&'static str]| {
            move |candidate: &str| list.iter().any(|t| t.eq_ignore_ascii_case(candidate))
        };
        assert_eq!(assign_path("react", taken(&[])), "r/react");
        assert_eq!(assign_path("React", taken(&["r/react"])), "r/React~2");
        assert_eq!(
            assign_path("react", taken(&["r/react", "r/react~2", "r/react~3"])),
            "r/react~4"
        );
        // A real repo called react2 can't collide with a suffix.
        assert_eq!(assign_path("react2", taken(&["r/react"])), "r/react2");
        assert_eq!(assign_path(".github", taken(&[])), "_/.github");
    }

    #[test]
    fn existence_ignores_case() {
        let root = std::env::temp_dir().join(format!("depository-layout-{}", std::process::id()));
        std::fs::create_dir_all(root.join("r/React")).unwrap();
        assert!(exists_ignoring_case(&root, "r/react"));
        assert!(exists_ignoring_case(&root, "r/REACT"));
        assert!(!exists_ignoring_case(&root, "r/react~2"));
        assert!(!exists_ignoring_case(&root, "x/react"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn letter_folders() {
        assert!(is_letter_folder("r"));
        assert!(is_letter_folder("_"));
        assert!(is_letter_folder("7"));
        assert!(is_letter_folder("R"));
        assert!(!is_letter_folder("rs"));
        assert!(!is_letter_folder(""));
        assert!(!is_letter_folder("."));
    }
}
