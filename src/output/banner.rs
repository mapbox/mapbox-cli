//! The name-and-version line a run opens with.
//!
//! It tells a person at a terminal which `mapbox` answered, which is the
//! first thing worth knowing when more than one is installed. It is not the
//! result, so it goes to stderr, and it is shown only where somebody is
//! watching: a pipe, a CI log or an agent reading stderr gets nothing, the
//! same reasoning `update_check` uses for its notice.
//!
//! `--quiet` (`-q`) or `MAPBOX_QUIET` hides it.
//!
//! In color where [`super::style`] allows it.

use std::ffi::OsString;
use std::io::IsTerminal;

use clap::ArgMatches;

use super::style::{self, RESET};
use crate::{completion, output};

pub const ARG: &str = "quiet";
pub const SHORT: char = 'q';
pub const ENV: &str = "MAPBOX_QUIET";

const RULE_WIDTH: usize = 40;

/// Prints the banner if this run should show one.
///
/// Called only for a parsed command about to run: `--help`, `--version`,
/// usage errors and `--schema` never reach it.
pub fn show(matches: &ArgMatches) {
    let terminal = std::io::stderr().is_terminal();
    if enabled(terminal, matches.get_flag(ARG), matches.subcommand_name()) {
        output::progress(&text(env!("CARGO_PKG_VERSION"), style::enabled(terminal)));
    }
}

/// Prints the banner ahead of help — any help, top-level or a command's, as
/// `cf` does: help is the page read most, the version is often what the
/// reader came for, and a command's help without it read as an omission
/// next to the command's own run.
///
/// clap writes help during the parse, before there are matches for `show`
/// to read, so `-q`/`--quiet` and `MAPBOX_QUIET` are read here the way clap
/// would: the flag anywhere before `--`, the variable as clap's
/// `FalseyValueParser` reads it.
pub fn show_before_help(argv: &[OsString]) {
    let terminal = std::io::stderr().is_terminal();
    let quiet = quiet_in(argv, std::env::var(ENV).ok().as_deref());
    if enabled(terminal, quiet, None) {
        output::progress(&text(env!("CARGO_PKG_VERSION"), style::enabled(terminal)));
    }
}

fn quiet_in(argv: &[OsString], env: Option<&str>) -> bool {
    let (long, short) = (format!("--{ARG}"), format!("-{SHORT}"));
    let flag = argv
        .iter()
        .skip(1)
        .filter_map(|arg| arg.to_str())
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == long || arg == short);
    let variable = env.is_some_and(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "n" | "no" | "f" | "false" | "off"
        )
    });
    flag || variable
}

/// `completion` is excluded because its usual caller is a shell startup
/// file, where a banner on every new shell is noise.
fn enabled(stderr_is_terminal: bool, quiet: bool, command: Option<&str>) -> bool {
    stderr_is_terminal && !quiet && command != Some(completion::COMMAND)
}

fn text(version: &str, color: bool) -> String {
    let rule = "─".repeat(RULE_WIDTH);
    if color {
        let (name, muted) = (style::accent(), style::muted());
        format!("🗺️  {name}mapbox{RESET} {muted}· v{version}{RESET}\n{muted}{rule}{RESET}")
    } else {
        format!("🗺️  mapbox · v{version}\n{rule}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shown_only_at_a_terminal_and_when_not_asked_to_be_quiet() {
        assert!(enabled(true, false, Some("styles")));
        assert!(!enabled(false, false, Some("styles")));
        assert!(!enabled(true, true, Some("styles")));
    }

    #[test]
    fn not_shown_for_completion() {
        assert!(!enabled(true, false, Some(completion::COMMAND)));
    }

    #[test]
    fn names_the_version_above_a_rule() {
        let banner = text("1.2.3", false);
        let mut lines = banner.lines();
        assert_eq!(lines.next(), Some("🗺️  mapbox · v1.2.3"));
        assert_eq!(lines.next(), Some("─".repeat(RULE_WIDTH).as_str()));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn quiet_before_help_is_read_as_clap_would() {
        let argv = |args: &[&str]| -> Vec<OsString> {
            std::iter::once("mapbox")
                .chain(args.iter().copied())
                .map(OsString::from)
                .collect()
        };
        assert!(quiet_in(&argv(&["-q", "--help"]), None));
        assert!(quiet_in(&argv(&["--help", "--quiet"]), None));
        assert!(quiet_in(&argv(&["--help"]), Some("1")));
        assert!(!quiet_in(&argv(&["--help"]), Some("0")));
        assert!(!quiet_in(&argv(&["--help"]), Some("false")));
        assert!(!quiet_in(&argv(&["--help"]), Some("")));
        assert!(!quiet_in(&argv(&["--help"]), None));
    }

    #[test]
    fn color_changes_the_escapes_and_nothing_else() {
        let colored = text("1.2.3", true);
        assert!(
            colored.contains(&format!("{}mapbox", style::accent())),
            "{colored:?}"
        );
        assert!(colored.ends_with(RESET), "{colored:?}");
        assert_eq!(style::strip(&colored), text("1.2.3", false));
    }
}
