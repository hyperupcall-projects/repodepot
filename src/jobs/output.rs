//! Captured output: turning raw bytes into displayable lines, and keeping a bounded number of them.

use std::collections::VecDeque;

/// Longest line kept, in bytes. Longer runs without a newline are split.
pub const MAX_LINE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub stream: Stream,
    pub text: String,
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

    pub fn push(&mut self, stream: Stream, text: String) {
        if self.lines.len() == self.limit {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(Line { stream, text });
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

    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter()
    }

    /// How many older lines were thrown away to stay under the limit.
    pub fn dropped(&self) -> usize {
        self.dropped
    }
}

/// Makes one line of raw output displayable:
/// - invalid UTF-8 is replaced
/// - after a carriage return only the last segment is kept, like a terminal shows
/// - ANSI escape sequences (colors, cursor moves) are removed
/// - tabs become spaces (tab stops every 8 columns); other control characters are dropped
pub fn clean_line(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let text = text.strip_suffix('\r').unwrap_or(&text);
    let text = match text.rfind('\r') {
        Some(i) => &text[i + 1..],
        None => text,
    };

    let mut out = String::with_capacity(text.len());
    let mut column = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => skip_escape(&mut chars),
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
    out
}

/// Skips the rest of an escape sequence whose ESC was just read.
fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        // CSI: parameters and intermediates, then one final byte in '@'..='~'.
        Some('[') => {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
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
        }
        // Any other escape is ESC plus one character.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(clean_line(b"hello world"), "hello world");
        assert_eq!(clean_line("caf\u{e9}".as_bytes()), "café");
    }

    #[test]
    fn colors_and_escapes_are_removed() {
        assert_eq!(clean_line(b"\x1b[1;31merror\x1b[0m: bad"), "error: bad");
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
            buffer.push(Stream::Stdout, i.to_string());
        }
        let kept: Vec<&str> = buffer.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(kept, ["3", "4", "5"]);
        assert_eq!(buffer.dropped(), 2);
        assert_eq!(buffer.len(), 3);
    }
}
