pub mod filter;
pub mod import;
pub mod log;
pub mod remote_list;
pub mod repos;
pub mod runner;
pub mod selection;
pub mod settings;
pub mod tags;

use std::cmp::Ordering;

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

/// Case-insensitive, without allocating.
pub fn cmp_text(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

/// Orders present values with `cmp` (reversed when `descending`) and puts missing ones last
/// either way.
pub fn present_first<T>(
    a: Option<T>,
    b: Option<T>,
    descending: bool,
    cmp: impl FnOnce(&T, &T) -> Ordering,
) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            let order = cmp(&a, &b);
            if descending { order.reverse() } else { order }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// `"42 ms"`, `"1.5 s"`, `"2m 05s"`.
pub fn short_duration(duration: std::time::Duration) -> String {
    let secs = duration.as_secs_f32();
    if secs >= 60.0 {
        format!(
            "{}m {:02}s",
            duration.as_secs() / 60,
            duration.as_secs() % 60
        )
    } else if secs >= 1.0 {
        format!("{secs:.1} s")
    } else {
        format!("{} ms", duration.as_millis())
    }
}

/// `"1 repo"`, `"1,203 repos"`.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        thousands(n as u64),
        if n == 1 { one } else { many }
    )
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
