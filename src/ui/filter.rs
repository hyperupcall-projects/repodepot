//! The filters every Repositories view shares (all but Import): a search over the owner, name
//! and description, a language filter, and hiding ignored or archived repos.

use eframe::egui;

/// What the search text looks in. At least one is always on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    pub owner: bool,
    pub name: bool,
    pub description: bool,
}

impl Fields {
    /// "Search owner and name", "Search description", …
    fn hint(self) -> String {
        let names: Vec<&str> = [
            (self.owner, "owner"),
            (self.name, "name"),
            (self.description, "description"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        let list = match names.as_slice() {
            [one] => (*one).to_owned(),
            [a, b] => format!("{a} and {b}"),
            [a, b, c] => format!("{a}, {b} and {c}"),
            _ => String::new(),
        };
        format!("Search {list}")
    }
}

/// The shared filters. Views keep it while you switch between them.
pub struct Filter {
    pub text: String,
    pub fields: Fields,
    /// Language names separated by commas; a repo matches if it's in any of them.
    pub language: String,
    pub hide_ignored: bool,
    pub hide_archived: bool,
}

impl Filter {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            fields: Fields {
                owner: true,
                name: true,
                description: false,
            },
            language: String::new(),
            hide_ignored: true,
            hide_archived: false,
        }
    }

    /// Something is typed in that narrows the rows (the toggles don't count).
    pub fn has_text(&self) -> bool {
        !self.text.trim().is_empty() || !self.language.trim().is_empty()
    }

    /// Clears what's typed in, keeping the toggles.
    pub fn clear_text(&mut self) {
        self.text.clear();
        self.language.clear();
    }

    /// Lowercases everything once, ready to test rows. `hide_ignored_applies` is false where
    /// every row is ignored anyway (the Ignored list).
    pub fn matcher(&self, hide_ignored_applies: bool) -> Matcher {
        Matcher {
            terms: self
                .text
                .split_whitespace()
                .map(str::to_lowercase)
                .collect(),
            fields: self.fields,
            languages: languages(&self.language),
            hide_ignored: hide_ignored_applies && self.hide_ignored,
            hide_archived: self.hide_archived,
        }
    }

    /// The search field, the field toggles, the language field and the hide toggles.
    /// `known_languages` fills the language menu; `ignored_hover` is `None` where "Hide
    /// ignored" doesn't apply.
    pub fn controls(
        &mut self,
        ui: &mut egui::Ui,
        known_languages: &[String],
        ignored_hover: Option<&str>,
    ) {
        keep_together(ui, 280.0);
        let label = ui.label("Search");
        ui.add(
            egui::TextEdit::singleline(&mut self.text)
                .hint_text(self.fields.hint())
                .desired_width(220.0),
        )
        .labelled_by(label.id)
        .on_hover_text("Words separated by spaces must all match, in any of the chosen fields.");
        keep_together(ui, 240.0);
        ui.weak("in");
        let before = self.fields;
        ui.toggle_value(&mut self.fields.owner, "Owner")
            .on_hover_text("The user or org that owns the repo");
        ui.toggle_value(&mut self.fields.name, "Name")
            .on_hover_text("The repo's name, and a clone's folder name (react~2)");
        ui.toggle_value(&mut self.fields.description, "Description")
            .on_hover_text("The repo's description on GitHub");
        if !(self.fields.owner || self.fields.name || self.fields.description) {
            // Turning off the last one would leave nothing to search.
            self.fields = before;
        }

        ui.separator();
        keep_together(ui, 220.0);
        self.language_field(ui, known_languages);

        ui.separator();
        if let Some(hover) = ignored_hover {
            ui.checkbox(&mut self.hide_ignored, "Hide ignored")
                .on_hover_text(hover);
        }
        ui.checkbox(&mut self.hide_archived, "Hide archived")
            .on_hover_text("Leave out repos archived on GitHub, and their clones");
    }

    fn language_field(&mut self, ui: &mut egui::Ui, known: &[String]) {
        let label = ui.label("Language");
        ui.add(
            egui::TextEdit::singleline(&mut self.language)
                .hint_text("Rust, Go")
                .desired_width(110.0),
        )
        .labelled_by(label.id)
        .on_hover_text("GitHub's main language for the repo. Separate several with commas.");
        let picked = languages(&self.language);
        // Stays open while languages are ticked, so several can be picked.
        let config = egui::containers::menu::MenuConfig::new()
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let (response, _) = egui::containers::menu::MenuButton::new("⏷")
            .config(config)
            .ui(ui, |ui| {
                if known.is_empty() {
                    ui.weak("No languages yet. Sync with GitHub to fetch them.");
                }
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for language in known {
                            let mut on = picked.contains(&language.to_lowercase());
                            if ui.checkbox(&mut on, language).changed() {
                                self.language = toggle_language(&self.language, language, on);
                            }
                        }
                    });
                if !picked.is_empty() {
                    ui.separator();
                    if ui.button("Any language").clicked() {
                        self.language.clear();
                        ui.close();
                    }
                }
            });
        response.on_hover_text("Pick languages");
        let unknown: Vec<&str> = self
            .language
            .split(',')
            .map(str::trim)
            .filter(|l| !l.is_empty() && !known.iter().any(|k| k.eq_ignore_ascii_case(l)))
            .collect();
        if !unknown.is_empty() && !known.is_empty() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("No repo is in {}", unknown.join(", ")),
            );
        }
    }
}

/// In a wrapping row, starts a new line unless `width` more fits on this one, so a label and
/// its field wrap together.
pub fn keep_together(ui: &mut egui::Ui, width: f32) {
    if ui.available_size_before_wrap().x < width {
        ui.end_row();
    }
}

/// `"Rust, go "` → `["rust", "go"]`.
fn languages(text: &str) -> Vec<String> {
    text.split(',')
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Adds `language` to the comma-separated `text`, or takes it out.
fn toggle_language(text: &str, language: &str, on: bool) -> String {
    let mut list: Vec<&str> = text
        .split(',')
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case(language))
        .collect();
    if on {
        list.push(language);
    }
    list.join(", ")
}

/// What one row tells the filter about itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct Item<'a> {
    /// `None` for clones without a hosted remote.
    pub owner: Option<&'a str>,
    pub name: &'a str,
    /// A clone's folder name (`react~2`), searched along with the name.
    pub folder: Option<&'a str>,
    pub description: Option<&'a str>,
    pub language: Option<&'a str>,
    pub ignored: bool,
    pub archived: bool,
}

/// A [`Filter`] ready to test rows against.
#[derive(Debug)]
pub struct Matcher {
    terms: Vec<String>,
    fields: Fields,
    languages: Vec<String>,
    hide_ignored: bool,
    hide_archived: bool,
}

impl Matcher {
    pub fn matches(&self, item: &Item<'_>) -> bool {
        self.keeps(item) && self.text_matches(std::slice::from_ref(item))
    }

    /// The hide toggles and the language, without the search text.
    pub fn keeps(&self, item: &Item<'_>) -> bool {
        if (self.hide_ignored && item.ignored) || (self.hide_archived && item.archived) {
            return false;
        }
        self.languages.is_empty()
            || item
                .language
                .is_some_and(|l| self.languages.iter().any(|x| x.eq_ignore_ascii_case(l)))
    }

    /// Every search word is found in one of `items` (a fork and its upstream, say).
    pub fn text_matches(&self, items: &[Item<'_>]) -> bool {
        self.terms
            .iter()
            .all(|term| items.iter().any(|item| self.term_matches(term, item)))
    }

    fn term_matches(&self, term: &str, item: &Item<'_>) -> bool {
        let has = |text: &str| text.to_lowercase().contains(term);
        let Fields {
            owner,
            name,
            description,
        } = self.fields;
        // With both on, `facebook/re` matches across the slash.
        (owner
            && name
            && item
                .owner
                .is_some_and(|o| has(&format!("{o}/{}", item.name))))
            || (owner && item.owner.is_some_and(has))
            || (name && (has(item.name) || item.folder.is_some_and(has)))
            || (description && item.description.is_some_and(has))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> Item<'static> {
        Item {
            owner: Some("facebook"),
            name: "react",
            folder: Some("react~2"),
            description: Some("The library for web and native user interfaces"),
            language: Some("JavaScript"),
            ignored: false,
            archived: false,
        }
    }

    fn matches(filter: &Filter, item: &Item<'_>) -> bool {
        filter.matcher(true).matches(item)
    }

    #[test]
    fn searches_owner_and_name_by_default() {
        let mut filter = Filter::new();
        for text in ["", "face", "REACT", "facebook/re", "~2", "react face"] {
            filter.text = text.into();
            assert!(matches(&filter, &item()), "{text}");
        }
        for text in ["library", "javascript", "react vue", "book/x"] {
            filter.text = text.into();
            assert!(!matches(&filter, &item()), "{text}");
        }
    }

    #[test]
    fn each_field_can_be_searched_alone() {
        let mut filter = Filter::new();
        filter.fields = Fields {
            owner: true,
            name: false,
            description: false,
        };
        filter.text = "react".into();
        assert!(!matches(&filter, &item()), "name is off");
        filter.text = "facebook".into();
        assert!(matches(&filter, &item()));
        filter.text = "facebook/react".into();
        assert!(!matches(&filter, &item()), "the full name needs both");

        filter.fields = Fields {
            owner: false,
            name: false,
            description: true,
        };
        filter.text = "native interfaces".into();
        assert!(matches(&filter, &item()));
        filter.text = "facebook".into();
        assert!(!matches(&filter, &item()));
        let no_description = Item {
            description: None,
            ..item()
        };
        filter.text = "native".into();
        assert!(!matches(&filter, &no_description));
    }

    #[test]
    fn clones_without_an_owner_match_by_name_and_folder() {
        let mut filter = Filter::new();
        let notes = Item {
            owner: None,
            name: "notes",
            folder: Some("notes"),
            ..Item::default()
        };
        filter.text = "note".into();
        assert!(matches(&filter, &notes));
        filter.fields.name = false;
        assert!(!matches(&filter, &notes));
    }

    #[test]
    fn languages_match_exactly_any_of_a_list() {
        let mut filter = Filter::new();
        filter.language = "rust, javascript".into();
        assert!(matches(&filter, &item()));
        filter.language = "java".into();
        assert!(!matches(&filter, &item()), "not a prefix match");
        let unknown = Item {
            language: None,
            ..item()
        };
        assert!(!matches(&filter, &unknown));
        filter.language = " , ".into();
        assert!(matches(&filter, &unknown), "an empty list doesn't filter");
    }

    #[test]
    fn hides_ignored_and_archived() {
        let mut filter = Filter::new();
        let ignored = Item {
            ignored: true,
            ..item()
        };
        let archived = Item {
            archived: true,
            ..item()
        };
        assert!(!matches(&filter, &ignored), "hidden by default");
        assert!(
            filter.matcher(false).matches(&ignored),
            "unless it doesn't apply"
        );
        assert!(matches(&filter, &archived), "shown by default");
        filter.hide_ignored = false;
        filter.hide_archived = true;
        assert!(matches(&filter, &ignored));
        assert!(!matches(&filter, &archived));
    }

    #[test]
    fn hint_names_the_fields() {
        let mut fields = Filter::new().fields;
        assert_eq!(fields.hint(), "Search owner and name");
        fields.description = true;
        assert_eq!(fields.hint(), "Search owner, name and description");
        fields.owner = false;
        fields.name = false;
        assert_eq!(fields.hint(), "Search description");
    }

    #[test]
    fn language_menu_adds_and_removes() {
        assert_eq!(toggle_language("", "Rust", true), "Rust");
        assert_eq!(toggle_language("Rust", "Go", true), "Rust, Go");
        assert_eq!(toggle_language("rust , Go", "Rust", false), "Go");
        assert_eq!(toggle_language("Go,", "Go", false), "");
    }
}
