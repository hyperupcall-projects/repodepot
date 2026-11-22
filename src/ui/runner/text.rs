//! Drawing output lines: terminal colors and search highlights.

use eframe::egui::{self, Color32, Stroke, TextFormat, text::LayoutJob};
use regex::Regex;

use crate::jobs::output::{Color, Line, Stream};

/// How a line should be highlighted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Highlight {
    None,
    /// The line has search matches.
    Matches,
    /// The line holds the match the user jumped to.
    Current,
}

/// Lays out one line in monospace, with its colors and any search matches highlighted.
pub fn line_job(
    ui: &egui::Ui,
    line: &Line,
    search: Option<&Regex>,
    highlight: Highlight,
) -> LayoutJob {
    let visuals = ui.visuals();
    let dark = visuals.dark_mode;
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let base = match line.stream {
        Stream::Stdout => visuals.text_color(),
        Stream::Stderr => visuals.error_fg_color.gamma_multiply(0.85),
    };
    let match_bg = match highlight {
        Highlight::Current => visuals.warn_fg_color.gamma_multiply(0.45),
        _ => visuals.selection.bg_fill.gamma_multiply(0.7),
    };

    let text = line.text.as_str();
    let matches: Vec<(usize, usize)> = match (search, highlight) {
        (Some(regex), Highlight::Matches | Highlight::Current) => regex
            .find_iter(text)
            .filter(|m| !m.is_empty())
            .map(|m| (m.start(), m.end()))
            .collect(),
        _ => Vec::new(),
    };
    let mut cuts = vec![0, text.len()];
    for span in &line.spans {
        cuts.extend([span.start, span.end]);
    }
    for (start, end) in &matches {
        cuts.extend([*start, *end]);
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        let style = line
            .spans
            .iter()
            .find(|s| s.start <= start && start < s.end)
            .map(|s| s.style)
            .unwrap_or_default();
        let mut color = style.fg.map_or(base, |c| ansi_color(c, dark));
        if style.bold && style.fg.is_none() {
            color = visuals.strong_text_color();
        }
        if style.dim {
            color = color.gamma_multiply(0.6);
        }
        let in_match = matches.iter().any(|(s, e)| *s <= start && start < *e);
        job.append(
            &text[start..end],
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                italics: style.italic,
                underline: if style.underline {
                    Stroke::new(1.0, color)
                } else {
                    Stroke::NONE
                },
                background: if in_match {
                    match_bg
                } else {
                    Color32::TRANSPARENT
                },
                ..Default::default()
            },
        );
    }
    if job.sections.is_empty() {
        job.append(
            "",
            0.0,
            TextFormat {
                font_id: font,
                color: base,
                ..Default::default()
            },
        );
    }
    job
}

/// Terminal colors, tuned to stay readable on the current background.
pub fn ansi_color(color: Color, dark: bool) -> Color32 {
    match color {
        Color::Ansi(n) => palette(n, dark),
        Color::Indexed(n) if n < 16 => palette(n, dark),
        Color::Indexed(n) if n < 232 => {
            let n = n - 16;
            let level = |v: u8| [0, 95, 135, 175, 215, 255][v as usize];
            Color32::from_rgb(level(n / 36), level(n % 36 / 6), level(n % 6))
        }
        Color::Indexed(n) => {
            let gray = 8 + (n - 232) * 10;
            Color32::from_gray(gray)
        }
        Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

fn palette(n: u8, dark: bool) -> Color32 {
    const DARK: [(u8, u8, u8); 16] = [
        (120, 120, 120),
        (240, 96, 96),
        (110, 200, 110),
        (229, 192, 92),
        (106, 159, 240),
        (198, 120, 221),
        (86, 192, 200),
        (208, 208, 208),
        (150, 150, 150),
        (255, 130, 130),
        (140, 230, 140),
        (250, 220, 120),
        (140, 185, 255),
        (220, 150, 240),
        (120, 220, 230),
        (240, 240, 240),
    ];
    const LIGHT: [(u8, u8, u8); 16] = [
        (40, 40, 40),
        (190, 30, 30),
        (25, 130, 25),
        (150, 110, 0),
        (30, 90, 200),
        (160, 45, 175),
        (15, 125, 140),
        (110, 110, 110),
        (90, 90, 90),
        (220, 50, 50),
        (40, 160, 40),
        (175, 130, 0),
        (50, 110, 230),
        (185, 70, 200),
        (25, 150, 165),
        (70, 70, 70),
    ];
    let (r, g, b) = if dark { DARK } else { LIGHT }[usize::from(n % 16)];
    Color32::from_rgb(r, g, b)
}

pub fn success_color(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(110, 200, 120)
    } else {
        Color32::from_rgb(20, 130, 50)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::output::parse_line;

    #[test]
    fn colors_map_to_rgb() {
        assert_eq!(
            ansi_color(Color::Rgb(1, 2, 3), true),
            Color32::from_rgb(1, 2, 3)
        );
        assert_eq!(
            ansi_color(Color::Indexed(16), true),
            Color32::from_rgb(0, 0, 0)
        );
        assert_eq!(
            ansi_color(Color::Indexed(231), true),
            Color32::from_rgb(255, 255, 255)
        );
        assert_eq!(ansi_color(Color::Indexed(232), true), Color32::from_gray(8));
        assert_eq!(
            ansi_color(Color::Indexed(1), true),
            ansi_color(Color::Ansi(1), true)
        );
        assert_ne!(
            ansi_color(Color::Ansi(1), true),
            ansi_color(Color::Ansi(1), false)
        );
    }

    #[test]
    fn jobs_split_at_colors_and_matches() {
        let mut harness = egui_kittest::Harness::new_ui(|ui| {
            let (text, spans) = parse_line(b"\x1b[31merror\x1b[0m: bad thing");
            let line = Line {
                stream: Stream::Stdout,
                text,
                spans,
            };
            let regex = Regex::new("bad").unwrap();
            let job = line_job(ui, &line, Some(&regex), Highlight::Matches);
            let pieces: Vec<&str> = job
                .sections
                .iter()
                .map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0])
                .collect();
            assert_eq!(pieces, ["error", ": ", "bad", " thing"]);
            assert_ne!(job.sections[2].format.background, Color32::TRANSPARENT);
            assert_eq!(
                job.sections[0].format.color,
                ansi_color(Color::Ansi(1), ui.visuals().dark_mode)
            );
            let plain = line_job(ui, &line, None, Highlight::None);
            assert!(
                plain
                    .sections
                    .iter()
                    .all(|s| s.format.background == Color32::TRANSPARENT)
            );
        });
        harness.run();
    }
}
