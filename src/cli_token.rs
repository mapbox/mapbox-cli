//! Requests the CLI makes for itself rather than for the user, and the token
//! they carry.
//!
//! # Who may use the CLI's token
//!
//! Every request made with the CLI's token is billed to the account that
//! owns it. So what a request is for decides whether it may use the token,
//! not where it goes. Each caller is named in this file with its [`Policy`]:
//! telemetry as [`TELEMETRY_CALLER`], and each command in
//! [`CLI_TOKEN_COMMANDS`] with the reason it qualifies. A URL overridden
//! from the environment keeps the same token.
//!
//! # One policy per caller
//!
//! A caller's [`Policy`] decides everything that differs between callers:
//!
//! - **paced**: count failures in a row and go quiet after three, as
//!   [`fetch`] explains. An unpaced caller fetches whenever it needs a token
//!   and keeps no count, because someone is waiting on it.
//! - **resends**: after a refusal, send once more with a different, newly
//!   fetched token, so a token republished or fixed in the Console works on
//!   the next try. That is safe because a refused request had no effect.
//! - **user_fallback**: when the CLI's token can't be used, the caller may
//!   send with the user's token instead. Otherwise the request is dropped.
//!
//! Telemetry is paced, never resends and never uses the user's token:
//! Mapbox Events stores the token each event was sent with, and its request
//! logs record it as `access_token`. A user's token there would leave a login
//! (`tk.`, with write scopes) or secret (`sk.`) token in those records, and
//! the token would link them together. A command such as `mapbox report` is
//! the opposite on all three. With no user token either, it says why the
//! CLI's could not be used ([`Unavailable`], [`Refusal`]).
//!
//! The token a caller sends is:
//!
//! 1. `MAPBOX_CLI_TOKEN`, when set. A developer sets it to use another CLI
//!    token, such as one for staging. It is never checked, counted or
//!    replaced, and is sent even while the caller is quiet. A caller without
//!    `user_fallback` sends it only if it is a `pk.` token.
//! 2. Otherwise the token published for the release channel, fetched and
//!    saved until the Tokens API says it is dead ([`fetch`]).
//!
//! Every send goes through [`send_with_cli_token`], which checks a refusal
//! right away. A dead token is replaced on disk whatever the policy, but
//! only a caller that resends uses the new one in the same run. Telemetry
//! uses it on its next run.
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

/// How a caller uses the CLI's token. The module docs explain each flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
    paced: bool,
    resends: bool,
    user_fallback: bool,
}

/// Nobody is waiting, and the event must never carry the user's token.
pub(crate) const TELEMETRY: Policy = Policy {
    paced: true,
    resends: false,
    user_fallback: false,
};

/// Someone ran it and is waiting, and their own token can stand in.
pub(crate) const COMMAND: Policy = Policy {
    paced: false,
    resends: true,
    user_fallback: true,
};

/// Something that sends with the CLI's token. Failures are counted under
/// its name.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Caller {
    name: &'static str,
    policy: Policy,
}

pub(crate) static TELEMETRY_CALLER: Caller = Caller {
    name: "telemetry",
    policy: TELEMETRY,
};

/// Commands that may run on the CLI's token, by the name `--schema` reports,
/// each with the reason the CLI's account should carry their use. Command
/// names are a compatibility promise, so an entry does not quietly move to
/// another API when a spec changes its URL.
///
/// A command here must not read its body from stdin: a refused request is
/// sent again, with a fresh token or the user's, and stdin can only be read
/// once. `cli_token_commands_read_nothing_from_stdin` holds that.
const CLI_TOKEN_COMMANDS: &[(Caller, &str)] = &[(
    Caller {
        name: "report",
        policy: COMMAND,
    },
    "Feedback about the CLI is filed under Mapbox's own account, so it is the \
     CLI's request rather than the user's, and it works for someone not \
     logged in.",
)];

/// The caller for `command`, or `None` when the command may not use the
/// CLI's token: the only way to get one, so an unlisted command never does.
pub(crate) fn command(command: &str) -> Option<&'static Caller> {
    CLI_TOKEN_COMMANDS
        .iter()
        .find(|(caller, _)| caller.name == command)
        .map(|(caller, _)| caller)
}

/// Whether `command` may run on the CLI's token at all.
pub(crate) fn may_use(command: &str) -> bool {
    self::command(command).is_some()
}

impl Caller {
    pub(crate) fn user_fallback(&self) -> bool {
        self.policy.user_fallback
    }

    /// The token this caller sends with, by its policy. `fetch` is false
    /// where nothing may be sent, as in a dry run, and only a token already
    /// on disk is returned then.
    pub(crate) fn token(&'static self, fetch: bool) -> Result<CliToken, Unavailable> {
        let override_token = std::env::var("MAPBOX_CLI_TOKEN").ok();
        let (token, source) = chosen(self.policy, override_token.as_deref(), || {
            if fetch {
                fetch::token(self.name, self.policy.paced)
            } else {
                fetch::saved_token()
                    .map(|token| (token, TokenSource::Saved))
                    .ok_or(Unavailable::NotFetched)
            }
        })?;
        Ok(CliToken {
            token,
            source,
            caller: self,
        })
    }
}

/// The CLI's token as a caller got it, for [`send_with`], which handles a
/// refusal of it by where it came from.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CliToken {
    pub(crate) token: String,
    source: TokenSource,
    caller: &'static Caller,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenSource {
    /// `MAPBOX_CLI_TOKEN`.
    Override,
    /// The one on disk.
    Saved,
    /// Fetched by this run.
    Fetched,
}

/// Prefers `MAPBOX_CLI_TOKEN`, and loads the fetched token only when it is
/// unset, so an override never causes a fetch.
fn chosen(
    policy: Policy,
    override_token: Option<&str>,
    fetched: impl FnOnce() -> Result<(String, TokenSource), Unavailable>,
) -> Result<(String, TokenSource), Unavailable> {
    match overridden(policy, override_token) {
        // Don't fall back to the fetched token: whoever set the override
        // wanted a different one.
        Some(token) => token.map(|token| (token, TokenSource::Override)),
        None => fetched(),
    }
}

/// `MAPBOX_CLI_TOKEN` trimmed, or why `policy` may not send it. `None` when
/// it is unset.
fn overridden(policy: Policy, value: Option<&str>) -> Option<Result<String, Unavailable>> {
    let token = value.map(str::trim).filter(|t| !t.is_empty())?;
    // A caller that falls back to the user's token sends any override: it
    // would send the user's token anyway.
    Some(if policy.user_fallback || fetch::is_plausible(token) {
        Ok(token.to_owned())
    } else {
        Err(Unavailable::OverrideNotPublic)
    })
}

/// Sends `request` once with the CLI's token as `access_token`, by
/// `caller`'s policy, for a caller with no user token to fall back to.
///
/// Returns `None` when there is no CLI token to use, an error when the
/// request could not be sent, and otherwise the response, even a refusal.
pub(crate) fn send(
    caller: &'static Caller,
    request: impl Fn() -> RequestBuilder,
) -> Option<reqwest::Result<Response>> {
    let given = caller.token(true).ok()?;
    let send = |token: &str| {
        http::send(
            request()
                .query(&[("access_token", token)])
                .header(reqwest::header::REFERER, REFERER),
        )
    };
    match send_with(given, send, |answer| {
        answer.as_ref().ok().map(|r| r.status().as_u16())
    }) {
        Sent::Answered(answer) | Sent::Refused(answer, _) => Some(answer),
    }
}

/// Sends with `given` by [`send_with_cli_token`]'s rules and its caller's
/// policy.
pub(crate) fn send_with<R>(
    given: CliToken,
    send: impl FnMut(&str) -> R,
    status: impl Fn(&R) -> Option<u16>,
) -> Sent<R> {
    let mut ledger = Disk {
        policy: given.caller.policy,
        source: given.source,
    };
    send_with_cli_token(given.caller.name, given.token, &mut ledger, send, status)
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
    /// Whether to retry a refused request with a [`fresh`](Ledger::fresh)
    /// token.
    fn retries(&self) -> bool;
    fn fresh(&mut self) -> Option<String>;
}

/// `<config dir>/cli-token.json`, through [`fetch`], as the caller's
/// `policy` and the token's `source` allow.
struct Disk {
    policy: Policy,
    source: TokenSource,
}

impl Ledger for Disk {
    fn refused(&mut self, caller: &str, token: &str) -> Refusal {
        // A developer chose `MAPBOX_CLI_TOKEN`, so its refusal is theirs to
        // see. It is not checked, counted or replaced.
        if self.source == TokenSource::Override {
            return Refusal::NotFetched;
        }
        fetch::refused(caller, token, self.policy.paced)
    }
    fn accepted(&mut self, caller: &str, token: &str) {
        if self.policy.paced && self.source != TokenSource::Override {
            fetch::accepted(caller, token);
        }
    }
    fn retries(&self) -> bool {
        // A token this run just fetched would only come back again.
        self.policy.resends && self.source == TokenSource::Saved
    }
    fn fresh(&mut self) -> Option<String> {
        // A paced caller's refusal has already fetched again, and fetching
        // once more would get around its count.
        if self.policy.paced {
            fetch::saved_token()
        } else {
            fetch::token_now().ok()
        }
    }
}

/// Sends with the CLI's `token` on `caller`'s behalf: the one place the
/// rules for using it live, for every caller.
///
/// A `2xx` resets `caller`'s failure count. A `401` or `403` is checked with
/// the Tokens API right away ([`fetch::refused`]). If the ledger allows a
/// retry ([`Ledger::retries`]) and a different token is available, the
/// request is sent once more with it. Otherwise the refusal is returned.
/// `status` gives a response's HTTP status, or `None` when there was no
/// response.
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
                if ledger.retries() && !replaced {
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

/// Why [`send`] has no CLI token to try for `caller`, checked without the
/// network so a caller can skip work that could not be sent anyway. `None`
/// when it may have one.
pub(crate) fn no_token_for(caller: &Caller) -> Option<&'static str> {
    match overridden(
        caller.policy,
        std::env::var("MAPBOX_CLI_TOKEN").ok().as_deref(),
    ) {
        Some(token) => token.err().map(|_| "override_not_public"),
        None if caller.policy.paced => fetch::no_token_for(caller.name),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_is_sent_trimmed_and_never_fetches() {
        for policy in [TELEMETRY, COMMAND] {
            let chosen = chosen(policy, Some(" pk.override\n"), || panic!("fetched"));
            assert_eq!(
                chosen,
                Ok(("pk.override".to_owned(), TokenSource::Override))
            );
        }
    }

    #[test]
    fn an_override_that_is_not_public_is_sent_only_with_a_user_fallback() {
        for token in ["sk.secret", "tk.login", "pk.has space"] {
            assert_eq!(
                chosen(TELEMETRY, Some(token), || panic!("fetched")),
                Err(Unavailable::OverrideNotPublic),
                "{token}"
            );
            assert_eq!(
                chosen(COMMAND, Some(token), || panic!("fetched")),
                Ok((token.to_owned(), TokenSource::Override)),
                "{token}"
            );
        }
    }

    #[test]
    fn without_an_override_the_fetched_token_is_sent() {
        for blank in [None, Some(""), Some("  ")] {
            let fetched = || Ok(("pk.fetched".to_owned(), TokenSource::Fetched));
            assert_eq!(chosen(TELEMETRY, blank, fetched), fetched(), "{blank:?}");
            assert_eq!(
                chosen(TELEMETRY, blank, || Err(Unavailable::Unreachable)),
                Err(Unavailable::Unreachable),
                "{blank:?}: nothing to send with"
            );
        }
    }

    fn disk(policy: Policy, source: TokenSource) -> Disk {
        Disk { policy, source }
    }

    #[test]
    fn telemetry_never_sends_again() {
        for source in [
            TokenSource::Override,
            TokenSource::Saved,
            TokenSource::Fetched,
        ] {
            assert!(!disk(TELEMETRY, source).retries(), "{source:?}");
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
        for (caller, reason) in CLI_TOKEN_COMMANDS {
            let name = caller.name;
            assert!(
                commands.iter().any(|c| *c == name),
                "`{name}` is in CLI_TOKEN_COMMANDS but is not a command"
            );
            assert!(!reason.trim().is_empty(), "`{name}` needs a reason");
        }
    }

    #[test]
    fn cli_token_commands_read_nothing_from_stdin() {
        let specs = crate::spec::effective_services().expect("the bundled specs parse");
        for op in specs.iter().flat_map(|svc| &svc.operations) {
            if !may_use(&op.command()) {
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
        assert_eq!(command("styles list"), None);
        assert!(!may_use("styles list"));
    }

    /// Returns `verdict` for every refusal and hands out `fresh` once.
    /// Records every token it is given.
    #[derive(Default)]
    struct FakeLedger {
        retries: bool,
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
        fn retries(&self) -> bool {
            self.retries
        }
        fn fresh(&mut self) -> Option<String> {
            self.fresh.take()
        }
    }

    /// Acts like [`Disk`] for a [`COMMAND`] token loaded from disk.
    fn command_ledger(verdict: Refusal, fresh: &str) -> FakeLedger {
        FakeLedger {
            retries: true,
            verdict: Some(verdict),
            fresh: Some(fresh.to_owned()),
            ..FakeLedger::default()
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
    fn a_command_tries_a_fresh_token_after_any_refusal() {
        for verdict in [
            Refusal::Dead,
            Refusal::Forbidden,
            Refusal::Unknown,
            Refusal::NotFetched,
        ] {
            let mut ledger = command_ledger(verdict, "pk.new");
            let (outcome, sent) = send_cli(&mut ledger, &[Some(403), Some(200)]);
            assert_eq!(outcome, Sent::Answered(Some(200)), "{verdict:?}");
            assert_eq!(sent, ["pk.old", "pk.new"], "{verdict:?}");
            assert_eq!(ledger.accepted, ["pk.new"], "{verdict:?}");
        }
    }

    #[test]
    fn a_commands_replacement_that_is_refused_too_is_not_replaced_again() {
        let mut ledger = command_ledger(Refusal::Dead, "pk.new");
        let (outcome, sent) = send_cli(&mut ledger, &[Some(401), Some(401)]);
        assert_eq!(outcome, Sent::Refused(Some(401), Refusal::Dead));
        assert_eq!(sent, ["pk.old", "pk.new"]);
    }

    #[test]
    fn a_command_gets_the_same_token_back_and_does_not_resend_it() {
        for verdict in [Refusal::Dead, Refusal::Forbidden] {
            let mut ledger = command_ledger(verdict, "pk.old");
            let (outcome, sent) = send_cli(&mut ledger, &[Some(403)]);
            assert_eq!(outcome, Sent::Refused(Some(403), verdict));
            assert_eq!(sent, ["pk.old"], "the same token is not sent again");
        }
    }

    #[test]
    fn in_the_background_any_refusal_is_handed_back_without_a_resend() {
        for verdict in [Refusal::Dead, Refusal::Forbidden, Refusal::Unknown] {
            let mut ledger = FakeLedger {
                verdict: Some(verdict),
                fresh: Some("pk.new".to_owned()),
                ..FakeLedger::default()
            };
            let (outcome, sent) = send_cli(&mut ledger, &[Some(403)]);
            assert_eq!(outcome, Sent::Refused(Some(403), verdict));
            assert_eq!(sent, ["pk.old"]);
            assert_eq!(ledger.refused, ["pk.old"], "{verdict:?} not checked");
            assert!(ledger.fresh.is_some(), "{verdict:?} asked to resend");
        }
    }

    #[test]
    fn a_command_token_just_fetched_or_overridden_is_not_replaced() {
        for (source, retries) in [
            (TokenSource::Saved, true),
            (TokenSource::Fetched, false),
            (TokenSource::Override, false),
        ] {
            assert_eq!(disk(COMMAND, source).retries(), retries, "{source:?}");
        }
    }

    #[test]
    fn an_override_is_never_checked_or_replaced() {
        for policy in [TELEMETRY, COMMAND] {
            let mut ledger = disk(policy, TokenSource::Override);
            assert_eq!(ledger.refused("report", "pk.override"), Refusal::NotFetched);
            assert!(!ledger.retries());
        }
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
