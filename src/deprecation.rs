//! What the CLI says when a command on its way out is run.
//!
//! Two different things retire a command, and a caller needs to know which
//! one happened: a spec can mark the *endpoint* deprecated — Mapbox saying
//! it will stop answering — or this CLI can retire a *command name* while
//! the endpoint stays exactly where it is. The first means find another
//! endpoint; the second means retype the command. A single warning that
//! blurred the two would send people looking for the wrong fix, so there are
//! two.
//!
//! Both land on stderr in every output mode, through
//! [`crate::output::progress`]: a deprecation is not the result, and a
//! caller collecting stdout must still get a clean document. `--schema` does
//! not come through here at all — it describes rather than runs, and carries
//! `deprecated` as a field instead.

use crate::output;

/// A command name this CLI is retiring on its own account.
///
/// Hand-kept because no spec knows about it: a rename, a merge, a command
/// that turned out to be a mistake. The spec-driven half needs no table —
/// see [`crate::spec::Operation`]'s `deprecated`.
pub struct Deprecation {
    /// The command as [`crate::typed_path`] reports it, minus `mapbox`:
    /// `styles get`, `styles draft get`, `auth whoami`, `tilesets-cli`.
    ///
    /// Two shapes are not keys here, and both look like they should be.
    /// **An alias cannot be named**: clap resolves one to its canonical name
    /// while parsing, so `auth status` never reaches us as itself — retiring
    /// an alias means making it a command of its own. **A bare service
    /// cannot be named** either: every service, and every intermediate
    /// command group, sets `subcommand_required(true)`, so `mapbox styles`
    /// and `mapbox styles draft` both fail at parse and no lookup happens. `tilesets-cli` is the one exception, being a command
    /// rather than a service. `every_deprecated_command_is_a_typeable_path`
    /// in `main.rs` rejects both.
    pub command: &'static str,
    /// The crate version that deprecated it, so the warning can say how long
    /// the notice has been up rather than just that it exists.
    pub since: &'static str,
    /// What to use instead, typed the same way. `None` where nothing
    /// replaces it.
    pub replacement: Option<&'static str>,
}

/// Empty, and accurately so: nothing in the current surface is deprecated.
///
/// Here ahead of its first entry because the alternative is a rename
/// shipping with no way to warn about it — a deprecation is only useful if
/// it is announced *before* the removal, which means the mechanism has to
/// predate the first one. `every_deprecated_command_is_a_command` in
/// `main.rs` holds these to the real command tree, so the first entry cannot
/// quietly be a typo that warns nobody.
pub const DEPRECATED_COMMANDS: &[Deprecation] = &[];

/// The key both halves look a command up by: what the person typed, minus
/// `mapbox`.
///
/// Here rather than as a `format!` at each call site because the runtime
/// warning and `--schema` have to agree about it exactly — a command spelled
/// one way in one and another way in the other is a deprecation that warns
/// in one place and not the other.
pub fn path(service: &str, command: &str) -> String {
    format!("{service} {command}")
}

/// The CLI's own notice for a command, if it has one.
pub fn find(command: &str) -> Option<&'static Deprecation> {
    DEPRECATED_COMMANDS
        .iter()
        .find(|entry| entry.command == command)
}

/// Warns if this CLI has deprecated the command that was typed.
pub fn warn_command(command: &str) {
    if let Some(entry) = find(command) {
        output::progress(&command_notice(entry));
    }
}

/// Warns if the endpoint a command calls is deprecated in its own spec.
///
/// Takes the operation rather than a bare flag so that it guards itself, the
/// way [`warn_command`] does. The two are named as a pair and would
/// otherwise have opposite contracts: a second caller that mirrored
/// `warn_command`'s call site would announce every command in the CLI as
/// deprecated.
pub fn warn_endpoint(op: &crate::spec::Operation, command: &str) {
    if !op.deprecated {
        return;
    }
    output::progress(&endpoint_notice(command));
}

/// Kept apart from the printing so the wording is testable without a
/// process, which is the only way the empty table above can be covered at
/// all: a test builds its own entry rather than the repo shipping a
/// deprecation nobody asked for.
fn command_notice(entry: &Deprecation) -> String {
    let mut notice = format!(
        "Warning: `mapbox {}` has been deprecated since {} and will be removed in a later release.",
        entry.command, entry.since
    );
    if let Some(replacement) = entry.replacement {
        notice.push_str(&format!(" Use `mapbox {replacement}` instead."));
    }
    notice
}

/// Deliberately says nothing about the command's future: the endpoint's
/// retirement is Mapbox's to announce, and until it happens the command
/// works exactly as it did.
fn endpoint_notice(command: &str) -> String {
    format!(
        "Warning: the endpoint behind `mapbox {command}` is marked deprecated by its OpenAPI \
         spec and may stop answering. The command is unchanged until it does."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_names_the_version_and_the_replacement() {
        let notice = command_notice(&Deprecation {
            command: "styles old-name",
            since: "0.2.0",
            replacement: Some("styles new-name"),
        });
        assert_eq!(
            notice,
            "Warning: `mapbox styles old-name` has been deprecated since 0.2.0 and will be \
             removed in a later release. Use `mapbox styles new-name` instead."
        );
    }

    /// Not every deprecation has somewhere to send people, and inventing one
    /// is worse than saying nothing — the sentence is simply left off.
    #[test]
    fn a_notice_with_nothing_to_recommend_recommends_nothing() {
        let notice = command_notice(&Deprecation {
            command: "styles old-name",
            since: "0.2.0",
            replacement: None,
        });
        assert!(
            notice.ends_with("will be removed in a later release."),
            "{notice}"
        );
        assert!(!notice.contains("instead"), "{notice}");
    }

    /// The two notices answer different questions, and a caller reading
    /// stderr has only the wording to tell them apart.
    #[test]
    fn an_endpoint_notice_is_not_a_command_notice() {
        let notice = endpoint_notice("maps get-legacy-tile");
        assert!(
            notice.contains("marked deprecated by its OpenAPI spec"),
            "{notice}"
        );
        assert!(!notice.contains("will be removed"), "{notice}");
    }

    #[test]
    fn nothing_is_deprecated_yet() {
        assert!(DEPRECATED_COMMANDS.is_empty());
        assert!(find(&path("styles", "get-style")).is_none());
    }
}
