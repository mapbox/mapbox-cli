//! Requests the CLI makes for itself rather than for the user, and the token
//! they carry.
//!
//! # Telemetry sends with the CLI's token alone
//!
//! Mapbox Events keeps the token an event was sent with beside the event,
//! and the request logs record it as `access_token`. A user's token there
//! would store a login's `tk.` token, with its write scopes, or a secret
//! `sk.` one, and make it a key shared between those records. So [`send`]
//! never takes the user's token, and with no usable CLI token the event is
//! dropped. It sends with:
//!
//! 1. `MAPBOX_CLI_TOKEN` at run time, when set: the CLI's token chosen by a
//!    developer, such as one for staging. Sent as it is, never checked,
//!    counted or replaced.
//! 2. Otherwise the token published on the release channel, fetched and kept
//!    on disk until the Tokens API calls it dead. See [`fetch`] for when it
//!    is fetched again, and for a caller's rest from it.
//!
//! One token, one attempt: an event is never sent twice. A refusal is still
//! checked at once ([`send_with_cli_token`]), and a dead token replaced
//! there and then, so the next run's event goes with the new one.
//!
//! # What may use the CLI's token
//!
//! Every request made with the CLI's token is billed to the account that
//! owns it. So it is decided by what the request is for, not where it goes:
//! a request the CLI makes for itself goes through [`send`], and its caller
//! is named in this file (today only [`TELEMETRY_CALLER`]); a command must be
//! in [`CLI_TOKEN_COMMANDS`], which also says how it uses it ([`Use`]).
//! Where the request is sent does not matter, so a URL overridden from the
//! environment keeps the same token.
//!
//! Either way the token is sent through [`send_with_cli_token`], which holds
//! the rules: check a refusal at once, replace a dead token, rest from an API
//! that refuses a valid one. A command, unlike telemetry, sends again with a
//! fresh token, and one that cannot use the CLI's token falls back to the
//! user's, and with none says why: unavailable right now ([`Unavailable`])
//! or refused by this API ([`Refusal`]).
//!
//! # The token is public
//!
//! It is served from a public URL, so it must be a dedicated `pk.` token with
//! the minimum scopes, and nothing may ever treat it as a secret. [`fetch`]
//! refuses anything that is not `pk.`.

use reqwest::blocking::{RequestBuilder, Response};

use crate::http;

mod fetch;

pub(crate) use fetch::{Refusal, Unavailable};

/// The `Referer` every request with the CLI's token carries. The published
/// token is restricted to this URL, so that a web page elsewhere cannot use
/// it; without the header, the API answers `401`. It keeps nothing out of a
/// program that sets the header itself, which is all the restriction can do
/// for a token anyone can read.
pub(crate) const REFERER: &str = "https://cli.mapbox.com/";

/// Telemetry's name as a caller of the CLI's token, for its cooldown.
pub(crate) const TELEMETRY_CALLER: &str = "telemetry";

/// How a command listed in [`CLI_TOKEN_COMMANDS`] uses the CLI's token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Use {
    /// The CLI's token first, and the user's own only while the CLI's
    /// cannot be used: unavailable, resting, or refused by this API.
    Preferred,
}

/// Commands that may run on the CLI's token, by the name `--schema` reports,
/// each with how and with the reason the CLI's account should carry their
/// use. Command names are a compatibility promise, so an entry does not
/// quietly move to another API when a spec changes its URL.
///
/// A command here must not read its body from stdin: a refused request is
/// sent again, with a fresh token or the user's, and stdin can only be read
/// once. `cli_token_commands_read_nothing_from_stdin` holds that.
const CLI_TOKEN_COMMANDS: &[(&str, Use, &str)] = &[(
    "report",
    Use::Preferred,
    "Feedback about the CLI is filed under Mapbox's own account, so it is the \
     CLI's request rather than the user's, and it works for someone not \
     logged in.",
)];

/// Whether `command` may run on the CLI's token at all.
pub(crate) fn may_use(command: &str) -> bool {
    use_for(command).is_some()
}

fn use_for(command: &str) -> Option<Use> {
    CLI_TOKEN_COMMANDS
        .iter()
        .find(|(name, _, _)| *name == command)
        .map(|(_, how, _)| *how)
}

/// The CLI's token as a command got it, for [`send_for_command`], which
/// handles a refusal of it by where it came from.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CommandToken {
    pub(crate) token: String,
    source: TokenSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenSource {
    /// `MAPBOX_CLI_TOKEN`.
    Override,
    /// The one on disk.
    Saved,
    /// Fetched by this run.
    Fetched,
}

/// The CLI's token for `command`, or `None` when the command is not listed
/// for it: the only way to ask, so an unlisted command never gets one.
/// `fetch` is false where nothing may be sent, as in a dry run, and only a
/// token already on disk is returned then.
pub(crate) fn for_command(command: &str, fetch: bool) -> Option<Result<CommandToken, Unavailable>> {
    match use_for(command)? {
        Use::Preferred => Some(preferred_token(fetch)),
    }
}

fn preferred_token(fetch: bool) -> Result<CommandToken, Unavailable> {
    let given = |token: String, source| CommandToken { token, source };
    if let Some(token) = usable(std::env::var("MAPBOX_CLI_TOKEN").ok().as_deref()) {
        return Ok(given(token.to_owned(), TokenSource::Override));
    }
    if let Some(token) = fetch::saved_token() {
        return Ok(given(token, TokenSource::Saved));
    }
    if !fetch {
        return Err(Unavailable::NotFetched);
    }
    // A command is not held by the backoff, which paces background fetches:
    // someone is waiting, and the endpoint may have a new token by now.
    fetch::token_now().map(|token| given(token, TokenSource::Fetched))
}

/// Sends a command's request with the CLI's token by [`send_with_cli_token`]'s
/// rules, as [`Disk::Command`] for a token from disk or the endpoint.
/// `MAPBOX_CLI_TOKEN` is sent once and never checked, counted or replaced,
/// as [`send`] treats it: a developer chose that token, and a refusal of it
/// is theirs to see.
pub(crate) fn send_for_command<R>(
    command: &str,
    given: CommandToken,
    send: impl FnMut(&str) -> R,
    status: impl Fn(&R) -> Option<u16>,
) -> Sent<R> {
    let mut ledger = match given.source {
        TokenSource::Override => Disk::Unchecked,
        TokenSource::Saved => Disk::Command { fetched: false },
        TokenSource::Fetched => Disk::Command { fetched: true },
    };
    send_with_cli_token(command, given.token, &mut ledger, send, status)
}

/// Sends `request` once with the CLI's token attached as `access_token`, on
/// `caller`'s behalf: `MAPBOX_CLI_TOKEN` when set, otherwise the fetched
/// token by [`send_with_cli_token`]'s rules, which for a caller in the
/// background never send again ([`Disk::Background`]).
///
/// `None` when there is no CLI token to use right now; an error when the
/// request could not be sent at all; otherwise the answer, a refusal
/// included.
pub(crate) fn send(
    caller: &str,
    request: impl Fn() -> RequestBuilder,
) -> Option<reqwest::Result<Response>> {
    let send = |token: &str| {
        http::send(
            request()
                .query(&[("access_token", token)])
                .header(reqwest::header::REFERER, REFERER),
        )
    };
    let override_token = std::env::var("MAPBOX_CLI_TOKEN").ok();
    let token = match chosen(override_token.as_deref(), || fetch::token_for(caller))? {
        Chosen::Override(token) => return Some(send(&token)),
        Chosen::Fetched(token) => token,
    };
    match send_with_cli_token(caller, token, &mut Disk::Background, send, |answer| {
        answer.as_ref().ok().map(|r| r.status().as_u16())
    }) {
        Sent::Answered(answer) | Sent::Refused(answer, _) => Some(answer),
    }
}

/// Which CLI token [`send`] uses.
#[derive(Debug, PartialEq, Eq)]
enum Chosen {
    Override(String),
    Fetched(String),
}

/// `MAPBOX_CLI_TOKEN` when it is set to something, without loading the
/// fetched token at all, so a developer's override never costs a fetch.
fn chosen(
    override_token: Option<&str>,
    fetched: impl FnOnce() -> Option<String>,
) -> Option<Chosen> {
    match usable(override_token) {
        Some(token) => Some(Chosen::Override(token.to_owned())),
        None => fetched().map(Chosen::Fetched),
    }
}

/// What a request sent with the CLI's token came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Sent<R> {
    /// An answer that was not a refusal, or no answer at all: the caller's.
    Answered(R),
    /// A `401` or `403` that replacing a dead token did not fix, and why.
    Refused(R, Refusal),
}

/// The state on disk, as [`send_with_cli_token`] uses it, so its rules can
/// be tested without it.
pub(crate) trait Ledger {
    fn refused(&mut self, caller: &str, token: &str) -> Refusal;
    fn accepted(&mut self, caller: &str, token: &str);
    /// Whether a request refused for `refusal` is worth sending again with
    /// a [`fresh`](Ledger::fresh) token.
    fn retries(&self, refusal: Refusal) -> bool;
    fn fresh(&mut self) -> Option<String>;
}

/// `<config dir>/cli-token.json`, through [`fetch`], by kind of caller.
pub(crate) enum Disk {
    /// Nobody is waiting, as for telemetry: paced, and never sent again. A
    /// dead token is still replaced at once, when the backoff allows, so
    /// that the next run sends with the new one.
    Background,
    /// Someone ran it and is waiting: never paced, and any refusal tries a
    /// token fetched there and then, so a token republished or fixed in the
    /// Console works on the next try. Not when this run `fetched` the token
    /// already: asking again at once would only get the same one back.
    Command { fetched: bool },
    /// A token not from the endpoint (`MAPBOX_CLI_TOKEN`): the file is not
    /// touched, and a refusal is handed back as it is.
    Unchecked,
}

impl Ledger for Disk {
    fn refused(&mut self, caller: &str, token: &str) -> Refusal {
        match self {
            Disk::Background => {
                let refusal = fetch::refused(caller, token, true);
                if refusal == Refusal::Dead {
                    // Kept on disk for the next run; this one's event is not
                    // sent again.
                    let _ = fetch::token();
                }
                refusal
            }
            Disk::Command { .. } => fetch::refused(caller, token, false),
            Disk::Unchecked => Refusal::NotFetched,
        }
    }
    fn accepted(&mut self, caller: &str, token: &str) {
        if !matches!(self, Disk::Unchecked) {
            fetch::accepted(caller, token);
        }
    }
    fn retries(&self, _: Refusal) -> bool {
        match self {
            Disk::Background => false,
            Disk::Command { fetched } => !fetched,
            Disk::Unchecked => false,
        }
    }
    fn fresh(&mut self) -> Option<String> {
        match self {
            Disk::Background => fetch::token(),
            Disk::Command { .. } => fetch::token_now().ok(),
            Disk::Unchecked => None,
        }
    }
}

/// Sends with the CLI's `token` on `caller`'s behalf: the one place the
/// rules for using it live, for telemetry and commands alike.
///
/// A `2xx` ends `caller`'s cooldown. A `401` or `403` is checked with the
/// Tokens API at once ([`fetch::refused`]); when the ledger says it is worth
/// it ([`Ledger::retries`]) and a different token is to be had, the request
/// is sent once more with it, since a refused request was not acted on.
/// Otherwise the refusal is handed back. `status` is an answer's HTTP status,
/// or `None` when there was no answer.
pub(crate) fn send_with_cli_token<R>(
    caller: &str,
    mut token: String,
    ledger: &mut impl Ledger,
    mut send: impl FnMut(&str) -> R,
    status: impl Fn(&R) -> Option<u16>,
) -> Sent<R> {
    let mut replaced = false;
    loop {
        let answer = send(&token);
        match status(&answer) {
            Some(401 | 403) => {
                let refusal = ledger.refused(caller, &token);
                if ledger.retries(refusal) && !replaced {
                    if let Some(fresh) = ledger.fresh().filter(|fresh| *fresh != token) {
                        token = fresh;
                        replaced = true;
                        continue;
                    }
                }
                return Sent::Refused(answer, refusal);
            }
            Some(200..=299) => ledger.accepted(caller, &token),
            _ => {}
        }
        return Sent::Answered(answer);
    }
}

/// Whether [`send`] has a CLI token it could try for `caller`, judged
/// without the network, so a caller can skip work that would only find
/// nothing to send with.
pub(crate) fn has_token_for(caller: &str) -> bool {
    usable(std::env::var("MAPBOX_CLI_TOKEN").ok().as_deref()).is_some()
        || fetch::may_have_token(caller)
}

fn usable(token: Option<&str>) -> Option<&str> {
    token.map(str::trim).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_is_sent_trimmed_and_never_fetches() {
        let chosen = chosen(Some(" pk.override\n"), || panic!("fetched"));
        assert_eq!(chosen, Some(Chosen::Override("pk.override".to_owned())));
    }

    #[test]
    fn without_an_override_the_fetched_token_is_sent() {
        for blank in [None, Some(""), Some("  ")] {
            assert_eq!(
                chosen(blank, || Some("pk.fetched".to_owned())),
                Some(Chosen::Fetched("pk.fetched".to_owned())),
                "{blank:?}"
            );
            assert_eq!(
                chosen(blank, || None),
                None,
                "{blank:?}: nothing to send with"
            );
        }
    }

    #[test]
    fn telemetry_never_sends_again() {
        for refusal in [
            Refusal::Dead,
            Refusal::Forbidden,
            Refusal::Unknown,
            Refusal::NotFetched,
        ] {
            assert!(!Disk::Background.retries(refusal), "{refusal:?}");
        }
    }

    #[test]
    fn every_cli_token_command_exists_and_says_why() {
        let specs = crate::spec::effective_services().expect("the bundled specs parse");
        let commands: Vec<String> = specs
            .iter()
            .flat_map(|svc| &svc.operations)
            .map(|op| op.command())
            .collect();
        for (name, _, reason) in CLI_TOKEN_COMMANDS {
            assert!(
                commands.iter().any(|c| c == name),
                "`{name}` is in CLI_TOKEN_COMMANDS but is not a command"
            );
            assert!(!reason.trim().is_empty(), "`{name}` needs a reason");
        }
    }

    #[test]
    fn cli_token_commands_read_nothing_from_stdin() {
        let specs = crate::spec::effective_services().expect("the bundled specs parse");
        for op in specs.iter().flat_map(|svc| &svc.operations) {
            if use_for(&op.command()).is_none() {
                continue;
            }
            let cmd = crate::build_operation_command(op);
            for id in ["data", "file"] {
                assert!(
                    cmd.get_arguments().all(|arg| arg.get_id() != id),
                    "`{}` takes `--{id}`, which can read stdin, and a refused request \
                     is sent again",
                    op.command()
                );
            }
        }
    }

    #[test]
    fn an_unlisted_command_never_gets_the_clis_token() {
        assert_eq!(for_command("styles list", true), None);
    }

    /// A ledger that answers every refusal with `verdict` and has `fresh`
    /// to hand out once, and records what it was told.
    #[derive(Default)]
    struct FakeLedger {
        /// Retries after any refusal, as [`Disk::Command`] does, rather than
        /// only a dead token.
        command: bool,
        verdict: Option<Refusal>,
        fresh: Option<String>,
        refused: Vec<String>,
        accepted: Vec<String>,
    }

    impl Ledger for FakeLedger {
        fn refused(&mut self, _: &str, token: &str) -> Refusal {
            self.refused.push(token.to_owned());
            self.verdict.expect("a verdict to give")
        }
        fn accepted(&mut self, _: &str, token: &str) {
            self.accepted.push(token.to_owned());
        }
        fn retries(&self, refusal: Refusal) -> bool {
            self.command || refusal == Refusal::Dead
        }
        fn fresh(&mut self) -> Option<String> {
            self.fresh.take()
        }
    }

    /// Sends through [`send_with_cli_token`] to a fake API that answers
    /// `statuses` in turn, and returns the outcome and the tokens sent.
    fn send_cli(
        ledger: &mut FakeLedger,
        statuses: &[Option<u16>],
    ) -> (Sent<Option<u16>>, Vec<String>) {
        let mut sent = Vec::new();
        let mut answers = statuses.iter().copied();
        let outcome = send_with_cli_token(
            "report",
            "pk.old".to_owned(),
            ledger,
            |token| {
                sent.push(token.to_owned());
                answers.next().expect("an answer for each send")
            },
            |status| *status,
        );
        (outcome, sent)
    }

    #[test]
    fn a_dead_token_is_replaced_and_sent_once_more() {
        let mut ledger = FakeLedger {
            verdict: Some(Refusal::Dead),
            fresh: Some("pk.new".to_owned()),
            ..FakeLedger::default()
        };
        let (outcome, sent) = send_cli(&mut ledger, &[Some(401), Some(200)]);
        assert_eq!(outcome, Sent::Answered(Some(200)));
        assert_eq!(sent, ["pk.old", "pk.new"]);
        assert_eq!(ledger.accepted, ["pk.new"]);
    }

    #[test]
    fn a_replacement_that_is_refused_too_is_not_replaced_again() {
        let mut ledger = FakeLedger {
            verdict: Some(Refusal::Dead),
            fresh: Some("pk.new".to_owned()),
            ..FakeLedger::default()
        };
        let (outcome, sent) = send_cli(&mut ledger, &[Some(401), Some(401)]);
        assert_eq!(outcome, Sent::Refused(Some(401), Refusal::Dead));
        assert_eq!(sent, ["pk.old", "pk.new"]);
    }

    #[test]
    fn a_dead_token_with_no_new_one_is_handed_back() {
        let mut ledger = FakeLedger {
            verdict: Some(Refusal::Dead),
            fresh: Some("pk.old".to_owned()),
            ..FakeLedger::default()
        };
        let (outcome, sent) = send_cli(&mut ledger, &[Some(403)]);
        assert_eq!(outcome, Sent::Refused(Some(403), Refusal::Dead));
        assert_eq!(sent, ["pk.old"], "the same token is not sent again");
    }

    #[test]
    fn a_token_this_api_refuses_is_handed_back_without_a_fetch() {
        for verdict in [Refusal::Forbidden, Refusal::Unknown] {
            let mut ledger = FakeLedger {
                verdict: Some(verdict),
                fresh: Some("pk.new".to_owned()),
                ..FakeLedger::default()
            };
            let (outcome, sent) = send_cli(&mut ledger, &[Some(403)]);
            assert_eq!(outcome, Sent::Refused(Some(403), verdict));
            assert_eq!(sent, ["pk.old"]);
            assert!(ledger.fresh.is_some(), "{verdict:?} fetched a new token");
        }
    }

    #[test]
    fn a_command_tries_a_fresh_token_after_any_refusal() {
        for verdict in [Refusal::Forbidden, Refusal::Unknown, Refusal::NotFetched] {
            let mut ledger = FakeLedger {
                command: true,
                verdict: Some(verdict),
                fresh: Some("pk.new".to_owned()),
                ..FakeLedger::default()
            };
            let (outcome, sent) = send_cli(&mut ledger, &[Some(403), Some(200)]);
            assert_eq!(outcome, Sent::Answered(Some(200)), "{verdict:?}");
            assert_eq!(sent, ["pk.old", "pk.new"], "{verdict:?}");
        }
    }

    #[test]
    fn a_command_gets_the_same_token_back_and_does_not_resend_it() {
        let mut ledger = FakeLedger {
            command: true,
            verdict: Some(Refusal::Forbidden),
            fresh: Some("pk.old".to_owned()),
            ..FakeLedger::default()
        };
        let (outcome, sent) = send_cli(&mut ledger, &[Some(403)]);
        assert_eq!(outcome, Sent::Refused(Some(403), Refusal::Forbidden));
        assert_eq!(sent, ["pk.old"]);
    }

    #[test]
    fn a_command_token_just_fetched_or_overridden_is_not_replaced() {
        for (ledger, retries) in [
            (Disk::Command { fetched: false }, true),
            (Disk::Command { fetched: true }, false),
            (Disk::Unchecked, false),
        ] {
            for refusal in [
                Refusal::Dead,
                Refusal::Forbidden,
                Refusal::Unknown,
                Refusal::NotFetched,
            ] {
                assert_eq!(ledger.retries(refusal), retries, "{refusal:?}");
            }
        }
    }

    #[test]
    fn an_override_is_never_checked_or_replaced() {
        let mut ledger = Disk::Unchecked;
        assert_eq!(ledger.refused("report", "pk.override"), Refusal::NotFetched);
        assert_eq!(ledger.fresh(), None);
    }

    #[test]
    fn only_a_2xx_counts_as_accepted() {
        for status in [Some(200), Some(204), Some(404), Some(500), None] {
            let mut ledger = FakeLedger::default();
            let (outcome, _) = send_cli(&mut ledger, &[status]);
            assert_eq!(outcome, Sent::Answered(status));
            let accepted = status.is_some_and(|s| (200..300).contains(&s));
            assert_eq!(ledger.accepted.len(), usize::from(accepted), "{status:?}");
            assert!(ledger.refused.is_empty(), "{status:?} was checked");
        }
    }
}
