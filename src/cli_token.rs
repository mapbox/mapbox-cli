//! Requests the CLI makes for itself rather than for the user, and the token
//! they carry.
//!
//! The user's own token always comes first. The CLI's token exists only as
//! the fallback for someone who has none, so it is never sent while the user
//! has one that works. Tried highest first:
//!
//! 1. `--token` or `MAPBOX_ACCESS_TOKEN`, ranked the way a command ranks them
//!    (see [`user_token`]).
//! 2. The stored login, loaded the way a command loads it, refresh included.
//! 3. `MAPBOX_CLI_TOKEN` at run time, the CLI's token for someone with none
//!    of their own, such as a developer pointing at staging.
//! 4. The token compiled in from `MAPBOX_CLI_BUNDLED_TOKEN` at build time. A
//!    plain `cargo build` has none. It is a different variable from the
//!    run-time one so that a token exported in a dev shell is not baked into
//!    every local build.
//!
//! A `401` moves on to the next token; any other answer is the caller's.
//! Callers never see the token, so every one of them gets the same fallback
//! without doing anything.
//!
//! # Where the CLI's token may go
//!
//! Steps 3 and 4 are the CLI's token, and every request made with it is
//! billed to the account that owns it. So it is sent only to the APIs in
//! [`CLI_TOKEN_APIS`], whether the request is the CLI's own (through [`send`])
//! or a command's (through [`for_command`]). Anything else gets the user's
//! token or none.
//!
//! # The bundled token is public
//!
//! Anything compiled into a distributed binary can be pulled out with
//! `strings`. The bundled token must therefore be a dedicated `pk.` token with
//! the minimum scopes, and nothing may ever treat it as a secret. `build.rs`
//! refuses anything that is not `pk.`.

use reqwest::blocking::{RequestBuilder, Response};
use reqwest::StatusCode;

use crate::{auth, http};

/// APIs the CLI's token may be sent to, as host, path prefix and the reason
/// the CLI's account should carry their use. Matched on `https`, the exact
/// host with no port, and whole path segments, so `/events/v2` does not
/// cover `/events/v20`.
const CLI_TOKEN_APIS: &[(&str, &str, &str)] = &[
    (
        "events.mapbox.com",
        "/events/v2",
        "Telemetry; no user to bill.",
    ),
    (
        "api-events-staging.tilestream.net",
        "/events/v2",
        "Telemetry to staging; no user to bill.",
    ),
];

/// The CLI's token for a command's request, when the user has none of their
/// own and the command calls an API in [`CLI_TOKEN_APIS`].
///
/// Only for a user with no token at all: one whose token is rejected sees that
/// rejection rather than having it hidden behind the CLI's token.
pub(crate) fn for_command(op: &crate::spec::Operation) -> Option<String> {
    if allowed(CLI_TOKEN_APIS, &operation_url(op)?) {
        cli_token(std::env::var("MAPBOX_CLI_TOKEN").ok().as_deref())
    } else {
        None
    }
}

/// Where a command sends its request, enough to match against
/// [`CLI_TOKEN_APIS`]: path parameters stay as their `{placeholders}`.
fn operation_url(op: &crate::spec::Operation) -> Option<reqwest::Url> {
    reqwest::Url::parse(&format!("{}{}", op.base_url, op.path_template)).ok()
}

/// `MAPBOX_CLI_TOKEN` at run time, then the bundled token.
fn cli_token(override_token: Option<&str>) -> Option<String> {
    usable(override_token)
        .or_else(|| usable(option_env!("MAPBOX_CLI_BUNDLED_TOKEN")))
        .map(str::to_owned)
}

fn allowed(apis: &[(&str, &str, &str)], url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && apis.iter().any(|(host, prefix, _)| {
            url.host_str() == Some(*host) && {
                let mut segments = url.path().split('/').filter(|s| !s.is_empty());
                prefix
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .all(|want| segments.next() == Some(want))
            }
        })
}

/// The token the user gave this run, ranked as a command ranks it: with
/// `--use-login` only a typed `--token` counts, otherwise `MAPBOX_ACCESS_TOKEN`
/// does too. Resolved by the caller, which has the arguments, and handed to
/// [`send`].
#[allow(dead_code)]
pub(crate) fn user_token(matches: &clap::ArgMatches) -> Option<String> {
    if matches.get_flag("use-login") {
        auth::typed_token(matches)
    } else {
        matches.get_one::<String>("token").cloned()
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
/// reused. `None` when there is no token to send with, or the request could
/// not be sent at all; otherwise the first answer that was not a `401`, or
/// the last `401` when every token was rejected.
// The first caller is telemetry delivery, which lands separately.
#[allow(dead_code)]
pub(crate) fn send(
    user_token: Option<&str>,
    profile: Option<&str>,
    request: impl Fn() -> RequestBuilder,
) -> Option<Response> {
    let override_token = std::env::var("MAPBOX_CLI_TOKEN").ok();
    // Read off a request built only to be looked at; the one sent is built
    // again per attempt.
    let cli_allowed = request()
        .build()
        .is_ok_and(|built| allowed(CLI_TOKEN_APIS, built.url()));
    resolve(
        Sources {
            user: user_token,
            override_token: override_token.as_deref(),
            bundled: option_env!("MAPBOX_CLI_BUNDLED_TOKEN"),
            cli_allowed,
        },
        || login_token(profile),
        |token| http::send(request().query(&[("access_token", token)])).ok(),
        |response| response.status() == StatusCode::UNAUTHORIZED,
    )
}

/// The stored login's token, refreshed when it is about to expire.
fn login_token(profile: Option<&str>) -> Option<String> {
    // Looked for read-only first: `load_fresh_credentials` takes the
    // credentials lock, which creates `~/.mapbox` on a machine that never
    // logged in.
    auth::load_credentials_readonly(profile)?;
    auth::load_fresh_credentials(false, profile).map(|c| c.access_token)
}

struct Sources<'a> {
    user: Option<&'a str>,
    override_token: Option<&'a str>,
    bundled: Option<&'a str>,
    /// Whether the request goes to an API in [`CLI_TOKEN_APIS`]; without it
    /// the two tokens above are never sent.
    cli_allowed: bool,
}

/// The order and the fallback, with the login and the send passed in so each
/// path can be tested without the network or the disk.
///
/// The login is loaded only once the user's own token is missing or
/// rejected, and a token that was already rejected is not tried again. A
/// failed send stops here: no other token fixes a request that never
/// arrived.
fn resolve<R>(
    sources: Sources<'_>,
    login: impl FnOnce() -> Option<String>,
    mut attempt: impl FnMut(&str) -> Option<R>,
    rejected: impl Fn(&R) -> bool,
) -> Option<R> {
    let mut login = Some(login);
    let mut tried: Vec<String> = Vec::new();
    let mut last = None;
    for stage in 0..4 {
        let token = match stage {
            0 => usable(sources.user).map(str::to_owned),
            1 => login.take().and_then(|load| load()),
            2 if sources.cli_allowed => usable(sources.override_token).map(str::to_owned),
            3 if sources.cli_allowed => usable(sources.bundled).map(str::to_owned),
            _ => None,
        };
        let Some(token) = token else { continue };
        if tried.contains(&token) {
            continue;
        }
        let response = attempt(&token)?;
        if !rejected(&response) {
            return Some(response);
        }
        tried.push(token);
        last = Some(response);
    }
    last
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
        sources: Sources<'_>,
        login: Option<&str>,
        accepted: &[&str],
    ) -> (Option<u16>, Vec<String>, bool) {
        let sent = RefCell::new(Vec::new());
        let loaded = Cell::new(false);
        let answer = resolve(
            sources,
            || {
                loaded.set(true);
                login.map(str::to_owned)
            },
            |token| {
                sent.borrow_mut().push(token.to_owned());
                Some(if accepted.contains(&token) { 200 } else { 401 })
            },
            |status| *status == 401,
        );
        (answer, sent.into_inner(), loaded.get())
    }

    fn sources<'a>(
        user: Option<&'a str>,
        override_token: Option<&'a str>,
        bundled: Option<&'a str>,
    ) -> Sources<'a> {
        Sources {
            user,
            override_token,
            bundled,
            cli_allowed: true,
        }
    }

    #[test]
    fn an_accepted_user_token_is_all_that_is_sent() {
        let (answer, sent, loaded) = run(
            sources(Some("pk.typed"), Some("pk.override"), Some("pk.bundled")),
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
            sources(Some("pk.typed"), Some("pk.override"), Some("pk.bundled")),
            Some("sk.login"),
            &["pk.override"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.typed", "sk.login", "pk.override"]);
    }

    #[test]
    fn the_clis_token_is_never_sent_to_an_api_outside_the_list() {
        let (answer, sent, _) = run(
            Sources {
                cli_allowed: false,
                ..sources(Some("pk.typed"), Some("pk.override"), Some("pk.bundled"))
            },
            None,
            &["pk.override", "pk.bundled"],
        );
        assert_eq!(answer, Some(401));
        assert_eq!(sent, ["pk.typed"]);
    }

    #[test]
    fn override_comes_before_bundled() {
        let (answer, sent, _) = run(
            sources(None, Some("pk.override"), Some("pk.bundled")),
            None,
            &["pk.override", "pk.bundled"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.override"]);
    }

    #[test]
    fn a_rejected_token_is_not_sent_twice() {
        // The login and the typed token can be the same token.
        let (answer, sent, _) = run(
            sources(Some("sk.same"), None, Some("pk.bundled")),
            Some("sk.same"),
            &["pk.bundled"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["sk.same", "pk.bundled"]);
    }

    #[test]
    fn every_token_rejected_returns_the_last_answer() {
        let (answer, sent, _) = run(
            sources(Some("pk.typed"), Some("pk.override"), Some("pk.bundled")),
            Some("sk.login"),
            &[],
        );
        assert_eq!(answer, Some(401));
        assert_eq!(sent, ["pk.typed", "sk.login", "pk.override", "pk.bundled"]);
    }

    #[test]
    fn a_failed_send_stops_without_trying_other_tokens() {
        let mut sent = Vec::new();
        let answer: Option<u16> = resolve(
            sources(Some("pk.typed"), Some("pk.override"), Some("pk.bundled")),
            || Some("sk.login".to_owned()),
            |token| {
                sent.push(token.to_owned());
                None
            },
            |status| *status == 401,
        );
        assert_eq!(answer, None);
        assert_eq!(sent, ["pk.typed"]);
    }

    #[test]
    fn blank_tokens_are_skipped_and_the_rest_trimmed() {
        let (answer, sent, _) = run(
            sources(Some("  "), Some(""), Some(" pk.bundled\n")),
            None,
            &["pk.bundled"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.bundled"]);
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

    fn url(text: &str) -> reqwest::Url {
        reqwest::Url::parse(text).unwrap()
    }

    const APIS: &[(&str, &str, &str)] = &[("events.mapbox.com", "/events/v2", "test")];

    #[test]
    fn an_allowed_api_matches_on_whole_segments() {
        for (text, ok) in [
            ("https://events.mapbox.com/events/v2", true),
            ("https://events.mapbox.com/events/v2/", true),
            ("https://events.mapbox.com/events/v2/batch?x=1", true),
            ("https://events.mapbox.com/events/v20", false),
            ("https://events.mapbox.com/events", false),
            ("https://events.mapbox.com/other/events/v2", false),
        ] {
            assert_eq!(allowed(APIS, &url(text)), ok, "{text}");
        }
    }

    #[test]
    fn an_allowed_api_needs_the_exact_host_over_https() {
        for text in [
            "http://events.mapbox.com/events/v2",
            "https://events.mapbox.com:8443/events/v2",
            "https://evil.events.mapbox.com/events/v2",
            "https://events.mapbox.com.evil.example/events/v2",
            "https://api.mapbox.com/events/v2",
        ] {
            assert!(!allowed(APIS, &url(text)), "{text}");
        }
        assert!(!allowed(&[], &url("https://events.mapbox.com/events/v2")));
    }

    #[test]
    fn every_cli_token_api_is_well_formed() {
        for (host, prefix, reason) in CLI_TOKEN_APIS {
            assert_eq!(*host, host.to_ascii_lowercase(), "{host}");
            assert!(prefix.starts_with('/'), "{prefix}");
            assert!(!reason.trim().is_empty(), "{host}{prefix} needs a reason");
            assert!(
                allowed(CLI_TOKEN_APIS, &url(&format!("https://{host}{prefix}"))),
                "{host}{prefix}"
            );
        }
    }

    // A URL that failed to parse would quietly keep every command off the
    // CLI's token, listed or not.
    #[test]
    fn every_commands_url_can_be_matched() {
        let specs = crate::spec::effective_services().expect("the bundled specs parse");
        for op in specs.iter().flat_map(|svc| &svc.operations) {
            let url = operation_url(op).unwrap_or_else(|| panic!("{}", op.command()));
            assert_eq!(url.scheme(), "https", "{}", op.command());
        }
    }

    #[test]
    fn the_runtime_override_comes_before_the_bundled_token() {
        assert_eq!(
            cli_token(Some(" pk.override ")).as_deref(),
            Some("pk.override")
        );
        assert_eq!(
            cli_token(Some("  ")).as_deref(),
            option_env!("MAPBOX_CLI_BUNDLED_TOKEN")
                .map(str::trim)
                .filter(|t| !t.is_empty())
        );
    }
}
