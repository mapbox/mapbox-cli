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
//! In color where [`crate::style`] allows it.

use std::io::IsTerminal;

use clap::ArgMatches;

use crate::style::{self, DIM, RESET};
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
    if enabled(
        std::io::stderr().is_terminal(),
        matches.get_flag(ARG),
        matches.subcommand_name(),
    ) {
        let color = style::enabled(true);
        output::progress(&text(env!("CARGO_PKG_VERSION"), color));
    }
}

/// Bold, in the accent color.
const NAME: &str = "\x1b[1;94m";

/// `completion` is excluded because its usual caller is a shell startup
/// file, where a banner on every new shell is noise.
fn enabled(stderr_is_terminal: bool, quiet: bool, command: Option<&str>) -> bool {
    stderr_is_terminal && !quiet && command != Some(completion::COMMAND)
}

fn text(version: &str, color: bool) -> String {
    let rule = "─".repeat(RULE_WIDTH);
    if color {
        format!("🗺️  {NAME}mapbox{RESET} {DIM}· v{version}{RESET}\n{DIM}{rule}{RESET}")
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
    fn color_changes_the_escapes_and_nothing_else() {
        let colored = text("1.2.3", true);
        assert!(colored.contains(NAME), "{colored:?}");
        assert!(colored.ends_with(RESET), "{colored:?}");
        assert_eq!(style::strip(&colored), text("1.2.3", false));
    }
}
