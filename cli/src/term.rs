//! Terminal facts and styling: TTY detection, width, ANSI color that honors
//! `NO_COLOR`, and display-width-aware truncation.

use std::io::IsTerminal;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal()
}

/// Columns available on stdout: `$COLUMNS`, then the terminal, then 100.
pub fn width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|columns| *columns > 0)
        .or_else(terminal_columns)
        .unwrap_or(100)
}

#[cfg(unix)]
fn terminal_columns() -> Option<usize> {
    // SAFETY: TIOCGWINSZ only fills the zeroed `winsize` we pass in.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    (result == 0 && size.ws_col > 0).then_some(size.ws_col as usize)
}

#[cfg(not(unix))]
fn terminal_columns() -> Option<usize> {
    None
}

/// Color only for a real terminal, never with `--no-color`, `NO_COLOR`, or `TERM=dumb`.
pub fn color_enabled(no_color_flag: bool) -> bool {
    !no_color_flag
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
        && std::env::var("TERM").map_or(true, |term| term != "dumb")
        && stdout_is_terminal()
}

/// Semantic tones; the palette maps them to the terminal's own ANSI colors so
/// light and dark themes both stay legible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Danger,
    Warning,
    Success,
    Accent,
    Muted,
}

#[derive(Debug, Clone, Copy)]
pub struct Paint {
    enabled: bool,
}

impl Paint {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    pub fn tone(&self, tone: Tone, text: &str) -> String {
        let code = match tone {
            Tone::Danger => "31",
            Tone::Warning => "33",
            Tone::Success => "32",
            Tone::Accent => "34",
            Tone::Muted => "2",
        };
        self.wrap(code, text)
    }

    pub fn bold(&self, text: &str) -> String {
        self.wrap("1", text)
    }

    fn wrap(&self, code: &str, text: &str) -> String {
        if self.enabled && !text.is_empty() {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Cut `text` to at most `max` display columns, ending in "…" when shortened.
pub fn truncate(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > max - 1 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Truncate then right-pad with spaces to exactly `width` columns.
pub fn fit(text: &str, width: usize) -> String {
    let cut = truncate(text, width);
    let pad = width.saturating_sub(display_width(&cut));
    format!("{cut}{}", " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_respects_display_width() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("日本語のタイトル", 7), "日本語…");
        assert_eq!(display_width(&truncate("日本語のタイトル", 7)), 7);
        assert_eq!(fit("ab", 4), "ab  ");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(truncate("x", 0), "");
    }

    #[test]
    fn disabled_paint_is_plain_text() {
        let paint = Paint::new(false);
        assert_eq!(paint.tone(Tone::Danger, "fail"), "fail");
        assert_eq!(paint.bold("Needs action"), "Needs action");
        assert_eq!(
            Paint::new(true).tone(Tone::Success, "pass"),
            "\x1b[32mpass\x1b[0m"
        );
    }
}
