//! Where the CLI's token comes from: fetched from the release channel, and
//! kept on disk until the Tokens API calls it dead.
//!
//! The token is published at `https://cli.mapbox.com/config/token.json` as
//! `{"token": "pk...."}`, so rotating it needs no release. It is fetched
//! when there is none on disk or the one there is dead, and saved to
//! `<config dir>/cli-token.json`.
//!
//! # When the endpoint has nothing new
//!
//! A `401` or `403` alone does not say the token is dead: the API may only be
//! refusing it for one endpoint. So [`refused`] asks the Tokens API about
//! the token itself, and only a revoked, invalid, expired or malformed one is
//! replaced. A fetch right after a
//! rotation can still return the revoked token: the edge caches the file for
//! a few minutes, and the secret may not be republished yet. So the dead
//! token served again is checked once more, since a token called invalid for
//! something fixed in the Console stays the same string; it is taken back if
//! the Tokens API now calls it valid. When fetches answer with nothing usable
//! three times running, the next waits an hour ([`backoff`]). A fetch that
//! never got an answer
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
//! The state file is what makes all of this hold across runs, so writing it
//! creates the config directory when there is none — a run that never needs
//! the CLI's token, or only fails to reach the endpoint, still leaves no
//! trace. See [`write_state`].

use std::collections::BTreeMap;
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
/// Misses in a row before an hour's wait.
const TRIES_BEFORE_WAITING: u32 = 3;

/// Long enough for any `pk.` token; anything longer is not one.
const MAX_TOKEN_LEN: usize = 512;

/// Why there is no CLI token to use right now, as a command that wanted one
/// tells its user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// The endpoint could not be reached. Asked again on the next run.
    Unreachable,
    /// The endpoint answered without a usable token. `revoked` when there
    /// is a dead token it has not replaced yet, rather than never a token at
    /// all.
    NoToken { revoked: bool },
    /// None on disk, and the caller asked not to fetch, as a dry run does.
    NotFetched,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable => write!(f, "the token endpoint could not be reached"),
            Self::NoToken { revoked: true } => {
                write!(f, "it is revoked and no new one is published yet")
            }
            Self::NoToken { revoked: false } => {
                write!(f, "the token endpoint did not return a usable token")
            }
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
    Failed,
}

/// What is kept on disk between runs.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    /// Whether the Tokens API called `token` dead. A `401` or `403` alone
    /// never sets it: the API may only be refusing it for one endpoint.
    /// Cleared if the endpoint serves it again and it now checks valid.
    #[serde(default)]
    rejected: bool,
    /// Who is resting from using `token`, by the name [`cooling`] takes:
    /// an API refused it while the Tokens API called it valid. Cleared with
    /// a new token, which that API may accept.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    cooldowns: BTreeMap<String, Cooldown>,
    /// Whether it was written with an endpoint overridden ([`overridden`]).
    /// A dev build pointed at staging and an installed release share this
    /// file, and neither is to trust the other's token or verdicts.
    #[serde(default)]
    overridden: bool,
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
        self.retry_at = now + backoff(self.failures);
    }
}

/// One caller's rest from the CLI's token, after an API refused it and the
/// Tokens API could not call it dead, paced as the fetch is ([`backoff`]).
/// Never longer than an hour: a token that is valid but refused is a
/// configuration someone can fix in the Console without the token changing,
/// and a fix should not wait long to be seen.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cooldown {
    /// Consecutive refusals.
    #[serde(default)]
    failures: u32,
    /// Unix seconds before which it is not used again.
    #[serde(default)]
    until: u64,
}

/// The wait after `failures` consecutive misses: an hour after every third,
/// none otherwise, so a miss that was only bad timing costs nothing.
fn backoff(failures: u32) -> u64 {
    if failures.is_multiple_of(TRIES_BEFORE_WAITING) {
        HOUR
    } else {
        0
    }
}

/// Whether `caller` — today only `telemetry` — is resting from the
/// CLI's token. While it is, it sends nothing with it, so nothing is
/// refused, and nothing is checked.
pub(crate) fn cooling(caller: &str) -> bool {
    read_state().is_some_and(|state| {
        state
            .cooldowns
            .get(caller)
            .is_some_and(|cooldown| holds(cooldown.until, now()))
    })
}

/// Records that `caller`'s request with the saved `token` went through, which
/// ends any cooldown it was counting.
pub(crate) fn accepted(caller: &str, token: &str) {
    if let Some(mut state) = read_state().filter(|state| state.token.as_deref() == Some(token)) {
        if state.cooldowns.remove(caller).is_some() {
            write_state(&state);
        }
    }
}

/// The CLI's token: the one on disk while it is not dead, otherwise a fresh
/// one from the endpoint unless it is being waited for. `None` when there is
/// none to be had right now.
pub(crate) fn token() -> Option<String> {
    let mut state = read_state().unwrap_or_default();
    let before = state.clone();
    let token = acquire(&mut state, now(), || get(&endpoint()), check);
    if state != before {
        save_fetch(&before, state);
    }
    token
}

/// [`token`], but asking the endpoint now, whatever the backoff says and
/// even with a token on disk: for a command someone is waiting on, so that a
/// token republished or reconfigured is picked up at once.
///
/// Only a token it gets is saved. A miss is not counted: the backoff paces
/// background fetches, and a command retried by hand must not lengthen it.
pub(crate) fn token_now() -> Result<String, Unavailable> {
    let mut state = read_state().unwrap_or_default();
    let before = state.clone();
    let fetched = get(&endpoint());
    let why = match fetched {
        Fetched::Failed => Unavailable::Unreachable,
        _ => Unavailable::NoToken {
            revoked: state.rejected,
        },
    };
    let token = judge(&mut state, now(), fetched, check).ok_or(why)?;
    if state != before {
        save_fetch(&before, state);
    }
    Ok(token)
}

/// The token on disk while it is not dead, without asking the endpoint, for
/// a run that must not send anything, such as a dry run.
pub(crate) fn saved_token() -> Option<String> {
    read_state()?.usable_token().map(str::to_owned)
}

/// Writes what a fetch decided, unless another run decided first. The fetch
/// can take seconds: if the token, its verdict or the count changed on disk
/// meanwhile, that run's answer stands. Cooldowns another run added are
/// kept, unless this fetch brought a new token, which ends them all.
fn save_fetch(before: &State, state: State) {
    if let Some(state) = merged(before, state, read_state().unwrap_or_default()) {
        write_state(&state);
    }
}

/// [`save_fetch`]'s decision: what to write, given the state the fetch
/// started from, what it decided, and what is on disk now.
fn merged(before: &State, mut state: State, disk: State) -> Option<State> {
    let same_fetch = disk.token == before.token
        && disk.rejected == before.rejected
        && disk.failures == before.failures
        && disk.retry_at == before.retry_at;
    if !same_fetch {
        return None;
    }
    if state.token == before.token {
        state.cooldowns = disk.cooldowns;
    }
    Some(state)
}

/// Whether [`token`] could have one, judged without the network.
pub(crate) fn may_have_token() -> bool {
    let state = read_state().unwrap_or_default();
    state.usable_token().is_some() || !holds(state.retry_at, now())
}

/// What a `401` or `403` to the CLI's token meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The token itself is revoked, invalid, expired or malformed. It is
    /// marked, so the next [`token`] fetches a new one.
    Dead,
    /// The token is fine; this API does not accept it.
    Forbidden,
    /// The Tokens API could not say. The token is kept, and the caller rests
    /// as for [`Refusal::Forbidden`]: sending it again at once would most
    /// likely be refused again, and cost another check.
    Unknown,
    /// Not checked: `token` is not the one on disk. Another run saved a new
    /// one meanwhile, or the state cannot be written at all, in which case
    /// nothing is marked or paced and each run fetches as if for the first
    /// time (see [`write_state`]).
    NotFetched,
}

/// Asks the Tokens API, at once, why `caller`'s request with the fetched
/// `token` was refused. A dead token is marked, so the next [`token`]
/// fetches a new one; otherwise `caller`'s refusals are counted, and the
/// third in a row rests it for an hour.
///
/// Only a caller that `rests` is paced: one in the background, such as
/// telemetry. A command someone is waiting on is never held back; it tries
/// a fresh token instead ([`token_now`]).
pub(crate) fn refused(caller: &str, token: &str, rests: bool) -> Refusal {
    let now = now();
    if read_state()
        .filter(|state| state.token.as_deref() == Some(token))
        .is_none()
    {
        return Refusal::NotFetched;
    }
    let refusal = check(token);
    // Read again: the check can take seconds, and another run may have
    // saved a new token meanwhile, which this must not write over.
    if let Some(mut state) = read_state().filter(|state| state.token.as_deref() == Some(token)) {
        // `verdict` never answers `NotFetched`; that was returned above.
        if refusal == Refusal::Dead {
            state.rejected = true;
        } else if !rests {
            return refusal;
        } else {
            let cooldown = state.cooldowns.entry(caller.to_owned()).or_default();
            cooldown.failures += 1;
            cooldown.until = now + backoff(cooldown.failures);
        }
        write_state(&state);
    }
    refusal
}

/// What the Tokens API says about `token`. Never [`Refusal::NotFetched`].
fn check(token: &str) -> Refusal {
    verdict(auth::token_code(&tokens_url(), token, Some(super::REFERER), FETCH_TIMEOUT).as_deref())
}

fn tokens_url() -> String {
    http::url_override(TOKENS_URL_ENV).unwrap_or_else(|| auth::VALIDATION_ENDPOINT.to_owned())
}

/// Whether either endpoint is overridden from the environment, as only a
/// build that is not a production release allows.
fn overridden() -> bool {
    http::url_override(URL_ENV).is_some() || http::url_override(TOKENS_URL_ENV).is_some()
}

fn verdict(code: Option<&str>) -> Refusal {
    match code {
        Some("TokenValid") => Refusal::Forbidden,
        Some("TokenRevoked" | "TokenInvalid" | "TokenExpired" | "TokenMalformed") => Refusal::Dead,
        _ => Refusal::Unknown,
    }
}

/// The decision, with the clock, the endpoint and the Tokens API passed in.
fn acquire(
    state: &mut State,
    now: u64,
    get: impl FnOnce() -> Fetched,
    check: impl FnOnce(&str) -> Refusal,
) -> Option<String> {
    if let Some(token) = state.usable_token() {
        return Some(token.to_owned());
    }
    if holds(state.retry_at, now) {
        return None;
    }
    judge(state, now, get(), check)
}

/// What a fetch's answer does to the state. A token that is new, or the dead
/// one taken back, replaces what is kept, cooldowns and counts with it; the
/// kept token again, not dead, leaves it as it is.
fn judge(
    state: &mut State,
    now: u64,
    fetched: Fetched,
    check: impl FnOnce(&str) -> Refusal,
) -> Option<String> {
    match fetched {
        Fetched::Failed => None,
        Fetched::Token(token)
            if is_plausible(&token)
                && (!(state.rejected && state.token.as_deref() == Some(token.as_str()))
                    || check(&token) == Refusal::Forbidden) =>
        {
            if state.rejected || state.token.as_deref() != Some(token.as_str()) {
                *state = State {
                    token: Some(token.clone()),
                    ..State::default()
                };
            }
            Some(token)
        }
        // The rejected token again and still not valid, something that is
        // not a `pk.` token, or no token at all.
        Fetched::Token(_) | Fetched::Nothing => {
            state.fail(now);
            None
        }
    }
}

/// Whether a wait until `until` still holds at `now`: the endpoint's, or a
/// caller's cooldown. One further out than the longest wait, an hour, is a
/// clock that went backwards or an edited file, and is not honored.
fn holds(until: u64, now: u64) -> bool {
    now < until && until <= now + HOUR
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
    http::url_override(URL_ENV).unwrap_or_else(|| PRODUCTION_URL.to_owned())
}

fn get(url: &str) -> Fetched {
    let Ok(client) = http::client() else {
        return Fetched::Failed;
    };
    let Ok(response) = http::send(client.get(url).timeout(FETCH_TIMEOUT)) else {
        return Fetched::Failed;
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
    Some(auth::config_dir_path()?.join(STATE_FILE))
}

/// The state on disk, or `None` when there is no readable one. A malformed
/// file costs one fetch, which is not worth failing anything over.
fn read_state() -> Option<State> {
    let text = std::fs::read_to_string(state_path()?).ok()?;
    let state: State = serde_json::from_str(&text).ok()?;
    (state.overridden == overridden()).then_some(state)
}

/// Best-effort: a read-only home directory costs a fetch per run, not an
/// error.
///
/// Creates the config directory when there is none, `0700` like any other
/// first write to it, because without this file a dead token is never marked
/// and nothing backs off. An existing one is written into as it is, so its
/// permissions are not changed by a run that only sent telemetry. A plain
/// file where the directory belongs leaves nothing to write into, and each
/// run then fetches as if for the first time.
fn write_state(state: &State) {
    let state = &State {
        overridden: overridden(),
        ..state.clone()
    };
    let Some(dir) = auth::config_dir_path() else {
        return;
    };
    let dir = if dir.is_dir() {
        dir
    } else if dir.exists() {
        return;
    } else {
        match auth::config_dir() {
            Ok(dir) => dir,
            Err(_) => return,
        }
    };
    if let Ok(text) = serde_json::to_string(state) {
        let _ = auth::write_private(&dir.join(STATE_FILE), &text);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
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

    /// Runs [`acquire`] with an endpoint that answers `answer` and a Tokens
    /// API that still calls the token dead, and says whether the endpoint was
    /// asked.
    fn run(state: &mut State, now: u64, answer: Fetched) -> (Option<String>, bool) {
        let (result, asked, _) = run_checked(state, now, answer, Refusal::Dead);
        (result, asked)
    }

    /// [`run`] with the Tokens API answering `verdict`, and also says whether
    /// it was asked.
    fn run_checked(
        state: &mut State,
        now: u64,
        answer: Fetched,
        verdict: Refusal,
    ) -> (Option<String>, bool, bool) {
        let (mut asked, mut checked) = (false, false);
        let result = acquire(
            state,
            now,
            || {
                asked = true;
                answer
            },
            |_| {
                checked = true;
                verdict
            },
        );
        (result, asked, checked)
    }

    fn token(value: &str) -> Fetched {
        Fetched::Token(value.to_owned())
    }

    #[test]
    fn a_saved_token_is_used_without_asking() {
        let mut state = saved("pk.saved", false);
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert_eq!(result, Some("pk.saved".to_owned()));
        assert!(!asked);
    }

    #[test]
    fn no_saved_token_fetches_one_and_keeps_it() {
        let mut state = State::default();
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert_eq!(result, Some("pk.new".to_owned()));
        assert!(asked);
        assert_eq!(state, saved("pk.new", false));
    }

    #[test]
    fn a_rejected_token_is_replaced_by_a_new_one() {
        let mut state = saved("pk.old", true);
        let (result, _) = run(&mut state, NOW, token("pk.new"));
        assert_eq!(result, Some("pk.new".to_owned()));
        assert_eq!(state, saved("pk.new", false));
    }

    #[test]
    fn the_rejected_token_again_waits_an_hour_after_every_third_time() {
        let mut state = saved("pk.old", true);
        let mut now = NOW;
        let mut waits = Vec::new();
        for _ in 0..8 {
            let (result, asked) = run(&mut state, now, token("pk.old"));
            assert!(asked);
            assert_eq!(result, None);
            waits.push(state.retry_at - now);
            now = state.retry_at;
        }
        assert_eq!(
            waits,
            [0, 0, HOUR, 0, 0, HOUR, 0, 0],
            "an hour after every third failure in a row"
        );
    }

    #[test]
    fn the_rejected_token_again_is_taken_back_once_it_checks_valid() {
        let mut state = State {
            failures: 2,
            ..saved("pk.old", true)
        };
        let (result, _, checked) =
            run_checked(&mut state, NOW, token("pk.old"), Refusal::Forbidden);
        assert!(checked);
        assert_eq!(result, Some("pk.old".to_owned()));
        assert_eq!(state, saved("pk.old", false), "the count starts again");

        for verdict in [Refusal::Dead, Refusal::Unknown] {
            let mut state = saved("pk.old", true);
            let (result, _, _) = run_checked(&mut state, NOW, token("pk.old"), verdict);
            assert_eq!(result, None, "{verdict:?}");
            assert!(state.rejected, "{verdict:?}");
            assert_eq!(state.failures, 1, "{verdict:?}");
        }
    }

    #[test]
    fn only_the_rejected_token_served_again_is_checked() {
        for (mut state, answer) in [
            (State::default(), token("pk.new")),
            (saved("pk.old", true), token("pk.new")),
            (saved("pk.old", true), Fetched::Nothing),
            (saved("pk.old", true), Fetched::Failed),
        ] {
            let (_, _, checked) = run_checked(&mut state, NOW, answer, Refusal::Forbidden);
            assert!(!checked);
        }
    }

    #[test]
    fn nothing_is_fetched_while_waiting() {
        let mut state = State {
            retry_at: NOW + HOUR,
            ..saved("pk.old", true)
        };
        let (result, asked) = run(&mut state, NOW + HOUR - 1, token("pk.new"));
        assert!(!asked);
        assert_eq!(result, None);
        let (result, asked) = run(&mut state, NOW + HOUR, token("pk.new"));
        assert!(asked);
        assert_eq!(result, Some("pk.new".to_owned()));
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
            assert_eq!(result, None);
            assert_eq!(state.failures, 1);
            assert_eq!(state.retry_at, NOW, "the first failure waits for nothing");
            assert_eq!(state.token, None, "nothing unusable is kept");
        }
    }

    #[test]
    fn no_answer_at_all_is_tried_again_next_run() {
        let mut state = saved("pk.old", true);
        let (result, _) = run(&mut state, NOW, Fetched::Failed);
        assert_eq!(result, None);
        assert_eq!(state, saved("pk.old", true), "the schedule is untouched");
    }

    #[test]
    fn a_forced_fetch_of_the_kept_token_keeps_what_is_known() {
        let mut state = State {
            cooldowns: BTreeMap::from([(
                "telemetry".to_owned(),
                Cooldown {
                    failures: 1,
                    until: NOW,
                },
            )]),
            ..saved("pk.kept", false)
        };
        let before = state.clone();
        assert_eq!(
            judge(&mut state, NOW, token("pk.kept"), |_| Refusal::Dead),
            Some("pk.kept".to_owned())
        );
        assert_eq!(state, before);
        assert_eq!(
            judge(&mut state, NOW, token("pk.new"), |_| Refusal::Dead),
            Some("pk.new".to_owned())
        );
        assert_eq!(state, saved("pk.new", false), "a new token starts over");
    }

    #[test]
    fn a_wait_beyond_an_hour_is_not_honored() {
        let mut state = State {
            retry_at: u64::MAX,
            ..saved("pk.old", true)
        };
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert!(asked);
        assert_eq!(result, Some("pk.new".to_owned()));
    }

    #[test]
    fn a_saved_token_that_is_not_plausible_is_not_used() {
        let mut state = saved("sk.edited-in", false);
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert!(asked);
        assert_eq!(result, Some("pk.new".to_owned()));
    }

    #[test]
    fn a_cooldown_holds_for_its_hour_and_not_beyond() {
        let cooldown = Cooldown {
            failures: 3,
            until: NOW + HOUR,
        };
        assert!(holds(cooldown.until, NOW));
        assert!(!holds(cooldown.until, NOW + HOUR));
        let edited = Cooldown {
            failures: 3,
            until: u64::MAX,
        };
        assert!(
            !holds(edited.until, NOW),
            "a wait beyond an hour is not honored"
        );
    }

    #[test]
    fn a_fetch_keeps_what_another_run_wrote_meanwhile() {
        let resting = || State {
            cooldowns: BTreeMap::from([(
                "telemetry".to_owned(),
                Cooldown {
                    failures: 3,
                    until: NOW + HOUR,
                },
            )]),
            ..saved("pk.old", true)
        };
        let before = saved("pk.old", true);

        // A cooldown added meanwhile is kept by a fetch that only failed...
        let mut failed = before.clone();
        failed.fail(NOW);
        let written = merged(&before, failed.clone(), resting()).expect("written");
        assert_eq!(written.failures, 1);
        assert_eq!(written.cooldowns, resting().cooldowns);

        // ...and ended by one that brought a new token.
        let fresh = saved("pk.new", false);
        let written = merged(&before, fresh.clone(), resting()).expect("written");
        assert_eq!(written, fresh);

        // A token or a count another run saved meanwhile stands.
        assert_eq!(
            merged(&before, fresh.clone(), saved("pk.other", false)),
            None
        );
        let mut counted = before.clone();
        counted.fail(NOW);
        assert_eq!(merged(&before, failed, counted), None);
    }

    #[test]
    fn no_token_says_whether_one_was_ever_revoked() {
        let reason = |revoked| Unavailable::NoToken { revoked }.to_string();
        assert!(
            reason(true).starts_with("it is revoked"),
            "{}",
            reason(true)
        );
        assert!(
            reason(false).starts_with("the token endpoint did not return"),
            "{}",
            reason(false)
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
            cooldowns: BTreeMap::from([(
                "telemetry".to_owned(),
                Cooldown {
                    failures: 1,
                    until: 9,
                },
            )]),
            overridden: true,
        };
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
        assert_eq!(
            serde_json::from_str::<State>(r#"{"token":"pk.a"}"#).unwrap(),
            saved("pk.a", false)
        );
    }
}
