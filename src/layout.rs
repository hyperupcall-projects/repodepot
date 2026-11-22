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
