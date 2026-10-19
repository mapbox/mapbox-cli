//! Where the CLI's token comes from: fetched from the release channel, and
//! kept on disk until the API rejects it.
//!
//! The token is published at `https://cli.mapbox.com/config/token.json` as
//! `{"token": "pk...."}`, so rotating it needs no release. It is fetched
//! when there is none on disk or the one there was rejected, and saved to
//! `<config dir>/cli-token.json`.
//!
//! # When the endpoint has nothing new
//!
//! A `401` or `403` alone does not say the token is dead: the API may only be
//! refusing it for one endpoint. So [`refused`] asks the Tokens API about
//! the token itself, and only a revoked, invalid, expired or malformed one is
//! replaced. A fetch right after a
//! rotation can still return the revoked token: the edge caches the file for
//! a few minutes, and the secret may not be republished yet. So when a fetch
//! answers with nothing usable, the next one waits an hour, three times,
//! then a day, and the cycle starts again. A fetch that never got an answer
//! does not count: that is this machine being offline, not the endpoint
//! being stale, and it is asked again on the next run.
//!
//! # Where it is fetched from
//!
//! Every build fetches from production, a local `cargo build` included,
//! since the token is a production token whatever the channel. [`URL_ENV`]
//! points a build that is not a production release somewhere else, which is
//! how the tests stay off the network: every end-to-end test sets it
//! (`tests/source_guards.rs` holds that).
//!
//! The state file is written only when the config directory already exists,
//! for the reason `telemetry_sink::dir` gives: a read-only command must not
//! be what creates `~/.mapbox`. Without it, every run that needs the token
//! fetches it.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{auth, http};

const PRODUCTION_URL: &str = "https://cli.mapbox.com/config/token.json";

/// Asked about a refused token's own state; see [`refused`].
const TOKENS_URL_ENV: &str = "MAPBOX_INTERNAL_TOKENS_URL";

/// Overrides [`PRODUCTION_URL`] in a build that is not a production release.
/// A release ignores it, so an injected variable cannot choose the token a
/// release sends.
pub(crate) const URL_ENV: &str = "MAPBOX_INTERNAL_CLI_TOKEN_URL";

const STATE_FILE: &str = "cli-token.json";

/// A small JSON file on a CDN; nobody should wait longer for it.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

const HOUR: u64 = 60 * 60;
const DAY: u64 = 24 * HOUR;
/// Hourly retries after a fetch that found nothing new, before waiting a day.
const HOURLY_RETRIES: u32 = 3;

/// Long enough for any `pk.` token; anything longer is not one.
const MAX_TOKEN_LEN: usize = 512;

/// Why there is no CLI token to use right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// The endpoint could not be reached. Asked again on the next run.
    Unreachable(String),
    /// The endpoint answered without a usable token, so it is not asked
    /// again before `retry_at` (Unix seconds).
    Waiting { retry_at: u64 },
    /// None on disk, and the caller asked not to fetch, as a dry run does.
    NotFetched,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(error) => write!(f, "fetching it failed: {error}"),
            Self::Waiting { retry_at } => write!(
                f,
                "it was rejected and no new one is published yet; trying again in {}",
                wait_text(retry_at.saturating_sub(now()))
            ),
            Self::NotFetched => write!(f, "it has not been fetched yet"),
        }
    }
}

/// What a fetch got back, before [`acquire`] judges it.
#[derive(Debug)]
pub(crate) enum Fetched {
    /// The body's `token`, unchecked.
    Token(String),
    /// An answer with no token in it: a bad status or a bad body.
    Nothing,
    /// No answer at all.
    Failed(String),
}

/// What is kept on disk between runs.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    /// Whether the API answered `401` to `token`.
    #[serde(default)]
    rejected: bool,
    /// Consecutive fetches that found nothing usable.
    #[serde(default)]
    failures: u32,
    /// Unix seconds before which the endpoint is not asked.
    #[serde(default)]
    retry_at: u64,
}

impl State {
    fn usable_token(&self) -> Option<&str> {
        self.token
            .as_deref()
            .filter(|token| !self.rejected && is_plausible(token))
    }

    fn fail(&mut self, now: u64) {
        self.failures += 1;
        let wait = if self.failures.is_multiple_of(HOURLY_RETRIES + 1) {
            DAY
        } else {
            HOUR
        };
        self.retry_at = now + wait;
    }
}

/// The CLI's token: the one on disk while it has not been rejected,
/// otherwise a fresh one from the endpoint, fetched only when `fetch` is set.
pub(crate) fn token(fetch: bool) -> Result<String, Unavailable> {
    let mut state = read_state().unwrap_or_default();
    let before = state.clone();
    let result = acquire(&mut state, now(), fetch, || get(&endpoint()));
    if state != before {
        write_state(&state);
    }
    result
}

/// Whether [`token`] could have one, judged without the network.
pub(crate) fn may_have_token() -> bool {
    let state = read_state().unwrap_or_default();
    state.usable_token().is_some() || !waiting(&state, now())
}

/// Whether `token` is the one kept on disk.
pub(crate) fn is_saved(token: &str) -> bool {
    read_state().is_some_and(|state| state.token.as_deref() == Some(token))
}

/// What a `401` or `403` to the CLI's token meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The token itself is revoked, invalid, expired or malformed. It is
    /// marked, so the next [`token`] fetches a new one.
    Dead,
    /// The token is fine; this API does not accept it.
    Forbidden,
    /// The Tokens API could not say. Nothing is changed.
    Unknown,
}

/// Asks the Tokens API why `token` was refused, and marks it when it is
/// dead. A token other than the saved one is never marked.
pub(crate) fn refused(token: &str) -> Refusal {
    let url = override_outside_production(
        option_env!("MAPBOX_CLI_BUILD_ENV"),
        std::env::var(TOKENS_URL_ENV).ok(),
    )
    .unwrap_or_else(|| auth::VALIDATION_ENDPOINT.to_owned());
    let answer = auth::check_token(&url, token, FETCH_TIMEOUT).ok();
    let refusal = verdict(answer.as_ref().and_then(|a| a.get("code")?.as_str()));
    if refusal == Refusal::Dead {
        mark_dead(token);
    }
    refusal
}

fn verdict(code: Option<&str>) -> Refusal {
    match code {
        Some("TokenValid") => Refusal::Forbidden,
        Some("TokenRevoked" | "TokenInvalid" | "TokenExpired" | "TokenMalformed") => Refusal::Dead,
        _ => Refusal::Unknown,
    }
}

fn mark_dead(token: &str) {
    let Some(mut state) = read_state() else {
        return;
    };
    if state.token.as_deref() == Some(token) && !state.rejected {
        state.rejected = true;
        write_state(&state);
    }
}

/// The decision, with the clock and the network passed in.
fn acquire(
    state: &mut State,
    now: u64,
    fetch: bool,
    get: impl FnOnce() -> Fetched,
) -> Result<String, Unavailable> {
    if let Some(token) = state.usable_token() {
        return Ok(token.to_owned());
    }
    if !fetch {
        return Err(Unavailable::NotFetched);
    }
    if waiting(state, now) {
        return Err(Unavailable::Waiting {
            retry_at: state.retry_at,
        });
    }
    match get() {
        Fetched::Failed(error) => Err(Unavailable::Unreachable(error)),
        Fetched::Token(token)
            if is_plausible(&token)
                && !(state.rejected && state.token.as_deref() == Some(token.as_str())) =>
        {
            *state = State {
                token: Some(token.clone()),
                ..State::default()
            };
            Ok(token)
        }
        // The rejected token again, something that is not a `pk.` token, or
        // no token at all.
        Fetched::Token(_) | Fetched::Nothing => {
            state.fail(now);
            Err(Unavailable::Waiting {
                retry_at: state.retry_at,
            })
        }
    }
}

/// Whether the endpoint is still to be left alone. A `retry_at` further out
/// than the longest wait is a clock that went backwards or an edited file,
/// and is not honored.
fn waiting(state: &State, now: u64) -> bool {
    now < state.retry_at && state.retry_at <= now + DAY
}

/// Whether `value` looks like a public token and nothing else. Checked on
/// what the endpoint returns and on what is read back from disk, since both
/// end up in a request.
fn is_plausible(value: &str) -> bool {
    value.starts_with("pk.")
        && value.len() <= MAX_TOKEN_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn endpoint() -> String {
    override_outside_production(
        option_env!("MAPBOX_CLI_BUILD_ENV"),
        std::env::var(URL_ENV).ok(),
    )
    .unwrap_or_else(|| PRODUCTION_URL.to_owned())
}

/// A URL override, honored only in a build that is not a production
/// release, so an injected variable cannot choose where a release asks.
fn override_outside_production(build_env: Option<&str>, url: Option<String>) -> Option<String> {
    let url = url.filter(|url| !url.trim().is_empty())?;
    (build_env != Some("production")).then(|| url.trim().to_owned())
}

fn get(url: &str) -> Fetched {
    let client = match http::client() {
        Ok(client) => client,
        Err(error) => return Fetched::Failed(error.to_string()),
    };
    let response = match http::send(client.get(url).timeout(FETCH_TIMEOUT)) {
        Ok(response) => response,
        Err(error) => return Fetched::Failed(http::failure(&error)),
    };
    if !response.status().is_success() {
        return Fetched::Nothing;
    }
    response
        .json::<serde_json::Value>()
        .ok()
        .and_then(|body| Some(body.get("token")?.as_str()?.trim().to_owned()))
        .map_or(Fetched::Nothing, Fetched::Token)
}

fn state_path() -> Option<PathBuf> {
    let dir = auth::config_dir_path()?;
    dir.is_dir().then(|| dir.join(STATE_FILE))
}

/// The state on disk, or `None` when there is no readable one. A malformed
/// file costs one fetch, which is not worth failing anything over.
fn read_state() -> Option<State> {
    let text = std::fs::read_to_string(state_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Best-effort: a read-only home directory costs a fetch per run, not an
/// error.
fn write_state(state: &State) {
    let Some(path) = state_path() else {
        return;
    };
    if let Ok(text) = serde_json::to_string(state) {
        let _ = auth::write_private(&path, &text);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn wait_text(seconds: u64) -> String {
    let minutes = seconds.div_ceil(60).max(1);
    match minutes {
        1 => "1 minute".to_owned(),
        m if m < 60 => format!("{m} minutes"),
        m => {
            let hours = m.div_ceil(60);
            if hours == 1 {
                "1 hour".to_owned()
            } else {
                format!("{hours} hours")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;

    fn saved(token: &str, rejected: bool) -> State {
        State {
            token: Some(token.to_owned()),
            rejected,
            ..State::default()
        }
    }

    /// Runs [`acquire`] with an endpoint that answers `answer`, and says
    /// whether it was asked.
    fn run(state: &mut State, now: u64, answer: Fetched) -> (Result<String, Unavailable>, bool) {
        let mut asked = false;
        let result = acquire(state, now, true, || {
            asked = true;
            answer
        });
        (result, asked)
    }

    fn token(value: &str) -> Fetched {
        Fetched::Token(value.to_owned())
    }

    #[test]
    fn a_saved_token_is_used_without_asking() {
        let mut state = saved("pk.saved", false);
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert_eq!(result, Ok("pk.saved".to_owned()));
        assert!(!asked);
    }

    #[test]
    fn no_saved_token_fetches_one_and_keeps_it() {
        let mut state = State::default();
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert_eq!(result, Ok("pk.new".to_owned()));
        assert!(asked);
        assert_eq!(state, saved("pk.new", false));
    }

    #[test]
    fn a_rejected_token_is_replaced_by_a_new_one() {
        let mut state = saved("pk.old", true);
        let (result, _) = run(&mut state, NOW, token("pk.new"));
        assert_eq!(result, Ok("pk.new".to_owned()));
        assert_eq!(state, saved("pk.new", false));
    }

    #[test]
    fn the_rejected_token_again_waits_an_hour_three_times_then_a_day() {
        let mut state = saved("pk.old", true);
        let mut now = NOW;
        let mut waits = Vec::new();
        for _ in 0..8 {
            let (result, asked) = run(&mut state, now, token("pk.old"));
            assert!(asked);
            assert_eq!(
                result,
                Err(Unavailable::Waiting {
                    retry_at: state.retry_at
                })
            );
            waits.push(state.retry_at - now);
            now = state.retry_at;
        }
        assert_eq!(
            waits,
            [HOUR, HOUR, HOUR, DAY, HOUR, HOUR, HOUR, DAY],
            "an hour three times, then a day, and again"
        );
    }

    #[test]
    fn nothing_is_fetched_while_waiting() {
        let mut state = State {
            retry_at: NOW + HOUR,
            ..saved("pk.old", true)
        };
        let (result, asked) = run(&mut state, NOW + HOUR - 1, token("pk.new"));
        assert!(!asked);
        assert_eq!(
            result,
            Err(Unavailable::Waiting {
                retry_at: NOW + HOUR
            })
        );
        let (result, asked) = run(&mut state, NOW + HOUR, token("pk.new"));
        assert!(asked);
        assert_eq!(result, Ok("pk.new".to_owned()));
        assert_eq!(state.failures, 0, "a new token starts the count again");
    }

    #[test]
    fn an_answer_without_a_usable_token_counts_as_a_failure() {
        for answer in [
            Fetched::Nothing,
            token("sk.secret"),
            token("pk.has space"),
            token(""),
        ] {
            let mut state = State::default();
            let (result, _) = run(&mut state, NOW, answer);
            assert_eq!(
                result,
                Err(Unavailable::Waiting {
                    retry_at: NOW + HOUR
                })
            );
            assert_eq!(state.token, None, "nothing unusable is kept");
        }
    }

    #[test]
    fn no_answer_at_all_is_tried_again_next_run() {
        let mut state = saved("pk.old", true);
        let (result, _) = run(&mut state, NOW, Fetched::Failed("offline".to_owned()));
        assert_eq!(result, Err(Unavailable::Unreachable("offline".to_owned())));
        assert_eq!(state, saved("pk.old", true), "the schedule is untouched");
    }

    #[test]
    fn without_fetch_only_a_saved_token_is_used() {
        let mut state = State::default();
        let result = acquire(&mut state, NOW, false, || unreachable!());
        assert_eq!(result, Err(Unavailable::NotFetched));
    }

    #[test]
    fn a_wait_beyond_a_day_is_not_honored() {
        let mut state = State {
            retry_at: u64::MAX,
            ..saved("pk.old", true)
        };
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert!(asked);
        assert_eq!(result, Ok("pk.new".to_owned()));
    }

    #[test]
    fn a_saved_token_that_is_not_plausible_is_not_used() {
        let mut state = saved("sk.edited-in", false);
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert!(asked);
        assert_eq!(result, Ok("pk.new".to_owned()));
    }

    #[test]
    fn only_production_ignores_an_override() {
        let custom = || Some("http://127.0.0.1:1/token.json".to_owned());
        assert_eq!(override_outside_production(None, None), None);
        assert_eq!(override_outside_production(None, custom()), custom());
        assert_eq!(
            override_outside_production(Some("staging"), custom()),
            custom()
        );
        assert_eq!(
            override_outside_production(Some("production"), custom()),
            None
        );
        assert_eq!(
            override_outside_production(None, Some("  ".to_owned())),
            None
        );
    }

    #[test]
    fn only_a_dead_token_is_replaced() {
        for code in [
            "TokenRevoked",
            "TokenInvalid",
            "TokenExpired",
            "TokenMalformed",
        ] {
            assert_eq!(verdict(Some(code)), Refusal::Dead, "{code}");
        }
        assert_eq!(verdict(Some("TokenValid")), Refusal::Forbidden);
        assert_eq!(verdict(Some("SomethingNew")), Refusal::Unknown);
        assert_eq!(verdict(None), Refusal::Unknown, "the check itself failed");
    }

    #[test]
    fn the_state_round_trips_and_tolerates_a_partial_one() {
        let state = State {
            token: Some("pk.a".to_owned()),
            rejected: true,
            failures: 2,
            retry_at: 7,
        };
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
        assert_eq!(
            serde_json::from_str::<State>(r#"{"token":"pk.a"}"#).unwrap(),
            saved("pk.a", false)
        );
    }

    #[test]
    fn the_wait_reads_in_minutes_or_hours() {
        assert_eq!(wait_text(0), "1 minute");
        assert_eq!(wait_text(59 * 60), "59 minutes");
        assert_eq!(wait_text(HOUR), "1 hour");
        assert_eq!(wait_text(DAY), "24 hours");
    }
}
