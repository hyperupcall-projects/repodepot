pub mod settings;

use eframe::egui;

/// A centered note for tabs that aren't built yet.
pub fn placeholder(ui: &mut egui::Ui, title: &str, note: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.35);
        ui.heading(title);
        ui.weak(note);
    });
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
