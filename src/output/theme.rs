//! The palette: every color this CLI puts on a terminal, and when.
//!
//! A terminal's sixteen ANSI colors are whatever its theme says they are,
//! so none of them is safe on every background. Measured against ten common
//! themes, ANSI cyan fell to 2.1:1 on iTerm2's light background and dimmed
//! text to 1.9:1 on Solarized Light — below the 3:1 bold text needs.
//!
//! So the palette is fixed RGB, the approach Cloudflare's `cf` CLI takes —
//! three of them, for the background the colors land on. Where the terminal
//! answers an OSC 11 query for its background color, colors are chosen for
//! a dark or a light one and clear 4.5:1 on the common themes of that kind.
//! Where it does not (tmux, a slow link, a terminal without the query), the
//! fallback sits at mid luminance, the band where a color reads on black
//! and on white alike: about 5.2:1 and 4.1:1 for text, no worse than 3.5:1
//! on any theme measured. In every palette the accent is only ever bold, so
//! it needs 3:1 rather than the 4:1 text does, and spends the difference on
//! being bright. `every_color_clears_its_floor` holds each color to that.
//!
//! RGB needs a terminal that renders it. Where one is not known to, there is
//! no palette at all, only what any terminal shows the same way: the accent
//! and the error color become bold, and muted text becomes plain.
//!
//! Whether to color at all is [`super::style::enabled`]'s answer, and this
//! module only says what with.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use clap::builder::styling::{RgbColor, Style, Styles};

/// What the terminal draws on, as far as it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Background {
    Dark,
    Light,
    Unknown,
}

/// How color is rendered: the palette for a background, or bold alone where
/// 24-bit color is not known to render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Rgb(Background),
    Bold,
}

/// The three palettes share one accent hue, a teal (`#60B8C5` on a dark
/// background), at the lightness each background needs.
struct Palette {
    /// Headings, the banner's name, the names in a tip. Only ever bold.
    accent: RgbColor,
    /// Notes, hints, the banner's version — set back from the result
    /// without the faintness `dim` has on some themes.
    muted: RgbColor,
    error: RgbColor,
}

/// On a dark background: the accent at full strength, 6.2:1 or better on
/// the dark themes measured, and text colors at 4.75:1 or better.
const DARK: Palette = Palette {
    accent: RgbColor(0x60, 0xb8, 0xc5),
    muted: RgbColor(0x95, 0x95, 0x95),
    error: RgbColor(0xe8, 0x71, 0x71),
};

/// On a light background: the accent's hue darkened to 0.14 luminance,
/// 5.1:1 or better on the light themes measured, and text colors at 4.8:1
/// or better.
const LIGHT: Palette = Palette {
    accent: RgbColor(0x2d, 0x72, 0x7c),
    muted: RgbColor(0x6d, 0x6d, 0x6d),
    error: RgbColor(0xbe, 0x1f, 0x1f),
};

/// Background unknown: mid luminance. The accent's hue at 0.30, the
/// brightest still at 3:1 on white — 6.9:1 on black, and 2.8:1 on
/// Solarized Light, the one theme measured it falls short on, chosen over a
/// dimmer accent everywhere else. Text colors sit at 0.21.
const EITHER: Palette = Palette {
    accent: RgbColor(0x40, 0xa1, 0xb0),
    muted: RgbColor(0x7f, 0x7f, 0x7f),
    error: RgbColor(0xe1, 0x46, 0x46),
};

fn palette(background: Background) -> &'static Palette {
    match background {
        Background::Dark => &DARK,
        Background::Light => &LIGHT,
        Background::Unknown => &EITHER,
    }
}

/// How long to wait for the terminal to report its background. A terminal
/// without the query answers the one sent after it straight away, so this
/// only bounds a slow link; running out costs the fallback palette, not the
/// command.
const QUERY_TIMEOUT: Duration = Duration::from_millis(100);

/// The look for this run, settled once.
pub fn current() -> Look {
    static LOOK: OnceLock<Look> = OnceLock::new();
    *LOOK.get_or_init(|| {
        if truecolor() {
            Look::Rgb(background())
        } else {
            Look::Bold
        }
    })
}

static QUERY_ALLOWED: AtomicBool = AtomicBool::new(false);

/// Lets this run ask the terminal for its background, given whether there
/// is a terminal to ask — `output::on_a_terminal`, since this module does
/// not touch the streams itself. Only for help: see [`background`].
pub fn allow_background_query(on_a_terminal: bool) {
    QUERY_ALLOWED.store(on_a_terminal, Ordering::Relaxed);
}

/// Asks the terminal for its background color.
///
/// Only for a run that shows help and then exits. The answer arrives on the
/// terminal's input, and whatever else is waiting there is read with it: a
/// command that goes on to ask "Delete? [y/N]" lost the answer typed ahead
/// to the query — `non_interactive`'s prompt test hung on exactly that. Help
/// reads nothing after, so there is nothing to lose; every other run uses
/// the `Unknown` palette.
///
/// Also only when there is a terminal to ask and color will be shown on it,
/// so a pipe, `NO_COLOR` or `--no-color` never sends the query.
fn background() -> Background {
    if !QUERY_ALLOWED.load(Ordering::Relaxed) || !super::style::enabled(true) {
        return Background::Unknown;
    }
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = QUERY_TIMEOUT;
    // The background alone, not `theme_mode`, which asks for the foreground
    // too: the timeout is per read, and on a terminal that never answers two
    // queries waited half a second.
    match terminal_colorsaurus::background_color(options) {
        Ok(color) if color.perceived_lightness() < 0.5 => Background::Dark,
        Ok(_) => Background::Light,
        Err(_) => Background::Unknown,
    }
}

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
    accent_in(current())
}

/// Secondary text.
pub fn muted() -> Style {
    muted_in(current())
}

fn accent_in(look: Look) -> Style {
    colored(look, |p| p.accent).bold()
}

/// Bold like every other color here: a colored run of thin text reads as
/// fainter than the same color bold, and the palette is set to be read.
fn muted_in(look: Look) -> Style {
    match look {
        Look::Rgb(_) => colored(look, |p| p.muted).bold(),
        Look::Bold => Style::new(),
    }
}

fn colored(look: Look, pick: fn(&Palette) -> RgbColor) -> Style {
    match look {
        Look::Rgb(background) => Style::new().fg_color(Some(pick(palette(background)).into())),
        Look::Bold => Style::new(),
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
    styles_in(current())
}

pub fn styles_in(look: Look) -> Styles {
    let bold = Style::new().bold();
    Styles::plain()
        .header(accent_in(look))
        .usage(accent_in(look))
        .literal(bold)
        .placeholder(bold)
        .valid(bold)
        .invalid(bold)
        .error(colored(look, |p| p.error).bold())
        .context(muted_in(look))
        // `[possible values: on, off]`: the note muted, the values in it bold
        // like every other thing to type.
        .context_value(bold)
}

/// The escape that opens `style`, for the code that writes raw ANSI.
pub fn open(style: Style) -> String {
    style.render().to_string()
}

/// Every look there is, for tests that render each.
#[cfg(test)]
pub(crate) const LOOKS: &[Look] = &[
    Look::Rgb(Background::Dark),
    Look::Rgb(Background::Light),
    Look::Rgb(Background::Unknown),
    Look::Bold,
];

/// Every color in every palette.
#[cfg(test)]
pub(crate) fn colors() -> Vec<RgbColor> {
    [&DARK, &LIGHT, &EITHER]
        .iter()
        .flat_map(|p| [p.accent, p.muted, p.error])
        .collect()
}

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

    fn hex(code: u32) -> f64 {
        luminance(RgbColor((code >> 16) as u8, (code >> 8) as u8, code as u8))
    }

    /// The palette's one rule: each color clears its floor on every
    /// background it may land on — 3:1 for the accent, which is only ever
    /// bold, and 4.5:1 (4:1 for the fallback, which has to serve both kinds)
    /// for the colors used on plain text. The backgrounds are the common
    /// themes measured: plain black and white, VS Code, Windows Terminal,
    /// Solarized and Dracula.
    #[test]
    fn every_color_clears_its_floor() {
        let dark = [0x000000, 0x1c1c1c, 0x1e1e1e, 0x0c0c0c, 0x002b36, 0x282a36].map(hex);
        let light = [0xffffff, 0xfdf6e3, 0xf6f8fa].map(hex);
        let either: Vec<f64> = [0.0, 1.0].to_vec();
        for (name, palette, backgrounds, text_floor) in [
            ("dark", &DARK, dark.to_vec(), 4.5),
            ("light", &LIGHT, light.to_vec(), 4.5),
            ("fallback", &EITHER, either, 4.0),
        ] {
            for (role, color, floor) in [
                ("accent", palette.accent, 3.0),
                ("muted", palette.muted, text_floor),
                ("error", palette.error, text_floor),
            ] {
                for &background in &backgrounds {
                    let ratio = contrast(luminance(color), background);
                    assert!(
                        ratio >= floor,
                        "{name} {role} {color:?} is {ratio:.2}:1 on a background of \
                         luminance {background:.3}, under {floor}:1"
                    );
                }
            }
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
        assert_eq!(accent_in(Look::Bold), Style::new().bold());
        assert_eq!(muted_in(Look::Bold), Style::new());
        assert_eq!(
            accent_in(Look::Rgb(Background::Light)),
            Style::new().fg_color(Some(LIGHT.accent.into())).bold()
        );
    }
}
