//! Terminal color, for the parts of the output a person reads: whether to
//! color, and raw ANSI helpers for the code that writes its own escapes.
//! What the colors are is [`super::theme`]'s business.
//!
//! Whether follows the order `cf` uses: `NO_COLOR` turns color off whatever
//! else is set, `FORCE_COLOR` (anything but `0`) turns it on even into a
//! pipe, and otherwise a stream is colored when it is a terminal that is not
//! `TERM=dumb`. `color_choice` hands clap the same answer for help and usage
//! errors.
//!
//! Windows is the one place that needs care: a legacy console prints the
//! escapes literally unless virtual-terminal mode is switched on, which takes
//! an API call this crate has no `unsafe` for. There, color is kept to
//! terminals known to have that mode on already.
//!
//! Callers pass in whether the stream is a terminal rather than this module
//! probing it, so that stdout is only ever touched by the modules
//! `tests/source_guards.rs` allows to.

use clap::ColorChoice;

use super::theme;

pub const RESET: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";

/// The palette's accent, as an escape. Bold where 24-bit color is not known
/// to render.
pub fn accent() -> String {
    theme::open(theme::accent())
}

/// The palette's muted color, as an escape. Empty where 24-bit color is not
/// known to render: plain text, not a theme's `dim`.
pub fn muted() -> String {
    theme::open(theme::muted())
}

/// Whether a stream should be written in color.
pub fn enabled(stream_is_terminal: bool) -> bool {
    decide(
        env_value("NO_COLOR").is_some(),
        env_value("FORCE_COLOR").as_deref(),
    )
    .unwrap_or_else(|| {
        stream_is_terminal && env_value("TERM").as_deref() != Some("dumb") && console_renders_ansi()
    })
}

/// The same decision for clap, which checks for a terminal itself.
pub fn color_choice() -> ColorChoice {
    match decide(
        env_value("NO_COLOR").is_some(),
        env_value("FORCE_COLOR").as_deref(),
    ) {
        Some(true) => ColorChoice::Always,
        Some(false) => ColorChoice::Never,
        None if env_value("TERM").as_deref() == Some("dumb") => ColorChoice::Never,
        None => ColorChoice::Auto,
    }
}

/// What the environment settles before the stream is looked at, if anything.
fn decide(no_color: bool, force_color: Option<&str>) -> Option<bool> {
    if no_color {
        Some(false)
    } else {
        force_color.map(|value| value != "0")
    }
}

/// A legacy Windows console prints escapes literally unless
/// virtual-terminal mode is on, and switching it on takes an API call this
/// crate has no `unsafe` for — so color there is kept to terminals known to
/// have it on already.
fn console_renders_ansi() -> bool {
    !cfg!(windows) || env_value("WT_SESSION").is_some() || env_value("TERM_PROGRAM").is_some()
}

/// A variable's value, treating an empty one as unset — `NO_COLOR=` is how a
/// shell clears it, not a request for no color.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// `text` in `style`, or unchanged when color is off or `style` is empty.
pub fn paint(text: &str, style: &str, on: bool) -> String {
    if on && !style.is_empty() {
        format!("{style}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Mutes prose while keeping its `code spans` in the accent, so the part a
/// reader copies stands out from the advice around it. The backticks stay:
/// the text must mean the same thing with the color stripped.
pub fn dim_prose(text: &str, on: bool) -> String {
    if !on {
        return text.to_string();
    }
    let (accent, muted) = (accent(), muted());
    let parts: Vec<&str> = text.split('`').collect();
    let mut out = String::new();
    for (index, part) in parts.iter().enumerate() {
        // Odd parts sit between a pair of backticks, except a last one after
        // an unpaired tick — a value from a response can carry one. That tick
        // is written back as prose rather than closed, which would change
        // the text.
        let unpaired = index == parts.len() - 1 && index % 2 == 1;
        if unpaired {
            out.push_str(&paint(&format!("`{part}"), &muted, true));
        } else if index % 2 == 1 {
            out.push_str(&paint(&format!("`{part}`"), &accent, true));
        } else if !part.is_empty() {
            out.push_str(&paint(part, &muted, true));
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

    /// `NO_COLOR` wins over everything; `FORCE_COLOR` decides next, with
    /// `0` meaning off; with neither, the stream decides.
    #[test]
    fn no_color_then_force_color_then_the_stream() {
        assert_eq!(decide(true, Some("1")), Some(false));
        assert_eq!(decide(false, Some("1")), Some(true));
        assert_eq!(decide(false, Some("3")), Some(true));
        assert_eq!(decide(false, Some("0")), Some(false));
        assert_eq!(decide(false, None), None);
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
            colored.contains(&format!("{}`-o json`{RESET}", accent())),
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
