//! The palette: every color this CLI puts on a terminal, and when.
//!
//! A terminal's sixteen ANSI colors are whatever its theme says they are,
//! so none of them is safe on every background. Measured against ten common
//! themes, ANSI cyan fell to 2.1:1 on iTerm2's light background and dimmed
//! text to 1.9:1 on Solarized Light — below the 3:1 bold text needs.
//!
//! So this palette is fixed RGB, the approach Cloudflare's `cf` CLI takes,
//! at mid luminance — the band where a color reads on black and on white
//! alike. Text colors sit at about 0.21: 5.2:1 on black, 4.1:1 on white,
//! and no worse than 3.5:1 on any theme measured. The accent, only ever
//! bold, sits a little higher (see [`ACCENT`]). The colors differ in hue
//! rather than lightness. `every_color_reads_on_black_and_on_white` holds
//! each to its floor, so a new color has to meet one too.
//!
//! RGB needs a terminal that renders it. Where one is not known to, the
//! palette falls back to what any terminal shows the same way: the accent
//! and the error color become bold, and muted text becomes plain.
//!
//! Whether to color at all is [`super::style::enabled`]'s answer, and this
//! module only says what with.

use clap::builder::styling::{RgbColor, Style, Styles};

/// Mapbox blue (`#4264FB`), lightened to 0.26 luminance. Only ever bold, so
/// it needs 3:1 rather than 4:1, and spends the difference on dark themes:
/// it is the brightest blue of that hue still at 3:1 on every theme
/// measured (Solarized Light, 3.1:1), and 6.2:1 on black. At the 0.21 the
/// rest of the palette sits at, headings looked darker than the reader's
/// own text on a dark theme.
const ACCENT: RgbColor = RgbColor(0x67, 0x83, 0xfc);
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

/// The accent: headings, the banner's name, and the names in a tip a reader
/// copies. Always bold.
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
/// Headings in the accent; names to type, values to fill in and clap's
/// suggestions bold in the terminal's own foreground — the color every
/// theme makes most readable, for the part a reader has to get exactly
/// right; notes muted. Values are not underlined: clap gives the space
/// before a value the value's style, which bold hides and an underline
/// shows as a stray rule.
pub fn styles() -> Styles {
    styles_for(truecolor())
}

pub fn styles_for(truecolor: bool) -> Styles {
    let bold = Style::new().bold();
    Styles::plain()
        .header(accent_for(truecolor))
        .usage(accent_for(truecolor))
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

/// Every color, with the contrast it must clear on both black and white:
/// 3:1 for one only ever bold, 4:1 for one used on plain text.
#[cfg(test)]
pub(crate) const PALETTE: &[(RgbColor, f64)] = &[(ACCENT, 3.0), (MUTED, 4.0), (ERROR, 4.0)];

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

    /// The palette's one rule. 3:1 is what bold text needs, and the accent
    /// is only ever bold; the colors used on plain text clear 4:1, which
    /// leaves room for the themes whose backgrounds are neither black nor
    /// white.
    #[test]
    fn every_color_reads_on_black_and_on_white() {
        for &(color, floor) in PALETTE {
            let l = luminance(color);
            let on_black = contrast(l, 0.0);
            let on_white = contrast(l, 1.0);
            assert!(
                on_black >= floor && on_white >= floor,
                "{color:?} is {on_black:.1}:1 on black and {on_white:.1}:1 on white, under {floor}:1"
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
