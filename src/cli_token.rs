//! Requests the CLI makes for itself rather than for the user, and the token
//! they carry.
//!
//! The user's own token always comes first. The CLI's token exists only as
//! the fallback for someone who has none, so it is never sent while the user
//! has one that works. Tried highest first:
//!
//! 1. `--token` or `MAPBOX_ACCESS_TOKEN`, ranked the way a command ranks them
//!    (see [`user_token`]).
//! 2. The account's default public token, kept with the stored login. When
//!    there is none yet, or the API rejects it because the user rotated or
//!    deleted it, a new one is fetched with the login's OAuth token and saved.
//!    Preferred over the OAuth token itself, which carries write scopes and
//!    expires.
//! 3. The login's OAuth token, when the default public token could not be had.
//!    It is only read, never refreshed: a refresh takes the credentials lock
//!    and spends a single-use refresh token, and a request nobody is waiting
//!    for must not be the reason the next command's login is gone. An expired
//!    login skips this step and the one above.
//! 4. `MAPBOX_CLI_TOKEN` at run time, the CLI's token for someone with none
//!    of their own, such as a developer pointing at staging.
//! 5. The token compiled in from `MAPBOX_CLI_BUNDLED_TOKEN` at build time. A
//!    plain `cargo build` has none. It is a different variable from the
//!    run-time one so that a token exported in a dev shell is not baked into
//!    every local build.
//!
//! A `401` moves on to the next token; any other answer is the caller's.
//! Callers never see the token, so every one of them gets the same fallback
//! and replacement without doing anything.
//!
//! # The bundled token is public
//!
//! Anything compiled into a distributed binary can be pulled out with
//! `strings`. The bundled token must therefore be a dedicated `pk.` token with
//! the minimum scopes, and nothing may ever treat it as a secret. `build.rs`
//! refuses anything that is not `pk.`.

use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::StatusCode;

use crate::auth::{self, Credentials};
use crate::http;

/// Seconds of remaining life below which a stored OAuth token is not used.
const EXPIRY_MARGIN_SECS: u64 = 60;

/// Lists an account's tokens; `?default=true` narrows it to the default one.
const TOKENS_ENDPOINT: &str = "https://api.mapbox.com/tokens/v2";

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
    // Read-only: a background request must not be what creates or
    // re-permissions the config directory on a machine that never logged in.
    let creds = auth::load_credentials_readonly(profile);
    let override_token = std::env::var("MAPBOX_CLI_TOKEN").ok();
    let sources = Sources {
        user: user_token,
        stored: creds
            .as_ref()
            .and_then(|c| c.default_public_token.as_deref()),
        login: creds
            .as_ref()
            .and_then(|c| Login::from_credentials(c, now_secs())),
        override_token: override_token.as_deref(),
        bundled: option_env!("MAPBOX_CLI_BUNDLED_TOKEN"),
    };
    resolve(
        sources,
        |login| {
            let token = fetch_default_public_token(&http::client().ok()?, TOKENS_ENDPOINT, login)?;
            // Used for this request even when it could not be saved; the
            // next request simply fetches it again.
            auth::store_default_public_token(profile, &login.account, &token);
            Some(token)
        },
        |token| http::send(request().query(&[("access_token", token)])).ok(),
        |response| response.status() == StatusCode::UNAUTHORIZED,
    )
}

struct Sources<'a> {
    user: Option<&'a str>,
    stored: Option<&'a str>,
    login: Option<Login>,
    override_token: Option<&'a str>,
    bundled: Option<&'a str>,
}

/// A stored OAuth login that is still good for long enough to use.
#[derive(Debug, PartialEq, Eq)]
struct Login {
    access_token: String,
    account: String,
}

impl Login {
    fn from_credentials(creds: &Credentials, now_secs: u64) -> Option<Self> {
        let access_token = usable(Some(&creds.access_token))?;
        // No `exp` claim means no expiry to honor, as in
        // `auth::token_needs_refresh`.
        let alive = auth::token_expires_at(access_token)
            .is_none_or(|exp| exp > now_secs.saturating_add(EXPIRY_MARGIN_SECS));
        // The account becomes a path segment. Mapbox usernames are plain, so
        // anything else is refused rather than escaped.
        let account = auth::credentials_account(creds).filter(|a| {
            !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })?;
        alive.then(|| Self {
            access_token: access_token.to_owned(),
            account,
        })
    }
}

/// The order and the fallback, with fetching and sending passed in so each
/// path can be tested without the network or the disk.
///
/// The fetch runs only once every token above it is missing or rejected, and
/// a token that was already rejected is not tried again. A failed send stops
/// here: no other token fixes a request that never arrived.
fn resolve<R>(
    sources: Sources<'_>,
    mut fetch: impl FnMut(&Login) -> Option<String>,
    mut attempt: impl FnMut(&str) -> Option<R>,
    rejected: impl Fn(&R) -> bool,
) -> Option<R> {
    enum Stage {
        User,
        Stored,
        Fetched,
        Login,
        Override,
        Bundled,
    }

    let mut tried: Vec<String> = Vec::new();
    let mut last = None;
    for stage in [
        Stage::User,
        Stage::Stored,
        Stage::Fetched,
        Stage::Login,
        Stage::Override,
        Stage::Bundled,
    ] {
        let token = match stage {
            Stage::User => usable(sources.user).map(str::to_owned),
            Stage::Stored => usable(sources.stored).map(str::to_owned),
            Stage::Fetched => sources.login.as_ref().and_then(&mut fetch),
            Stage::Login => sources.login.as_ref().map(|l| l.access_token.clone()),
            Stage::Override => usable(sources.override_token).map(str::to_owned),
            Stage::Bundled => usable(sources.bundled).map(str::to_owned),
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

#[derive(serde::Deserialize)]
struct TokenEntry {
    token: String,
    #[serde(default)]
    default: bool,
    #[serde(default)]
    usage: String,
}

/// The account's default public token, or `None` on any failure: the caller
/// falls through to the next token rather than reporting anything.
fn fetch_default_public_token(client: &Client, endpoint: &str, login: &Login) -> Option<String> {
    let response = http::send(client.get(format!("{endpoint}/{}", login.account)).query(&[
        ("access_token", login.access_token.as_str()),
        ("default", "true"),
    ]))
    .ok()?;
    if !response.status().is_success() {
        return None;
    }
    // Checked rather than trusted: whatever comes back here is sent on every
    // background request and saved next to the login.
    response
        .json::<Vec<TokenEntry>>()
        .ok()?
        .into_iter()
        .find(|t| t.default && t.usage == "pk" && t.token.starts_with("pk."))
        .map(|t| t.token)
}

fn usable(token: Option<&str>) -> Option<&str> {
    token.map(str::trim).filter(|t| !t.is_empty())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    const NOW: u64 = 1_000_000;

    fn oauth_token(claims: &str) -> String {
        format!("sk.{}.signature", URL_SAFE_NO_PAD.encode(claims))
    }

    fn creds(access_token: String, username: Option<&str>) -> Credentials {
        Credentials {
            access_token,
            refresh_token: None,
            username: username.map(str::to_owned),
            client_id: None,
            default_public_token: None,
        }
    }

    fn login() -> Login {
        Login {
            access_token: "sk.oauth".into(),
            account: "someone".into(),
        }
    }

    /// Runs [`resolve`] against a fake API that accepts only `accepted`, and
    /// returns the answer, every token sent, and how many fetches ran.
    fn run(
        sources: Sources<'_>,
        fetched: Option<&str>,
        accepted: &[&str],
    ) -> (Option<u16>, Vec<String>, usize) {
        let sent = RefCell::new(Vec::new());
        let mut fetches = 0;
        let answer = resolve(
            sources,
            |_| {
                fetches += 1;
                fetched.map(str::to_owned)
            },
            |token| {
                sent.borrow_mut().push(token.to_owned());
                Some(if accepted.contains(&token) { 200 } else { 401 })
            },
            |status| *status == 401,
        );
        (answer, sent.into_inner(), fetches)
    }

    fn sources<'a>(
        stored: Option<&'a str>,
        login: Option<Login>,
        override_token: Option<&'a str>,
        bundled: Option<&'a str>,
    ) -> Sources<'a> {
        Sources {
            user: None,
            stored,
            login,
            override_token,
            bundled,
        }
    }

    #[test]
    fn an_accepted_stored_token_is_all_that_is_sent() {
        let (answer, sent, fetches) = run(
            sources(
                Some("pk.stored"),
                Some(login()),
                Some("pk.override"),
                Some("pk.bundled"),
            ),
            Some("pk.fresh"),
            &["pk.stored"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.stored"]);
        assert_eq!(fetches, 0);
    }

    #[test]
    fn a_rejected_stored_token_is_replaced_by_a_fetched_one() {
        let (answer, sent, fetches) = run(
            sources(Some("pk.revoked"), Some(login()), Some("pk.override"), None),
            Some("pk.fresh"),
            &["pk.fresh"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.revoked", "pk.fresh"]);
        assert_eq!(fetches, 1);
    }

    #[test]
    fn a_missing_stored_token_is_fetched() {
        let (answer, sent, _) = run(
            sources(None, Some(login()), None, None),
            Some("pk.fresh"),
            &["pk.fresh"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.fresh"]);
    }

    #[test]
    fn without_a_usable_login_nothing_is_fetched() {
        let (answer, sent, fetches) = run(
            sources(
                Some("pk.revoked"),
                None,
                Some("pk.override"),
                Some("pk.bundled"),
            ),
            Some("pk.fresh"),
            &["pk.override"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.revoked", "pk.override"]);
        assert_eq!(fetches, 0);
    }

    #[test]
    fn a_failed_fetch_falls_back_to_the_oauth_token() {
        let (answer, sent, _) = run(
            sources(Some("pk.revoked"), Some(login()), Some("pk.override"), None),
            None,
            &["sk.oauth"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.revoked", "sk.oauth"]);
    }

    #[test]
    fn the_clis_token_is_sent_only_once_every_user_token_is_rejected() {
        let (answer, sent, _) = run(
            sources(Some("pk.revoked"), Some(login()), Some("pk.override"), None),
            None,
            &["pk.override"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.revoked", "sk.oauth", "pk.override"]);
    }

    #[test]
    fn a_users_own_token_comes_first() {
        let (answer, sent, fetches) = run(
            Sources {
                user: Some("pk.typed"),
                ..sources(
                    Some("pk.stored"),
                    Some(login()),
                    Some("pk.override"),
                    Some("pk.bundled"),
                )
            },
            Some("pk.fresh"),
            &["pk.typed", "pk.stored"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.typed"]);
        assert_eq!(fetches, 0);
    }

    #[test]
    fn a_rejected_user_token_falls_through_to_the_login() {
        let (answer, sent, _) = run(
            Sources {
                user: Some("pk.typed"),
                ..sources(Some("pk.stored"), Some(login()), None, None)
            },
            None,
            &["pk.stored"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.typed", "pk.stored"]);
    }

    #[test]
    fn a_rejected_token_is_not_sent_twice() {
        // The fetch hands back the token that was just rejected, as it would
        // if the API refuses the default token itself rather than a stale one.
        let (answer, sent, _) = run(
            sources(Some("pk.same"), Some(login()), None, Some("pk.bundled")),
            Some("pk.same"),
            &["pk.bundled"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.same", "sk.oauth", "pk.bundled"]);
    }

    #[test]
    fn override_comes_before_bundled() {
        let (answer, sent, _) = run(
            sources(None, None, Some("pk.override"), Some("pk.bundled")),
            None,
            &["pk.override", "pk.bundled"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.override"]);
    }

    #[test]
    fn every_token_rejected_returns_the_last_answer() {
        let (answer, sent, _) = run(
            sources(
                Some("pk.stored"),
                None,
                Some("pk.override"),
                Some("pk.bundled"),
            ),
            None,
            &[],
        );
        assert_eq!(answer, Some(401));
        assert_eq!(sent, ["pk.stored", "pk.override", "pk.bundled"]);
    }

    #[test]
    fn a_failed_send_stops_without_trying_other_tokens() {
        let mut sent = Vec::new();
        let answer: Option<u16> = resolve(
            sources(
                Some("pk.stored"),
                None,
                Some("pk.override"),
                Some("pk.bundled"),
            ),
            |_| None,
            |token| {
                sent.push(token.to_owned());
                None
            },
            |status| *status == 401,
        );
        assert_eq!(answer, None);
        assert_eq!(sent, ["pk.stored"]);
    }

    #[test]
    fn blank_tokens_are_skipped_and_the_rest_trimmed() {
        let (answer, sent, _) = run(
            sources(Some("  "), None, Some(""), Some(" pk.bundled\n")),
            None,
            &["pk.bundled"],
        );
        assert_eq!(answer, Some(200));
        assert_eq!(sent, ["pk.bundled"]);
    }

    #[test]
    fn nothing_to_send_with_sends_nothing() {
        let (answer, sent, _) = run(sources(None, None, None, None), None, &[]);
        assert_eq!(answer, None);
        assert!(sent.is_empty());
    }

    #[test]
    fn a_login_is_usable_until_the_margin() {
        for (exp, usable) in [(NOW - 1, false), (NOW + 60, false), (NOW + 61, true)] {
            let c = creds(
                oauth_token(&format!(r#"{{"u":"someone","exp":{exp}}}"#)),
                None,
            );
            assert_eq!(
                Login::from_credentials(&c, NOW).is_some(),
                usable,
                "exp = {exp}"
            );
        }
    }

    #[test]
    fn a_login_without_exp_is_usable() {
        let c = creds(oauth_token(r#"{"u":"someone"}"#), None);
        assert_eq!(
            Login::from_credentials(&c, NOW),
            Some(Login {
                access_token: c.access_token.clone(),
                account: "someone".into(),
            })
        );
    }

    #[test]
    fn the_saved_username_wins_over_the_token_claim() {
        let c = creds(oauth_token(r#"{"u":"from-token"}"#), Some("saved"));
        assert_eq!(Login::from_credentials(&c, NOW).unwrap().account, "saved");
    }

    #[test]
    fn an_account_that_is_not_a_plain_username_is_refused() {
        for account in ["a/b", "..", "a?b", "a b", ""] {
            let c = creds(oauth_token(r#"{"u":"someone"}"#), Some(account));
            assert_eq!(Login::from_credentials(&c, NOW), None, "{account:?}");
        }
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

    /// A loopback server answering one request; returns the request head it
    /// received and the address to send to.
    fn serve_once(status_line: &str, body: &str) -> (std::thread::JoinHandle<String>, String) {
        let response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("the client's connection");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let _ = stream.write_all(response.as_bytes());
            String::from_utf8_lossy(&head).into_owned()
        });
        (server, format!("http://{addr}/tokens/v2"))
    }

    fn fetch_from(status_line: &str, body: &str) -> (Option<String>, String) {
        let (server, endpoint) = serve_once(status_line, body);
        let token = fetch_default_public_token(&http::client().unwrap(), &endpoint, &login());
        (token, server.join().unwrap())
    }

    #[test]
    fn fetch_asks_for_the_accounts_default_token() {
        let (token, head) = fetch_from(
            "200 OK",
            r#"[{"token":"pk.default","default":true,"usage":"pk","scopes":["styles:read"]}]"#,
        );
        assert_eq!(token.as_deref(), Some("pk.default"));
        let request_line = head.lines().next().unwrap();
        assert!(
            request_line.starts_with("GET /tokens/v2/someone?"),
            "{request_line}"
        );
        assert!(
            request_line.contains("access_token=sk.oauth"),
            "{request_line}"
        );
        assert!(request_line.contains("default=true"), "{request_line}");
    }

    #[test]
    fn fetch_refuses_anything_but_a_default_public_token() {
        for body in [
            r#"[{"token":"sk.secret","default":true,"usage":"sk"}]"#,
            r#"[{"token":"pk.other","default":false,"usage":"pk"}]"#,
            r#"[{"token":"xx.odd","default":true,"usage":"pk"}]"#,
            r#"[]"#,
            r#"{"message":"not a list"}"#,
        ] {
            assert_eq!(fetch_from("200 OK", body).0, None, "{body}");
        }
    }

    #[test]
    fn fetch_gives_up_on_an_error_status() {
        let (token, _) = fetch_from(
            "401 Unauthorized",
            r#"[{"token":"pk.default","default":true,"usage":"pk"}]"#,
        );
        assert_eq!(token, None);
    }
}
