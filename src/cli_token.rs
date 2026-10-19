//! Requests the CLI makes for itself rather than for the user, and the token
//! they carry.
//!
//! The user's own token always comes first. The CLI's token exists only as
//! the fallback for someone who has none, so it is never sent while the user
//! has one that works. Tried highest first:
//!
//! 1. `--token` or `MAPBOX_ACCESS_TOKEN`, ranked the way a command ranks them
//!    (see [`user_token`]).
//! 2. The stored login, read as it is on disk and refreshed, under the
//!    credentials lock, only when it is about to expire.
//! 3. `MAPBOX_CLI_TOKEN` at run time, the CLI's token for someone with none
//!    of their own, such as a developer pointing at staging.
//! 4. The token published on the release channel, fetched and kept on disk
//!    until the Tokens API calls it dead. See [`fetch`] for when it is
//!    fetched again.
//!
//! A `401` moves on to the next token; any other answer is the caller's.
//! [`send`]'s callers never see the token, so every one of them gets the same
//! fallback without doing anything.
//!
//! # What may use the CLI's token
//!
//! Steps 3 and 4 are the CLI's token, and every request made with it is
//! billed to the account that owns it. So it is decided by what the request
//! is for, not where it goes: a request the CLI makes for itself passes
//! [`send`] an order that ends in [`Fallback::CliToken`], and every such order
//! is defined in this file (today only [`TELEMETRY`]); a command must be in
//! [`CLI_TOKEN_COMMANDS`], which also says how it uses it ([`Use`]). Where
//! the request is sent does not matter, so a URL overridden from the
//! environment keeps the same fallback.
//!
//! Either way the token is sent through [`send_with_cli_token`], which holds
//! the rules: check a refusal at once, replace a dead token, rest from an API
//! that refuses a valid one. A command that cannot use the CLI's token falls
//! back to the user's, and with none says why: unavailable right now
//! ([`Unavailable`]) or refused by this API ([`Refusal`]).
//!
//! # The token is public
//!
//! It is served from a public URL, so it must be a dedicated `pk.` token with
//! the minimum scopes, and nothing may ever treat it as a secret. [`fetch`]
//! refuses anything that is not `pk.`.

use std::cell::RefCell;

use reqwest::blocking::{RequestBuilder, Response};
use reqwest::StatusCode;

use crate::{auth, http};

mod fetch;

pub(crate) use fetch::{Refusal, Unavailable};

/// The `Referer` every request with the CLI's token carries. The published
/// token is restricted to this URL, so that a web page elsewhere cannot use
/// it; without the header, the API answers `401`. It keeps nothing out of a
/// program that sets the header itself, which is all the restriction can do
/// for a token anyone can read.
pub(crate) const REFERER: &str = "https://cli.mapbox.com/";

/// One place [`send`] may take a token from. A caller passes them in the
/// order to try.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fallback {
    /// `--token` or `MAPBOX_ACCESS_TOKEN`, as [`user_token`] ranks them.
    UserToken,
    /// The stored login, refreshed only when it is about to expire.
    Login,
    /// `MAPBOX_CLI_TOKEN` at run time, then the fetched token. Billed to the
    /// CLI's account, so it only ever comes after every token of the user's.
    CliToken,
}

/// Telemetry's name as a caller of the CLI's token, for its cooldown.
pub(crate) const TELEMETRY_CALLER: &str = "telemetry";

/// Telemetry's order. The CLI's own usage data, so there is no user's
/// account to bill when the user has no token.
pub(crate) const TELEMETRY: &[Fallback] =
    &[Fallback::UserToken, Fallback::Login, Fallback::CliToken];

/// Every order defined here, for the test that holds the CLI's token last.
/// An order that uses it belongs in this file and in this list.
#[cfg(test)]
const ORDERS: &[&[Fallback]] = &[TELEMETRY];

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

fn use_for(command: &str) -> Option<Use> {
    CLI_TOKEN_COMMANDS
        .iter()
        .find(|(name, _, _)| *name == command)
        .map(|(_, how, _)| *how)
}

/// The CLI's token for `command`, or `None` when the command is not listed
/// for it: the only way to ask, so an unlisted command never gets one.
/// `fetch` is false where nothing may be sent, as in a dry run, and only a
/// token already on disk is returned then.
pub(crate) fn for_command(command: &str, fetch: bool) -> Option<Result<String, Unavailable>> {
    match use_for(command)? {
        Use::Preferred => Some(preferred_token(command, fetch)),
    }
}

fn preferred_token(command: &str, fetch: bool) -> Result<String, Unavailable> {
    // A developer's override is not the fetched token, whose cooldown does
    // not apply to it.
    if let Some(token) = usable(std::env::var("MAPBOX_CLI_TOKEN").ok().as_deref()) {
        return Ok(token.to_owned());
    }
    if let Some(until) = fetch::resting_until(command) {
        return Err(Unavailable::Resting { until });
    }
    if fetch {
        fetch::token()
    } else {
        fetch::saved_token().ok_or(Unavailable::NotFetched)
    }
}

/// The token the user gave this run, ranked as a command ranks it: with
/// `--use-login` only a typed `--token` counts, otherwise the environment
/// does too, `MapboxAccessToken` included, since that is the user's token
/// for `tilesets-cli`. Resolved by the caller, which has the arguments, and
/// handed to [`send`].
pub(crate) fn user_token(matches: &clap::ArgMatches) -> Option<String> {
    if matches.get_flag("use-login") {
        auth::typed_token(matches)
    } else {
        matches
            .get_one::<String>("token")
            .cloned()
            .or_else(|| auth::environment_token().map(|(_, token)| token))
    }
}

/// Sends `request` with a token attached as `access_token`, trying the
/// next token whenever the API answers `401`.
///
/// May block on the credentials lock and refresh the login over the network,
/// as a command does, so call it where nobody is waiting on the answer, such
/// as a detached child.
///
/// `request` is called once per attempt, since a sent request cannot be
/// reused. `None` when there is no token to send with; an error when the
/// request could not be sent at all; otherwise the first answer that was not
/// a `401`, or the last `401` when every token was rejected.
pub(crate) fn send(
    caller: &str,
    order: &[Fallback],
    user_token: Option<&str>,
    profile: Option<&str>,
    request: impl Fn() -> RequestBuilder,
) -> Option<reqwest::Result<Response>> {
    let override_token = std::env::var("MAPBOX_CLI_TOKEN").ok();
    let fetched = RefCell::new(None);
    let send = |token: &str| http::send(request().query(&[("access_token", token)]));
    // Any CLI token, a replacement for a dead one included.
    let send_cli = |token: &str| {
        http::send(
            request()
                .query(&[("access_token", token)])
                .header(reqwest::header::REFERER, REFERER),
        )
    };
    let attempt = |token: &str| {
        // Only the fetched token goes through the CLI token's rules: the
        // user's tokens are never checked or counted, nor is
        // `MAPBOX_CLI_TOKEN`, which still carries the CLI token's Referer.
        if fetched.borrow().as_deref() != Some(token) {
            return if usable(override_token.as_deref()) == Some(token) {
                send_cli(token)
            } else {
                send(token)
            };
        }
        match send_with_cli_token(caller, token.to_owned(), &mut Disk, send_cli, |answer| {
            answer.as_ref().ok().map(|r| r.status().as_u16())
        }) {
            Sent::Answered(answer) | Sent::Refused(answer, _) => answer,
        }
    };
    let fetch = || {
        // A caller resting from the CLI's token sends nothing with it.
        let token = (!fetch::cooling(caller)).then(|| fetch::token().ok())??;
        *fetched.borrow_mut() = Some(token.clone());
        Some(token)
    };
    resolve(
        order,
        Sources {
            user: user_token,
            override_token: override_token.as_deref(),
        },
        || login_token(profile),
        fetch,
        attempt,
        |response| response.status() == StatusCode::UNAUTHORIZED,
    )
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
    fn fresh(&mut self) -> Option<String>;
}

/// `<config dir>/cli-token.json`, through [`fetch`].
pub(crate) struct Disk;

impl Ledger for Disk {
    fn refused(&mut self, caller: &str, token: &str) -> Refusal {
        fetch::refused(caller, token)
    }
    fn accepted(&mut self, caller: &str, token: &str) {
        fetch::accepted(caller, token);
    }
    fn fresh(&mut self) -> Option<String> {
        fetch::token().ok()
    }
}

/// Sends with the CLI's `token` on `caller`'s behalf: the one place the
/// rules for using it live, for telemetry and commands alike.
///
/// A `2xx` ends `caller`'s cooldown. A `401` or `403` is checked with the
/// Tokens API at once ([`fetch::refused`]): a dead token is replaced and the
/// request sent once more, since a refused request was not acted on; any
/// other refusal starts `caller`'s cooldown and is handed back. `status` is
/// an answer's HTTP status, or `None` when there was no answer.
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
                if refusal == Refusal::Dead && !replaced {
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

/// Whether [`send`] has a token it could try in `order`, judged without the
/// network or the credentials lock, so a caller can skip work that would only
/// find nothing to send with. A login counts by being there: it may still turn
/// out expired and unrefreshable, and then nothing is sent.
pub(crate) fn has_token_for(
    caller: &str,
    order: &[Fallback],
    user_token: Option<&str>,
    profile: Option<&str>,
) -> bool {
    order.iter().any(|fallback| match fallback {
        Fallback::UserToken => usable(user_token).is_some(),
        Fallback::Login => auth::load_credentials_readonly(profile).is_some(),
        Fallback::CliToken => {
            usable(std::env::var("MAPBOX_CLI_TOKEN").ok().as_deref()).is_some()
                || (!fetch::cooling(caller) && fetch::may_have_token())
        }
    })
}

/// The stored login's token, refreshed when it is about to expire.
fn login_token(profile: Option<&str>) -> Option<String> {
    // Read without the lock first, and used as it is while it is good:
    // `load_fresh_credentials` takes the credentials lock, which creates and
    // hardens `~/.mapbox`, and a read-only command's telemetry must not be
    // what changes it. Only a login about to expire pays for the lock.
    let stored = auth::load_credentials_readonly(profile)?;
    if !auth::token_needs_refresh(&stored.access_token) {
        return Some(stored.access_token);
    }
    auth::load_fresh_credentials(false, profile).map(|c| c.access_token)
}

struct Sources<'a> {
    user: Option<&'a str>,
    override_token: Option<&'a str>,
}

enum Source {
    User,
    Login,
    Override,
    Fetched,
}

/// The order and the fallback, with the login, the fetched token and the
/// send passed in so each path can be tested without the network or the
/// disk.
///
/// Tries each [`Fallback`] in `order`. The login and the fetched token are
/// loaded only when their turn comes, and a token that was already rejected
/// is not tried again. A failed send stops here: no other token fixes a
/// request that never arrived.
fn resolve<R, E>(
    order: &[Fallback],
    sources: Sources<'_>,
    login: impl FnOnce() -> Option<String>,
    fetched: impl FnOnce() -> Option<String>,
    mut attempt: impl FnMut(&str) -> Result<R, E>,
    rejected: impl Fn(&R) -> bool,
) -> Option<Result<R, E>> {
    let mut login = Some(login);
    let mut fetched = Some(fetched);
    let mut tried: Vec<String> = Vec::new();
    let mut last = None;
    for fallback in order {
        // One source per step, each loaded only when the one before it was
        // rejected, so a developer's override never costs a fetch.
        let steps: &[Source] = match fallback {
            Fallback::UserToken => &[Source::User],
            Fallback::Login => &[Source::Login],
            Fallback::CliToken => &[Source::Override, Source::Fetched],
        };
        for source in steps {
            let token = match source {
                Source::User => usable(sources.user).map(str::to_owned),
                Source::Login => login.take().and_then(|load| load()),
                Source::Override => usable(sources.override_token).map(str::to_owned),
                Source::Fetched => fetched.take().and_then(|load| load()),
            };
            let Some(token) = token else {
                continue;
            };
            if tried.contains(&token) {
                continue;
            }
            let response = match attempt(&token) {
                Ok(response) => response,
                Err(error) => return Some(Err(error)),
            };
            if !rejected(&response) {
                return Some(Ok(response));
            }
            tried.push(token);
            last = Some(response);
        }
    }
    last.map(Ok)
}

fn usable(token: Option<&str>) -> Option<&str> {
    token.map(str::trim).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// Runs [`resolve`] against a fake API that accepts only `accepted`, and
    /// returns the answer, every token sent, and whether the login was loaded.
    fn run(
        sources: (Sources<'_>, Option<&str>),
        login: Option<&str>,
        accepted: &[&str],
    ) -> (Option<u16>, Vec<String>, bool) {
        run_in(TELEMETRY, sources, login, accepted)
    }

    fn run_in(
        order: &[Fallback],
        (sources, fetched): (Sources<'_>, Option<&str>),
        login: Option<&str>,
        accepted: &[&str],
    ) -> (Option<u16>, Vec<String>, bool) {
        let sent = RefCell::new(Vec::new());
        let loaded = Cell::new(false);
        let answer = resolve(
            order,
            sources,
            || {
                loaded.set(true);
                login.map(str::to_owned)
            },
            || fetched.map(str::to_owned),
            |token| {
                sent.borrow_mut().push(token.to_owned());
                Ok::<_, ()>(if accepted.contains(&token) { 200 } else { 401 })
            },
            |status| *status == 401,
        );
        (answer.map(|a| a.unwrap()), sent.into_inner(), loaded.get())
    }

    /// The user's token, `MAPBOX_CLI_TOKEN`, and what a fetch would return.
    fn sources<'a>(
        user: Option<&'a str>,
        override_token: Option<&'a str>,
        fetched: Option<&'a str>,
    ) -> (Sources<'a>, Option<&'a str>) {
        (
            Sources {
                user,
                override_token,
            },
            fetched,
        )
    }

    #[test]
    fn an_accepted_user_token_is_all_that_is_sent() {
        let (answer, sent, loaded) = run(
            sources(Some("pk.typed"), Some("pk.override"), Some("pk.fetched")),
            Some("sk.login"),
            &["pk.typed"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.typed"]);
        assert!(
            !loaded,
            "the login is not touched when the user's token works"
        );
    }

    #[test]
    fn the_login_comes_after_the_users_token() {
        let (answer, sent, _) = run(
            sources(Some("pk.typed"), Some("pk.override"), None),
            Some("sk.login"),
            &["sk.login"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.typed", "sk.login"]);
    }

    #[test]
    fn the_login_is_used_when_the_user_typed_nothing() {
        let (answer, sent, _) = run(sources(None, None, None), Some("sk.login"), &["sk.login"]);
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["sk.login"]);
    }

    #[test]
    fn the_clis_token_is_sent_only_once_every_user_token_is_rejected() {
        let (answer, sent, _) = run(
            sources(Some("pk.typed"), Some("pk.override"), Some("pk.fetched")),
            Some("sk.login"),
            &["pk.override"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.typed", "sk.login", "pk.override"]);
    }

    #[test]
    fn the_clis_token_is_never_sent_by_an_order_without_it() {
        let (answer, sent, _) = run_in(
            &[Fallback::UserToken, Fallback::Login],
            sources(Some("pk.typed"), Some("pk.override"), Some("pk.fetched")),
            None,
            &["pk.override", "pk.fetched"],
        );
        assert_eq!(answer, Some(401));
        assert_eq!(sent, ["pk.typed"]);
    }

    #[test]
    fn the_order_given_is_the_order_tried() {
        let (_, sent, _) = run_in(
            &[Fallback::Login, Fallback::UserToken],
            sources(Some("pk.typed"), None, None),
            Some("sk.login"),
            &[],
        );
        assert_eq!(sent, ["sk.login", "pk.typed"]);
    }

    #[test]
    fn every_order_keeps_the_clis_token_last() {
        for order in ORDERS {
            if let Some(at) = order.iter().position(|f| *f == Fallback::CliToken) {
                assert_eq!(at, order.len() - 1, "{order:?}");
            }
        }
    }

    #[test]
    fn override_comes_before_fetched() {
        let (answer, sent, _) = run(
            sources(None, Some("pk.override"), Some("pk.fetched")),
            None,
            &["pk.override", "pk.fetched"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.override"]);
    }

    #[test]
    fn a_rejected_token_is_not_sent_twice() {
        // The login and the typed token can be the same token.
        let (answer, sent, _) = run(
            sources(Some("sk.same"), None, Some("pk.fetched")),
            Some("sk.same"),
            &["pk.fetched"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["sk.same", "pk.fetched"]);
    }

    #[test]
    fn every_token_rejected_returns_the_last_answer() {
        let (answer, sent, _) = run(
            sources(Some("pk.typed"), Some("pk.override"), Some("pk.fetched")),
            Some("sk.login"),
            &[],
        );
        assert_eq!(answer, Some(401));
        assert_eq!(sent, ["pk.typed", "sk.login", "pk.override", "pk.fetched"]);
    }

    #[test]
    fn a_failed_send_stops_without_trying_other_tokens() {
        let mut sent = Vec::new();
        let answer = resolve(
            TELEMETRY,
            sources(Some("pk.typed"), Some("pk.override"), None).0,
            || Some("sk.login".to_owned()),
            || Some("pk.fetched".to_owned()),
            |token| {
                sent.push(token.to_owned());
                Err::<u16, _>("unreachable")
            },
            |status| *status == 401,
        );
        assert_eq!(answer, Some(Err("unreachable")));
        assert_eq!(sent, ["pk.typed"]);
    }

    #[test]
    fn blank_tokens_are_skipped_and_the_rest_trimmed() {
        let (answer, sent, _) = run(
            sources(Some("  "), Some(""), Some("pk.fetched")),
            None,
            &["pk.fetched"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.fetched"]);
    }

    #[test]
    fn nothing_to_send_with_sends_nothing() {
        let (answer, sent, _) = run(sources(None, None, None), None, &[]);
        assert_eq!(answer, None);
        assert!(sent.is_empty());
    }

    fn user_token_for(args: &[&str]) -> Option<String> {
        let matches = crate::build_app(&[])
            .try_get_matches_from(args)
            .expect("arguments parse");
        user_token(&matches)
    }

    // The `MAPBOX_ACCESS_TOKEN` half is not covered here: setting it would
    // race every other test in this process that reads the environment.
    #[test]
    fn a_typed_token_is_the_users_with_or_without_use_login() {
        for args in [
            &["mapbox", "--token", "pk.typed", "auth", "whoami"][..],
            &[
                "mapbox",
                "--use-login",
                "--token",
                "pk.typed",
                "auth",
                "whoami",
            ][..],
        ] {
            assert_eq!(
                user_token_for(args).as_deref(),
                Some("pk.typed"),
                "{args:?}"
            );
        }
    }

    #[test]
    fn use_login_without_a_typed_token_leaves_the_user_none() {
        assert_eq!(
            user_token_for(&["mapbox", "--use-login", "auth", "whoami"]),
            None
        );
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

    #[test]
    fn the_override_is_trimmed_before_it_is_sent() {
        let (answer, sent, _) = run(
            sources(None, Some(" pk.override\n"), None),
            None,
            &["pk.override"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.override"]);
    }

    #[test]
    fn an_accepted_override_never_fetches() {
        let fetched = Cell::new(false);
        let answer = resolve(
            &[Fallback::CliToken],
            sources(None, Some("pk.override"), None).0,
            || None,
            || {
                fetched.set(true);
                Some("pk.fetched".to_owned())
            },
            |_| Ok::<_, ()>(200),
            |status| *status == 401,
        );
        assert_eq!(answer, Some(Ok(200)));
        assert!(!fetched.get());
    }
}
