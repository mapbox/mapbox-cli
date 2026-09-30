//! Terminal color, for the parts of the output a person reads.
//!
//! Raw ANSI escapes rather than a crate, following the usual conventions by
//! hand: color only on a stream that is a terminal, and never under
//! `NO_COLOR` or `TERM=dumb`. The palette is the terminal's own (bold, dim,
//! bright blue) rather than fixed RGB values, so it follows the user's theme
//! and reads on a light background as well as a dark one.
//!
//! Windows is the one place that needs care: a legacy console prints the
//! escapes literally unless virtual-terminal mode is switched on, which takes
//! an API call this crate has no `unsafe` for. There, color is kept to
//! terminals known to have that mode on already.
//!
//! Callers pass in whether the stream is a terminal rather than this module
//! probing it, so that stdout is only ever touched by the modules
//! `tests/source_guards.rs` allows to.

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const ACCENT: &str = "\x1b[94m";
pub const GREEN: &str = "\x1b[32m";
pub const RED: &str = "\x1b[31m";
pub const YELLOW: &str = "\x1b[33m";

/// Whether a stream should be written in color.
pub fn enabled(stream_is_terminal: bool) -> bool {
    stream_is_terminal
        && allowed(
            env_value("NO_COLOR").is_some(),
            env_value("TERM").as_deref(),
            !cfg!(windows)
                || env_value("WT_SESSION").is_some()
                || env_value("TERM_PROGRAM").is_some(),
        )
}

/// Whether a line on a stream can be redrawn in place, for a spinner.
///
/// Not a color question, so `NO_COLOR` does not turn it off; a terminal that
/// would print the escapes literally does.
pub fn redraws(stream_is_terminal: bool) -> bool {
    stream_is_terminal
        && allowed(
            false,
            env_value("TERM").as_deref(),
            !cfg!(windows)
                || env_value("WT_SESSION").is_some()
                || env_value("TERM_PROGRAM").is_some(),
        )
}

fn allowed(no_color: bool, term: Option<&str>, console_renders_ansi: bool) -> bool {
    !no_color && term != Some("dumb") && console_renders_ansi
}

/// A variable's value, treating an empty one as unset — `NO_COLOR=` is how a
/// shell clears it, not a request for no color.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// `text` in `style`, or unchanged when color is off.
pub fn paint(text: &str, style: &str, on: bool) -> String {
    if on {
        format!("{style}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Dims prose while keeping its `code spans` in the accent color, so the part
/// a reader copies stands out from the advice around it. The backticks stay:
/// the text must mean the same thing with the color stripped.
pub fn dim_prose(text: &str, on: bool) -> String {
    if !on {
        return text.to_string();
    }
    let parts: Vec<&str> = text.split('`').collect();
    let mut out = String::new();
    for (index, part) in parts.iter().enumerate() {
        // Odd parts sit between a pair of backticks, except a last one after
        // an unpaired tick — a value from a response can carry one. That tick
        // is written back as prose rather than closed, which would change
        // the text.
        let unpaired = index == parts.len() - 1 && index % 2 == 1;
        if unpaired {
            out.push_str(&format!("{DIM}`{part}{RESET}"));
        } else if index % 2 == 1 {
            out.push_str(&format!("{ACCENT}`{part}`{RESET}"));
        } else if !part.is_empty() {
            out.push_str(&format!("{DIM}{part}{RESET}"));
        }
    }
    out
}

/// `text` with every escape this module writes removed.
#[cfg(test)]
pub fn strip(text: &str) -> String {
    let mut plain = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            chars.by_ref().find(|&c| c == 'm');
        } else {
            plain.push(c);
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_a_dumb_terminal_or_a_legacy_console_get_plain_text() {
        assert!(allowed(false, Some("xterm-256color"), true));
        assert!(!allowed(true, Some("xterm-256color"), true));
        assert!(!allowed(false, Some("dumb"), true));
        assert!(!allowed(false, None, false));
    }

    #[test]
    fn a_stream_that_is_not_a_terminal_is_never_colored() {
        assert!(!enabled(false));
    }

    #[test]
    fn paint_off_is_the_text_itself() {
        assert_eq!(paint("ID", BOLD, false), "ID");
        assert_eq!(paint("ID", BOLD, true), "\x1b[1mID\x1b[0m");
    }

    #[test]
    fn code_spans_stand_out_and_keep_their_backticks() {
        let tip = "Values are shortened; `-o json` prints each row whole.";
        let colored = dim_prose(tip, true);
        assert!(
            colored.contains(&format!("{ACCENT}`-o json`{RESET}")),
            "{colored:?}"
        );
        assert_eq!(strip(&colored), tip);
        assert_eq!(dim_prose(tip, false), tip);
    }

    #[test]
    fn an_unpaired_backtick_is_kept_as_it_was() {
        for tip in ["`--product \"a`b\"` narrows.", "odd ` tick", "ends with `"] {
            assert_eq!(strip(&dim_prose(tip, true)), tip);
        }
    }

    #[test]
    fn a_tip_that_opens_with_a_code_span_loses_nothing() {
        let tip = "`--daily` for the day-by-day numbers.";
        assert_eq!(strip(&dim_prose(tip, true)), tip);
    }
}
