//! The palette: every color this CLI puts on a terminal, and when.
//!
//! A terminal's sixteen ANSI colors are whatever its theme says they are,
//! so none of them is safe on every background. Measured against ten common
//! themes, ANSI cyan fell to 2.1:1 on iTerm2's light background and dimmed
//! text to 1.9:1 on Solarized Light — below the 3:1 bold text needs.
//!
//! So this palette is fixed RGB, the approach Cloudflare's `cf` CLI takes,
//! and every color in it has the same relative luminance, about 0.21. That
//! is the band where a color reads on black and on white alike: about 5.2:1
//! on one and 4.1:1 on the other, and no worse than 3.5:1 on any of the
//! themes measured (Dracula's background is the darkest that is not black).
//! The colors differ in hue only. `every_color_reads_on_black_and_on_white`
//! holds the rule, so a new color has to meet it too.
//!
//! RGB needs a terminal that renders it. Where one is not known to, the
//! palette falls back to what any terminal shows the same way: the accent
//! and the error color become bold, and muted text becomes plain.
//!
//! Whether to color at all is [`super::style::enabled`]'s answer, and this
//! module only says what with.

use clap::builder::styling::{RgbColor, Style, Styles};

/// Mapbox blue, lightened to the palette's luminance: the brand color is
/// 0.17, a little dark for a dark background.
const ACCENT: RgbColor = RgbColor(0x52, 0x72, 0xfb);
/// Secondary text — notes, hints, the banner's version — set back from the
/// result without the faintness `dim` has on some themes.
const MUTED: RgbColor = RgbColor(0x7f, 0x7f, 0x7f);
const ERROR: RgbColor = RgbColor(0xe1, 0x46, 0x46);

/// Whether the terminal is known to render 24-bit color.
///
/// The same signals `cf` reads. Apple's Terminal is left out on purpose: it
/// renders 256 colors, and maps 24-bit ones to the nearest of those.
pub fn truecolor() -> bool {
    static TRUECOLOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRUECOLOR.get_or_init(|| {
        let var = |name| std::env::var(name).unwrap_or_default();
        supports_truecolor(
            &var("COLORTERM"),
            &var("TERM"),
            &var("TERM_PROGRAM"),
            std::env::var_os("WT_SESSION").is_some(),
        )
    })
}

fn supports_truecolor(
    colorterm: &str,
    term: &str,
    term_program: &str,
    windows_terminal: bool,
) -> bool {
    matches!(colorterm, "truecolor" | "24bit")
        || matches!(term, "xterm-kitty" | "xterm-ghostty" | "wezterm")
        || matches!(term_program, "iTerm.app" | "vscode" | "WezTerm" | "ghostty")
        || windows_terminal
}

/// The accent: the banner's name, and the names in a tip a reader copies —
/// small marks, not structure (see [`styles_for`]).
pub fn accent() -> Style {
    accent_for(truecolor())
}

/// Secondary text.
pub fn muted() -> Style {
    muted_for(truecolor())
}

fn accent_for(truecolor: bool) -> Style {
    colored(ACCENT, truecolor).bold()
}

fn muted_for(truecolor: bool) -> Style {
    if truecolor {
        Style::new().fg_color(Some(MUTED.into()))
    } else {
        Style::new()
    }
}

fn colored(color: RgbColor, truecolor: bool) -> Style {
    if truecolor {
        Style::new().fg_color(Some(color.into()))
    } else {
        Style::new()
    }
}

/// clap's styles for help and usage errors.
///
/// Headings, names to type, values to fill in and clap's suggestions are
/// bold in the terminal's own foreground; notes are muted. No accent: a
/// fixed color has to sit at mid luminance to read on light themes, which
/// on a dark one is darker than the reader's own text, so headings in it
/// receded where they should lead — and clashed with whatever hue the
/// reader's theme uses. Help takes its color from the theme; layout and
/// bold give it structure. Values are not underlined: clap gives the space
/// before a value the value's style, which bold hides and an underline
/// shows as a stray rule.
pub fn styles() -> Styles {
    styles_for(truecolor())
}

pub fn styles_for(truecolor: bool) -> Styles {
    let bold = Style::new().bold();
    Styles::plain()
        .header(bold)
        .usage(bold)
        .literal(bold)
        .placeholder(bold)
        .valid(bold)
        .invalid(bold)
        .error(colored(ERROR, truecolor).bold())
        .context(muted_for(truecolor))
        // `[possible values: on, off]`: the note muted, the values in it bold
        // like every other thing to type.
        .context_value(bold)
}

/// The escape that opens `style`, for the code that writes raw ANSI.
pub fn open(style: Style) -> String {
    style.render().to_string()
}

#[cfg(test)]
pub(crate) const PALETTE: &[RgbColor] = &[ACCENT, MUTED, ERROR];

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(RgbColor(r, g, b): RgbColor) -> f64 {
        let channel = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    fn contrast(a: f64, b: f64) -> f64 {
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// The palette's one rule. 3:1 is what bold text needs; on pure black and
    /// pure white the palette clears 4:1, which leaves room for the themes
    /// whose backgrounds are neither.
    #[test]
    fn every_color_reads_on_black_and_on_white() {
        for &color in PALETTE {
            let l = luminance(color);
            let on_black = contrast(l, 0.0);
            let on_white = contrast(l, 1.0);
            assert!(
                on_black >= 4.0 && on_white >= 4.0,
                "{color:?} is {on_black:.1}:1 on black and {on_white:.1}:1 on white"
            );
        }
    }

    #[test]
    fn the_terminals_cf_trusts_with_24_bit_color() {
        assert!(supports_truecolor("truecolor", "", "", false));
        assert!(supports_truecolor("24bit", "", "", false));
        assert!(supports_truecolor("", "xterm-ghostty", "", false));
        assert!(supports_truecolor("", "xterm-256color", "iTerm.app", false));
        assert!(supports_truecolor("", "", "", true));
        assert!(!supports_truecolor(
            "",
            "xterm-256color",
            "Apple_Terminal",
            false
        ));
        assert!(!supports_truecolor("", "xterm-256color", "", false));
    }

    /// Without 24-bit color the accent is bold and muted text plain, never a
    /// theme's ANSI color.
    #[test]
    fn the_fallback_is_bold_and_plain() {
        assert_eq!(accent_for(false), Style::new().bold());
        assert_eq!(muted_for(false), Style::new());
        assert_eq!(
            accent_for(true),
            Style::new().fg_color(Some(ACCENT.into())).bold()
        );
    }
}
