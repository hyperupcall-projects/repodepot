//! Captured output: turning raw bytes into displayable lines (keeping colors as styled spans),
//! and keeping a bounded number of them.

use std::collections::VecDeque;

/// Longest line kept, in bytes. Longer runs without a newline are split.
pub const MAX_LINE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// A terminal color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// One of the 16 standard colors (8–15 are the bright ones).
    Ansi(u8),
    /// The 256-color palette.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Style {
    fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// A styled stretch of a line: `start..end` are byte offsets into its text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Line {
    pub stream: Stream,
    pub text: String,
    /// Styled parts, in order and not overlapping. Empty for plain lines.
    pub spans: Vec<Span>,
}

/// The last `limit` lines of a process's output.
#[derive(Debug)]
pub struct OutputBuffer {
    lines: VecDeque<Line>,
    limit: usize,
    dropped: usize,
}

impl OutputBuffer {
    pub fn new(limit: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            limit: limit.max(1),
            dropped: 0,
        }
    }

    pub fn push(&mut self, stream: Stream, (text, spans): (String, Vec<Span>)) {
        if self.lines.len() == self.limit {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(Line {
            stream,
            text,
            spans,
        });
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Line> {
        self.lines.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter()
    }

    /// How many older lines were thrown away to stay under the limit (or by clearing).
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Forgets the output so far.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.dropped = 0;
    }
}

/// Plain text for a message the app itself adds to the output.
pub fn plain(text: impl Into<String>) -> (String, Vec<Span>) {
    (text.into(), Vec::new())
}

/// Makes one line of raw output displayable:
/// - invalid UTF-8 is replaced
/// - after a carriage return only the last segment is kept, like a terminal shows
/// - SGR escape sequences become styled spans; other escapes (cursor moves, titles) are removed
/// - tabs become spaces (tab stops every 8 columns); other control characters are dropped
pub fn parse_line(raw: &[u8]) -> (String, Vec<Span>) {
    let text = String::from_utf8_lossy(raw);
    let text = text.strip_suffix('\r').unwrap_or(&text);
    let text = match text.rfind('\r') {
        Some(i) => &text[i + 1..],
        None => text,
    };

    let mut out = String::with_capacity(text.len());
    let mut spans = Vec::new();
    let mut style = Style::default();
    let mut span_start = 0;
    let mut column = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                if let Some(params) = read_escape(&mut chars) {
                    let new = apply_sgr(style, &params);
                    if new != style {
                        close_span(&mut spans, span_start, out.len(), style);
                        style = new;
                        span_start = out.len();
                    }
                }
            }
            '\t' => {
                let spaces = 8 - column % 8;
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            c if c.is_control() => {}
            c => {
                out.push(c);
                column += 1;
            }
        }
    }
    close_span(&mut spans, span_start, out.len(), style);
    (out, spans)
}

/// Just the text of a line, without styles.
#[cfg(test)]
pub fn clean_line(raw: &[u8]) -> String {
    parse_line(raw).0
}

fn close_span(spans: &mut Vec<Span>, start: usize, end: usize, style: Style) {
    if end > start && !style.is_plain() {
        spans.push(Span { start, end, style });
    }
}

/// Reads the rest of an escape sequence whose ESC was just read. Returns the parameters of
/// an SGR sequence (`ESC [ … m`); every other sequence is skipped and gives `None`.
fn read_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<Vec<u16>> {
    match chars.next() {
        // CSI: parameters and intermediates, then one final byte in '@'..='~'.
        Some('[') => {
            let mut body = String::new();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    if c != 'm' {
                        return None;
                    }
                    let params = body
                        .split([';', ':'])
                        .map(|p| p.parse().unwrap_or(0))
                        .collect();
                    return Some(params);
                }
                body.push(c);
            }
            None
        }
        // OSC (e.g. hyperlinks, window titles): ends with BEL or ESC '\'.
        Some(']') => {
            while let Some(c) = chars.next() {
                if c == '\x07' {
                    break;
                }
                if c == '\x1b' {
                    if chars.peek() == Some(&'\\') {
                        chars.next();
                    }
                    break;
                }
            }
            None
        }
        // Any other escape is ESC plus one character.
        _ => None,
    }
}

fn apply_sgr(mut style: Style, params: &[u16]) -> Style {
    let mut i = 0;
    while i < params.len() {
        match params[i] {
            0 => style = Style::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            n @ 30..=37 => style.fg = Some(Color::Ansi((n - 30) as u8)),
            n @ 90..=97 => style.fg = Some(Color::Ansi((n - 90 + 8) as u8)),
            39 => style.fg = None,
            38 => match params.get(i + 1) {
                Some(5) => {
                    if let Some(&n) = params.get(i + 2) {
                        style.fg = Some(Color::Indexed(n as u8));
                    }
                    i += 2;
                }
                Some(2) => {
                    if let (Some(&r), Some(&g), Some(&b)) =
                        (params.get(i + 2), params.get(i + 3), params.get(i + 4))
                    {
                        style.fg = Some(Color::Rgb(r as u8, g as u8, b as u8));
                    }
                    i += 4;
                }
                _ => {}
            },
            // Backgrounds carry their own color arguments; skip them.
            48 => match params.get(i + 1) {
                Some(5) => i += 2,
                Some(2) => i += 4,
                _ => {}
            },
            _ => {}
        }
        i += 1;
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(fg: Option<Color>, bold: bool) -> Style {
        Style {
            fg,
            bold,
            ..Default::default()
        }
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(parse_line(b"hello world"), ("hello world".into(), vec![]));
        assert_eq!(clean_line("caf\u{e9}".as_bytes()), "café");
    }

    #[test]
    fn colors_become_spans() {
        let (text, spans) = parse_line(b"\x1b[1;31merror\x1b[0m: bad");
        assert_eq!(text, "error: bad");
        assert_eq!(
            spans,
            [Span {
                start: 0,
                end: 5,
                style: style(Some(Color::Ansi(1)), true)
            }]
        );

        let (text, spans) =
            parse_line(b"a\x1b[32mgreen\x1b[38;5;208morange\x1b[38;2;1;2;3mrgb\x1b[39m.");
        assert_eq!(text, "agreenorangergb.");
        let colors: Vec<(usize, usize, Option<Color>)> =
            spans.iter().map(|s| (s.start, s.end, s.style.fg)).collect();
        assert_eq!(
            colors,
            [
                (1, 6, Some(Color::Ansi(2))),
                (6, 12, Some(Color::Indexed(208))),
                (12, 15, Some(Color::Rgb(1, 2, 3))),
            ]
        );

        let (_, spans) = parse_line(b"\x1b[94mbright\x1b[m \x1b[48;5;1mbg only");
        assert_eq!(spans.len(), 1, "background colors aren't kept");
        assert_eq!(spans[0].style.fg, Some(Color::Ansi(12)));
    }

    #[test]
    fn other_escapes_are_removed() {
        assert_eq!(clean_line(b"\x1b[2K\x1b[1Gdone"), "done");
        assert_eq!(
            clean_line(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
        assert_eq!(clean_line(b"\x1b]0;title\x07text"), "text");
    }

    #[test]
    fn carriage_returns_keep_the_last_segment() {
        assert_eq!(clean_line(b"windows line\r"), "windows line");
        assert_eq!(clean_line(b" 10%\r 50%\r100% done"), "100% done");
    }

    #[test]
    fn tabs_and_control_characters() {
        assert_eq!(clean_line(b"a\tb"), "a       b");
        assert_eq!(clean_line(b"abcdefgh\tx"), "abcdefgh        x");
        assert_eq!(clean_line(b"bell\x07 and\x00 null"), "bell and null");
    }

    #[test]
    fn invalid_utf8_is_replaced() {
        assert_eq!(clean_line(b"ok \xff\xfe end"), "ok \u{fffd}\u{fffd} end");
    }

    #[test]
    fn buffer_keeps_the_newest_lines() {
        let mut buffer = OutputBuffer::new(3);
        for i in 1..=5 {
            buffer.push(Stream::Stdout, plain(i.to_string()));
        }
        let kept: Vec<&str> = buffer.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(kept, ["3", "4", "5"]);
        assert_eq!(buffer.dropped(), 2);
        buffer.clear();
        assert!(buffer.is_empty());
        assert_eq!(buffer.dropped(), 0);
    }
}
