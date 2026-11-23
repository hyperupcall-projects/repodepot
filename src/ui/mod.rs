pub mod import;
pub mod remote_list;
pub mod repos;
pub mod runner;
pub mod settings;
pub mod tags;

use eframe::egui;

/// A column title that sorts its table. The first click sorts by this column (ascending, or
/// descending when `starts_descending`); each further click reverses the order.
pub fn sort_header<K: PartialEq + Copy>(
    ui: &mut egui::Ui,
    label: &str,
    key: K,
    sort: &mut K,
    descending: &mut bool,
    starts_descending: bool,
) {
    let arrow = match (*sort == key, *descending) {
        (true, false) => " ⏶",
        (true, true) => " ⏷",
        (false, _) => "",
    };
    let button =
        egui::Button::new(egui::RichText::new(format!("{label}{arrow}")).strong()).frame(false);
    if ui.add(button).on_hover_text("Sort").clicked() {
        if *sort == key {
            *descending = !*descending;
        } else {
            *sort = key;
            *descending = starts_descending;
        }
    }
}

/// `12345` → `"12,345"`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::thousands;

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(4987), "4,987");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
