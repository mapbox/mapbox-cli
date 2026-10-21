//! Whether a destructive command may go ahead.
//!
//! The CLI is non-interactive by default, and that is a contract rather than
//! an accident: nothing in this process ever blocks waiting for a human
//! unless it can see one. A question is only ever asked when *both* stdin and
//! stderr are terminals — stdin because that is where the answer has to come
//! from, stderr because that is where the question is printed, and a question
//! nobody can read is indistinguishable from a hang. With either redirected —
//! CI, a container, an agent driving the CLI, `curl … | sh` — the request goes
//! out unasked.
//!
//! `--yes` says the answer is yes before it is asked, which is the only way a
//! caller *at* a terminal can get the non-interactive behavior on purpose.
//!
//! One question is the exception, and refuses rather than proceeds when nobody
//! can answer it: installing a workflow from a repository the CLI does not
//! vouch for. See [`untrusted_install`].
//!
//! The question is deliberately not `--output`-aware. It goes to stderr as
//! prose in both modes, because the only way to see it is to be a person at a
//! terminal, and a person reads prose. stdout still carries nothing but the
//! result.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::Result;

use crate::output::CliError;
use crate::remedy::Remedy;

/// The `--yes` arg's id, and its short and environment spellings.
pub const ARG: &str = "yes";
pub const SHORT: char = 'y';
pub const ENV: &str = "MAPBOX_YES";

/// Whether the request is worth a question.
///
/// Only `DELETE`. The other three mutating verbs write something the caller
/// can read back and write again; a delete removes a name other things
/// reference, and how recoverable that is varies per endpoint — Mapbox keeps
/// a style `styles delete` removed for 30 days, while a `sprites delete` is
/// gone the moment it answers. That spread is the argument for asking rather
/// than for picking a subset: the CLI would have to know which is which, from
/// a spec that does not say.
///
/// Widening this to `POST`/`PUT`/`PATCH` would put a question in front of
/// every style upload and sprite write, which is the kind of prompt that
/// trains people to answer without reading.
fn is_destructive(method: &str) -> bool {
    method == "DELETE"
}

/// What to do about a request before sending it.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Send it: it is not destructive, consent is already on the command
    /// line, or there is no terminal to ask on.
    Proceed,
    /// A person is watching and has not said yes yet.
    Ask,
}

/// The decision, with no I/O in it, so every branch is testable without a
/// terminal — which is the one thing a test process cannot have.
pub fn decide(
    method: &str,
    assume_yes: bool,
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
) -> Decision {
    if !is_destructive(method) || assume_yes {
        return Decision::Proceed;
    }
    if stdin_is_terminal && stderr_is_terminal {
        Decision::Ask
    } else {
        Decision::Proceed
    }
}

/// Whether an answer typed at the prompt was a yes.
///
/// Anything else is a no, including an empty line — the prompt shows `[y/N]`
/// and a bare Enter has to mean what the capital letter says.
fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// A refusal, and the flag that would have avoided the question.
///
/// An error rather than a silent exit 0: whoever gets here needs the exit code
/// to say the delete did not happen. Two callers reach it — a typed `n`, and
/// EOF part-way through the question, which is what a piped stdout at a
/// terminal produces when nothing answers. The code is stable so a caller can
/// branch on it instead of matching the prose.
fn cancelled(outcome: &str) -> anyhow::Error {
    // No `next_actions`, deliberately. The only command that would answer
    // this is the delete that was just declined, with `--yes` bolted on —
    // handing that back to whoever typed `n` is the one suggestion this CLI
    // must not make. The flag is named in the prose, where a person decides.
    CliError::new("cancelled", format!("Cancelled — {outcome}."))
        .with_remedy(
            Remedy::default().with_fix("Pass --yes (or set MAPBOX_YES=1) to skip this question."),
        )
        .into()
}

/// Asks before a destructive request, when there is someone to ask.
///
/// `url` must be the request URL *before* the query string is attached: the
/// access token rides in `access_token=`, and this string is printed. See
/// `executor::execute`, which calls this at exactly that point.
pub fn destructive_request(method: &str, url: &str, assume_yes: bool) -> Result<()> {
    let decision = decide(
        method,
        assume_yes,
        std::io::stdin().is_terminal(),
        std::io::stderr().is_terminal(),
    );
    if decision == Decision::Proceed {
        return Ok(());
    }
    // No "this cannot be undone": it is not true of every delete here, and a
    // warning the reader can catch out is worse than none. The method and the
    // resource are the part that lets them catch out the *command*.
    ask(&format!("About to {method} {url}"), "nothing was sent")
}

/// Asks a yes/no question on stderr/stdin, for a destructive local action
/// that (unlike `destructive_request`) is not an HTTP request — `uninstall`,
/// so far. The caller has already decided the question is worth asking
/// (typically via [`decide`]); this is just the I/O.
fn ask(question: &str, outcome: &str) -> Result<()> {
    let mut stderr = std::io::stderr();
    // Ignored deliberately: a broken stderr cannot be reported *on* stderr,
    // and the read below will fail too, which is where the refusal comes from.
    let _ = writeln!(stderr, "{question}");
    let _ = write!(stderr, "Continue? [y/N] ");
    let _ = stderr.flush();

    let mut answer = String::new();
    match std::io::stdin().lock().read_line(&mut answer) {
        // Ok(0) is EOF — the terminal went away mid-question. Treated as a
        // no, like every other answer that is not a yes.
        Ok(_) if is_yes(&answer) => Ok(()),
        _ => Err(cancelled(outcome)),
    }
}

/// Asks before removing the installed binary, when there is someone to ask.
///
/// Shares [`decide`]'s notion of "is anyone watching" with
/// `destructive_request`, but the action isn't an HTTP method — there is no
/// verb to gate on, so this is always worth asking about (subject to the
/// same terminal check and `--yes`).
pub fn destructive_local_action(question: &str, assume_yes: bool) -> Result<()> {
    if assume_yes || !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Ok(());
    }
    ask(question, "nothing was sent")
}

/// Asks before installing a workflow from a source the CLI does not vouch
/// for, and refuses when there is nobody to ask.
///
/// Everywhere else no terminal means go ahead. Here that would install code
/// that later runs as the caller, with their credentials, on nobody's
/// say-so — a mistyped `--repo` in CI would be enough. So without a terminal
/// it takes `--yes`.
pub fn untrusted_install(question: &str, assume_yes: bool) -> Result<()> {
    let at_terminal = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    match untrusted_decision(assume_yes, at_terminal) {
        Some(Decision::Proceed) => Ok(()),
        Some(Decision::Ask) => ask(question, "nothing was installed"),
        None => Err(CliError::new(
            "confirmation_required",
            format!("{question}\nNothing was installed: there is no terminal to confirm it on."),
        )
        .with_remedy(
            Remedy::default()
                .with_fix("Pass --yes (or set MAPBOX_YES=1) once you trust what it runs."),
        )
        .into()),
    }
}

/// [`untrusted_install`]'s choice, with no I/O in it: `None` is a refusal.
fn untrusted_decision(assume_yes: bool, at_terminal: bool) -> Option<Decision> {
    if assume_yes {
        Some(Decision::Proceed)
    } else if at_terminal {
        Some(Decision::Ask)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{decide, destructive_local_action, is_yes, untrusted_decision, Decision};

    /// Unlike a delete, no terminal is a refusal: code from an unvetted
    /// repository is not installed on nobody's say-so.
    #[test]
    fn an_untrusted_install_needs_yes_or_someone_to_ask() {
        assert_eq!(untrusted_decision(true, false), Some(Decision::Proceed));
        assert_eq!(untrusted_decision(true, true), Some(Decision::Proceed));
        assert_eq!(untrusted_decision(false, true), Some(Decision::Ask));
        assert_eq!(untrusted_decision(false, false), None);
    }

    /// The whole point of the flag: it makes a terminal behave like CI.
    #[test]
    fn yes_proceeds_even_at_a_terminal() {
        assert_eq!(decide("DELETE", true, true, true), Decision::Proceed);
    }

    #[test]
    fn a_delete_at_a_terminal_is_worth_asking_about() {
        assert_eq!(decide("DELETE", false, true, true), Decision::Ask);
    }

    /// Non-interactive by default. Each of these is a real caller: a CI job,
    /// `curl … | sh` where stdin genuinely is the pipe, an agent holding both
    /// pipes.
    ///
    /// Note what is *not* on that list: a command whose stdout is piped. This
    /// function is not given stdout and does not consult it, so
    /// `… styles delete x | jq .` at a terminal still asks.
    #[test]
    fn no_terminal_means_no_question() {
        for (stdin, stderr) in [(false, false), (false, true), (true, false)] {
            assert_eq!(
                decide("DELETE", false, stdin, stderr),
                Decision::Proceed,
                "stdin_is_terminal={stdin}, stderr_is_terminal={stderr}"
            );
        }
    }

    /// A question printed where nobody can read it is a hang, not a prompt —
    /// so a redirected stderr has to be enough on its own to skip it.
    #[test]
    fn a_readable_answer_is_not_enough_without_a_readable_question() {
        assert_eq!(decide("DELETE", false, true, false), Decision::Proceed);
    }

    #[test]
    fn only_a_delete_is_asked_about() {
        for method in ["GET", "POST", "PUT", "PATCH"] {
            assert_eq!(
                decide(method, false, true, true),
                Decision::Proceed,
                "{method} was asked about"
            );
        }
    }

    #[test]
    fn yes_is_spelled_two_ways_in_either_case() {
        for answer in ["y", "Y", "yes", "YES", "  yes\n", "y\r\n"] {
            assert!(is_yes(answer), "{answer:?} was not read as a yes");
        }
    }

    /// A bare Enter is the `N` in `[y/N]`, and a typo is not a yes either.
    #[test]
    fn everything_else_is_a_no() {
        for answer in ["", "\n", "n", "no", "ya", "yep", "1", "sure"] {
            assert!(!is_yes(answer), "{answer:?} was read as a yes");
        }
    }

    /// A test process has no terminal on either stream, so this always takes
    /// the same "nobody to ask" branch `no_terminal_means_no_question`
    /// exercises for `decide` — which is the only branch a test can reach
    /// without one.
    #[test]
    fn destructive_local_action_proceeds_without_a_terminal() {
        assert!(destructive_local_action("About to delete x", false).is_ok());
    }
}
