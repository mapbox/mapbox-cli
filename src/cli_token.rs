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
//! is defined in this file (today only [`TELEMETRY`]). No command uses it.
//! Where the request is sent does not matter, so a URL overridden from the
//! environment keeps the same fallback.
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

use fetch::Refusal;

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
    let dead = RefCell::new(None);
    let mut attempt = |token: &str| {
        let response = http::send(request().query(&[("access_token", token)]));
        // Only the fetched token's answers are kept: the user's tokens, and
        // `MAPBOX_CLI_TOKEN`, are never checked or counted.
        if let (Ok(r), true) = (&response, fetched.borrow().as_deref() == Some(token)) {
            if r.status().is_success() {
                fetch::accepted(caller, token);
            } else if is_refusal(r.status()) && fetch::refused(caller, token) == Refusal::Dead {
                *dead.borrow_mut() = Some(token.to_owned());
            }
        }
        response
    };
    let fetch = || {
        // A caller resting from the CLI's token sends nothing with it.
        let token = (!fetch::cooling(caller)).then(fetch::token)??;
        *fetched.borrow_mut() = Some(token.clone());
        Some(token)
    };
    let answer = resolve(
        order,
        Sources {
            user: user_token,
            override_token: override_token.as_deref(),
        },
        || login_token(profile),
        fetch,
        &mut attempt,
        |response| response.status() == StatusCode::UNAUTHORIZED,
    );
    // A dead token is replaced at once, and the request sent once more.
    match dead.take() {
        Some(dead) if matches!(&answer, Some(Ok(r)) if is_refusal(r.status())) => {
            match fetch::token() {
                Some(fresh) if fresh != dead => {
                    *fetched.borrow_mut() = Some(fresh.clone());
                    Some(attempt(&fresh))
                }
                _ => answer,
            }
        }
        _ => answer,
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

/// A `401` or `403`: the answers [`refused`] can explain.
pub(crate) fn is_refusal(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN
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
