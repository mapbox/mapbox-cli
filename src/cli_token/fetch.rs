//! Where the CLI's token comes from: fetched from the release channel, and
//! kept on disk until the Tokens API calls it dead.
//!
//! The token is published at `https://cli.mapbox.com/config/token.json` as
//! `{"token": "pk...."}`, so rotating it needs no release. It is fetched
//! when there is none on disk or the one there is dead, and after a caller
//! in the background is refused, and saved to `<config dir>/cli-token.json`.
//!
//! # A refusal
//!
//! A `401` or `403` alone does not say the token is dead: the API may only be
//! refusing it for one endpoint. So [`refused`] asks the Tokens API about
//! the token itself, and only a revoked, invalid, expired or malformed one is
//! marked dead. A fetch right after a rotation can still return the revoked
//! token: the edge caches the file for a few minutes, and the secret may not
//! be republished yet. So the dead token served again is checked once more,
//! since a token called invalid for something fixed in the Console stays the
//! same string; it is taken back if the Tokens API now calls it valid.
//!
//! # Each caller's quiet periods
//!
//! Only the token is shared. Each caller in the background, today only
//! telemetry, keeps its own count of failures in a row ([`Failures`]): a
//! fetch that found no usable token, or a refusal that the fetch right after
//! it did not answer with a different token, whether the Tokens API called
//! the refused one dead or not. A request that went through ends the count,
//! and so does a different token served, which is a fresh start. The third
//! failure in a row quiets the caller for an hour, during which it sends and
//! fetches nothing; once that is over, the next three quiet it again. So while
//! nothing changes, a caller asks the endpoint at most three times an hour.
//!
//! Nothing one caller does changes another's count, so one caller's failures
//! never stop another from trying. A command someone is waiting on, such as
//! `mapbox report`, keeps no count and is never held by one ([`token_now`]).
//!
//! A fetch that never got an answer, with no token to send with, does not
//! count: that is this machine being offline, not the endpoint being stale,
//! and it is asked again on the next run. After a refusal it does: the API
//! has just answered, and sending a refused token on every run would cost a
//! check each time.
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
/// Failures in a row before an hour's quiet.
const FAILURES_BEFORE_QUIET: u32 = 3;

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

/// What a fetch got back, before [`judge`] decides on it.
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
///
/// An earlier build also kept a pause shared by every caller (`failures`,
/// `retry_at`) and per-caller rests (`cooldowns`). They are not read, so a
/// file it wrote loads with no wait in it rather than holding anyone in one
/// after an upgrade, and the next write drops them.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    /// Whether the Tokens API called `token` dead. A `401` or `403` alone
    /// never sets it: the API may only be refusing it for one endpoint.
    /// Cleared if the endpoint serves it again and it now checks valid.
    #[serde(default)]
    rejected: bool,
    /// Each background caller's own failures, by the name [`token_for`]
    /// takes. Kept across a change of token: a caller's count is about it,
    /// not about the token, and only the caller itself ends it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    callers: BTreeMap<String, Failures>,
    /// Whether it was written with an endpoint overridden ([`overridden`]).
    /// A dev build pointed at staging and an installed release share this
    /// file, and neither is to trust the other's token or verdicts.
    #[serde(default)]
    overridden: bool,
}

/// One caller's failures in a row, and the quiet period the last third of
/// them started. Never longer than an hour: a token that is valid but
/// refused is a configuration someone can fix in the Console without the
/// token changing, and a fix should not wait long to be seen.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Failures {
    #[serde(default)]
    count: u32,
    /// Unix seconds before which the caller sends and fetches nothing.
    #[serde(default)]
    quiet_until: u64,
}

impl State {
    fn usable_token(&self) -> Option<&str> {
        self.token
            .as_deref()
            .filter(|token| !self.rejected && is_plausible(token))
    }

    fn quiet(&self, caller: &str, now: u64) -> bool {
        self.callers
            .get(caller)
            .is_some_and(|failures| holds(failures.quiet_until, now))
    }

    fn fail(&mut self, caller: &str, now: u64) {
        let failures = self.callers.entry(caller.to_owned()).or_default();
        failures.count += 1;
        if failures.count.is_multiple_of(FAILURES_BEFORE_QUIET) {
            failures.quiet_until = now + HOUR;
        }
    }
}

/// The CLI's token for `caller` — today only `telemetry` — or `None` while
/// it is quiet or there is none to be had: the one on disk while it is not
/// dead, otherwise a fresh one from the endpoint.
pub(crate) fn token_for(caller: &str) -> Option<String> {
    decided(caller, |state, now| {
        acquire(state, caller, now, || get(&endpoint()), check)
    })
}

/// Runs `decide` on the state on disk for `caller`, and saves what it
/// changed.
fn decided(caller: &str, decide: impl FnOnce(&mut State, u64) -> Option<String>) -> Option<String> {
    let mut state = read_state().unwrap_or_default();
    let before = state.clone();
    let token = decide(&mut state, now());
    save(&before, state, Some(caller));
    token
}

/// [`token_for`]'s decision, with the clock, the endpoint and the Tokens API
/// passed in.
fn acquire(
    state: &mut State,
    caller: &str,
    now: u64,
    get: impl FnOnce() -> Fetched,
    check: impl FnOnce(&str) -> Refusal,
) -> Option<String> {
    if state.quiet(caller, now) {
        return None;
    }
    if let Some(token) = state.usable_token() {
        return Some(token.to_owned());
    }
    let kept = state.token.clone();
    match get() {
        Fetched::Failed => None,
        fetched => {
            let token = judge(state, fetched, check);
            match token.as_deref() {
                None => state.fail(caller, now),
                // A fresh start. The dead token taken back is not one.
                Some(token) if Some(token) != kept.as_deref() => {
                    state.callers.remove(caller);
                }
                Some(_) => {}
            }
            token
        }
    }
}

/// Records that `caller`'s request went through, which ends its count.
pub(crate) fn accepted(caller: &str) {
    if let Some(mut state) = read_state().filter(|state| state.callers.contains_key(caller)) {
        let before = state.clone();
        state.callers.remove(caller);
        save(&before, state, Some(caller));
    }
}

/// The CLI's token, asking the endpoint now even with a token on disk: for a
/// command someone is waiting on, so that a token republished or
/// reconfigured is picked up at once. No caller's quiet period holds it.
///
/// Only a token it gets is saved. Nothing is counted: a caller's count paces
/// requests nobody is waiting on, and a command retried by hand must not
/// lengthen it.
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
    let token = judge(&mut state, fetched, check).ok_or(why)?;
    save(&before, state, None);
    Ok(token)
}

/// The token on disk while it is not dead, without asking the endpoint, for
/// a run that must not send anything, such as a dry run.
pub(crate) fn saved_token() -> Option<String> {
    read_state()?.usable_token().map(str::to_owned)
}

/// Writes what a run decided onto what is on disk now. A fetch or a check
/// can take seconds, and another run may have written meanwhile: if the
/// token or its verdict changed on disk, that run's stands, so a token
/// another run saved is never lost. `caller`'s own count is written as this
/// run left it, and every other caller's as it is on disk.
///
/// So two runs of one caller that fail at once can count one failure, not
/// two, having read the same count. That only ever costs an extra try
/// before the quiet period, never starts one early.
fn save(before: &State, state: State, caller: Option<&str>) {
    if state != *before {
        write_state(&merged(
            before,
            state,
            read_state().unwrap_or_default(),
            caller,
        ));
    }
}

/// [`save`]'s decision: what to write, given the state the run started
/// from, what it decided, and what is on disk now.
fn merged(before: &State, state: State, mut disk: State, caller: Option<&str>) -> State {
    if disk.token == before.token && disk.rejected == before.rejected {
        disk.token = state.token;
        disk.rejected = state.rejected;
    }
    if let Some(caller) = caller {
        let ours = state.callers.get(caller);
        if ours != before.callers.get(caller) {
            match ours {
                Some(failures) => disk.callers.insert(caller.to_owned(), failures.clone()),
                None => disk.callers.remove(caller),
            };
        }
    }
    disk
}

/// Why [`token_for`] cannot have one for `caller`, judged without the
/// network: `quiet` after its own failures. `None` when it may.
pub(crate) fn no_token_for(caller: &str) -> Option<&'static str> {
    let state = read_state().unwrap_or_default();
    state.quiet(caller, now()).then_some("quiet")
}

/// What a `401` or `403` to the CLI's token meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The token itself is revoked, invalid, expired or malformed. It is
    /// marked, so the next fetch replaces it.
    Dead,
    /// The token is fine; this API does not accept it.
    Forbidden,
    /// The Tokens API could not say. The token is kept, and counted against
    /// a caller in the background as for [`Refusal::Forbidden`]: sending it
    /// again at once would most likely be refused again, and cost another
    /// check.
    Unknown,
    /// Not checked: `token` is not the one on disk. Another run saved a new
    /// one meanwhile, or the state cannot be written at all, in which case
    /// nothing is marked or counted and each run fetches as if for the first
    /// time (see [`write_state`]).
    NotFetched,
}

/// Asks the Tokens API, at once, why `caller`'s request with the fetched
/// `token` was refused, and marks a dead token.
///
/// For a caller in the `background`, such as telemetry, the endpoint is
/// then asked right away, so a token rotated or republished is on disk for
/// the next run: a different one served starts the caller's count over, and
/// anything else counts a failure. A command someone is waiting on keeps no
/// count; it tries a fresh token itself ([`token_now`]).
pub(crate) fn refused(caller: &str, token: &str, background: bool) -> Refusal {
    if read_state()
        .filter(|state| state.token.as_deref() == Some(token))
        .is_none()
    {
        return Refusal::NotFetched;
    }
    let refusal = check(token);
    // Read again: the check can take seconds, and another run may have
    // saved a new token meanwhile, which this must not write over.
    if refusal == Refusal::Dead {
        if let Some(mut state) = read_state().filter(|state| state.token.as_deref() == Some(token))
        {
            state.rejected = true;
            write_state(&state);
        }
    }
    if background {
        decided(caller, |state, now| {
            refetch(state, caller, token, now, get(&endpoint()), check);
            None
        });
    }
    refusal
}

/// The fetch after a background caller's `refused` token, with its answer
/// and the Tokens API passed in. Nothing at all, the same token again, or
/// the dead one still dead, is one failure; this run's event is not sent
/// again either way.
fn refetch(
    state: &mut State,
    caller: &str,
    refused: &str,
    now: u64,
    fetched: Fetched,
    check: impl FnOnce(&str) -> Refusal,
) {
    match judge(state, fetched, check) {
        Some(token) if token != refused => {
            state.callers.remove(caller);
        }
        _ => state.fail(caller, now),
    }
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

/// What a fetch's answer does to the token on disk, and the token it gives,
/// if any. A token that is new replaces what is kept. The dead one served
/// again is taken back only if it now checks valid. Callers' counts are left
/// to the caller.
fn judge(
    state: &mut State,
    fetched: Fetched,
    check: impl FnOnce(&str) -> Refusal,
) -> Option<String> {
    // Not a `pk.` token, or no token at all.
    let Fetched::Token(token) = fetched else {
        return None;
    };
    if !is_plausible(&token) {
        return None;
    }
    if state.token.as_deref() == Some(token.as_str()) {
        if state.rejected {
            if check(&token) != Refusal::Forbidden {
                return None;
            }
            state.rejected = false;
        }
    } else {
        state.token = Some(token.clone());
        state.rejected = false;
    }
    Some(token)
}

/// Whether a quiet period until `until` still holds at `now`. One further
/// out than the longest, an hour, is a clock that went backwards or an
/// edited file, and is not honored.
fn holds(until: u64, now: u64) -> bool {
    now < until && until <= now + HOUR
}

/// Whether `value` looks like a public token and nothing else. Checked on
/// what the endpoint returns, on what is read back from disk and on
/// telemetry's `MAPBOX_CLI_TOKEN`, since all end up in a request.
pub(super) fn is_plausible(value: &str) -> bool {
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
    const CALLER: &str = "telemetry";

    fn saved(token: &str, rejected: bool) -> State {
        State {
            token: Some(token.to_owned()),
            rejected,
            ..State::default()
        }
    }

    fn with_failures(mut state: State, count: u32, quiet_until: u64) -> State {
        state
            .callers
            .insert(CALLER.to_owned(), Failures { count, quiet_until });
        state
    }

    fn count(state: &State) -> u32 {
        state
            .callers
            .get(CALLER)
            .map_or(0, |failures| failures.count)
    }

    /// Runs [`acquire`] for telemetry with an endpoint that answers `answer`
    /// and a Tokens API that still calls the token dead, and says whether
    /// the endpoint was asked.
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
            CALLER,
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
        let mut state = with_failures(saved("pk.old", true), 2, 0);
        let (result, _) = run(&mut state, NOW, token("pk.new"));
        assert_eq!(result, Some("pk.new".to_owned()));
        assert_eq!(state, saved("pk.new", false), "a new token starts over");
    }

    #[test]
    fn fetches_with_no_usable_token_quiet_the_caller_after_every_third() {
        let mut state = saved("pk.old", true);
        let mut now = NOW;
        let mut quiet = Vec::new();
        for _ in 0..8 {
            let (result, asked) = run(&mut state, now, token("pk.old"));
            assert!(asked);
            assert_eq!(result, None);
            let until = state.callers[CALLER].quiet_until;
            quiet.push(if holds(until, now) { until - now } else { 0 });
            now = now.max(until);
        }
        assert_eq!(
            quiet,
            [0, 0, HOUR, 0, 0, HOUR, 0, 0],
            "an hour after every third failure in a row"
        );
        assert_eq!(count(&state), 8);
    }

    #[test]
    fn nothing_is_fetched_while_quiet_and_asking_resumes_after_the_hour() {
        let mut state = with_failures(saved("pk.old", true), 3, NOW + HOUR);
        let (result, asked) = run(&mut state, NOW + HOUR - 1, token("pk.new"));
        assert!(!asked);
        assert_eq!(result, None);
        let (result, asked) = run(&mut state, NOW + HOUR, token("pk.new"));
        assert!(asked);
        assert_eq!(result, Some("pk.new".to_owned()));
        assert_eq!(count(&state), 0, "a new token starts the count again");
    }

    #[test]
    fn a_quiet_caller_does_not_use_even_a_good_saved_token() {
        let mut state = with_failures(saved("pk.kept", false), 3, NOW + 10);
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert_eq!(result, None);
        assert!(!asked);
    }

    #[test]
    fn the_rejected_token_again_is_taken_back_once_it_checks_valid() {
        let mut state = with_failures(saved("pk.old", true), 2, 0);
        let (result, _, checked) =
            run_checked(&mut state, NOW, token("pk.old"), Refusal::Forbidden);
        assert!(checked);
        assert_eq!(result, Some("pk.old".to_owned()));
        assert!(!state.rejected);
        assert_eq!(count(&state), 2, "the same token is no fresh start");

        for verdict in [Refusal::Dead, Refusal::Unknown] {
            let mut state = saved("pk.old", true);
            let (result, _, _) = run_checked(&mut state, NOW, token("pk.old"), verdict);
            assert_eq!(result, None, "{verdict:?}");
            assert!(state.rejected, "{verdict:?}");
            assert_eq!(count(&state), 1, "{verdict:?}");
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
            assert_eq!(
                state.callers[CALLER],
                Failures {
                    count: 1,
                    quiet_until: 0
                }
            );
            assert_eq!(state.token, None, "nothing unusable is kept");
        }
    }

    #[test]
    fn no_answer_at_all_is_tried_again_next_run() {
        let mut state = saved("pk.old", true);
        let (result, _) = run(&mut state, NOW, Fetched::Failed);
        assert_eq!(result, None);
        assert_eq!(state, saved("pk.old", true), "nothing is counted");
    }

    #[test]
    fn judging_a_fetch_leaves_every_callers_count_alone() {
        let mut state = with_failures(saved("pk.kept", false), 1, NOW);
        let before = state.clone();
        assert_eq!(
            judge(&mut state, token("pk.kept"), |_| Refusal::Dead),
            Some("pk.kept".to_owned())
        );
        assert_eq!(state, before);
        assert_eq!(
            judge(&mut state, token("pk.new"), |_| Refusal::Dead),
            Some("pk.new".to_owned())
        );
        assert_eq!(state.token.as_deref(), Some("pk.new"));
        assert_eq!(
            state.callers, before.callers,
            "only a caller ends its count"
        );
    }

    #[test]
    fn a_quiet_period_beyond_an_hour_is_not_honored() {
        let mut state = with_failures(saved("pk.old", true), 3, u64::MAX);
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert!(asked);
        assert_eq!(result, Some("pk.new".to_owned()));
        assert!(holds(NOW + HOUR, NOW));
        assert!(!holds(NOW + HOUR, NOW + HOUR));
    }

    #[test]
    fn a_saved_token_that_is_not_plausible_is_not_used() {
        let mut state = saved("sk.edited-in", false);
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert!(asked);
        assert_eq!(result, Some("pk.new".to_owned()));
    }

    #[test]
    fn another_callers_quiet_period_does_not_hold_this_one() {
        let mut other = State::default();
        other.callers.insert(
            "other".to_owned(),
            Failures {
                count: 3,
                quiet_until: NOW + HOUR,
            },
        );
        let mut state = other.clone();
        let (result, asked) = run(&mut state, NOW, token("pk.new"));
        assert!(asked);
        assert_eq!(result, Some("pk.new".to_owned()));
        assert_eq!(state.callers, other.callers, "the other's count stands");

        let mut state = other.clone();
        run(&mut state, NOW, Fetched::Nothing);
        assert_eq!(state.callers["other"], other.callers["other"]);
        assert_eq!(count(&state), 1, "this caller's own failure");
    }

    /// Runs [`refetch`] for telemetry after `pk.old` was refused, with the
    /// endpoint answering `answer` and the Tokens API calling a dead token
    /// still dead.
    fn refused_then(state: &mut State, now: u64, answer: Fetched) {
        refetch(state, CALLER, "pk.old", now, answer, |_| Refusal::Dead);
    }

    #[test]
    fn a_different_token_after_a_refusal_is_saved_and_starts_over() {
        for rejected in [false, true] {
            let mut state = with_failures(saved("pk.old", rejected), 2, 0);
            refused_then(&mut state, NOW, token("pk.new"));
            assert_eq!(state, saved("pk.new", false), "rejected: {rejected}");
        }
    }

    #[test]
    fn a_refusal_the_fetch_does_not_resolve_is_a_failure() {
        for (rejected, answer) in [
            (false, token("pk.old")),
            (true, token("pk.old")),
            (false, Fetched::Nothing),
            (false, Fetched::Failed),
            (true, Fetched::Failed),
        ] {
            let mut state = saved("pk.old", rejected);
            refused_then(&mut state, NOW, answer);
            assert_eq!(count(&state), 1, "rejected: {rejected}");
            assert_eq!(state.token.as_deref(), Some("pk.old"));
            assert_eq!(state.rejected, rejected);
        }
    }

    #[test]
    fn the_same_token_after_three_refusals_quiets_the_caller() {
        let mut state = saved("pk.old", false);
        for _ in 0..3 {
            assert!(!state.quiet(CALLER, NOW));
            refused_then(&mut state, NOW, token("pk.old"));
        }
        assert!(state.quiet(CALLER, NOW));
        assert!(!state.quiet(CALLER, NOW + HOUR), "for an hour");
    }

    #[test]
    fn a_save_writes_this_callers_count_over_what_is_on_disk() {
        let before = saved("pk.old", false);
        let mut failed = before.clone();
        failed.fail(CALLER, NOW);
        let mut disk = before.clone();
        disk.callers.insert("other".to_owned(), Failures::default());
        let written = merged(&before, failed.clone(), disk.clone(), Some(CALLER));
        assert_eq!(written.callers[CALLER], failed.callers[CALLER]);
        assert!(written.callers.contains_key("other"), "another's is kept");

        // A count this run did not change is not written over another run's.
        let mut counted = before.clone();
        counted.fail(CALLER, NOW);
        counted.fail(CALLER, NOW);
        let written = merged(&before, before.clone(), counted.clone(), Some(CALLER));
        assert_eq!(written, counted);
    }

    #[test]
    fn a_token_another_run_saved_meanwhile_stands() {
        let before = saved("pk.old", true);
        let fresh = saved("pk.new", false);
        let other = saved("pk.other", false);
        let written = merged(&before, fresh.clone(), other.clone(), Some(CALLER));
        assert_eq!(written.token, other.token);

        let mut marked = before.clone();
        marked.rejected = false;
        let written = merged(&before, fresh.clone(), marked, Some(CALLER));
        assert_eq!(written.token.as_deref(), Some("pk.old"), "a verdict stands");

        let written = merged(&before, fresh.clone(), before.clone(), None);
        assert_eq!(written, fresh);
    }

    #[test]
    fn a_command_saves_no_count() {
        let before = saved("pk.old", true);
        let mut state = before.clone();
        state.fail(CALLER, NOW);
        let written = merged(&before, state, before.clone(), None);
        assert!(written.callers.is_empty());
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
            callers: BTreeMap::from([(
                CALLER.to_owned(),
                Failures {
                    count: 1,
                    quiet_until: 9,
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

    /// A file an earlier build wrote, paused for everyone and resting
    /// telemetry, loads with no wait in it.
    #[test]
    fn an_earlier_builds_waits_are_not_read() {
        let old = format!(
            r#"{{"token":"pk.a","rejected":false,"overridden":true,"failures":3,
            "retry_at":{later},"cooldowns":{{"telemetry":{{"failures":3,
            "until":{later},"misses":3,"retry_at":{later}}}}}}}"#,
            later = NOW + 10
        );
        let mut state: State = serde_json::from_str(&old).expect("an old file reads");
        assert_eq!(
            state,
            State {
                overridden: true,
                ..saved("pk.a", false)
            }
        );
        assert!(!state.quiet(CALLER, NOW));
        let (result, _) = run(&mut state, NOW, token("pk.new"));
        assert_eq!(result, Some("pk.a".to_owned()));
        assert!(!serde_json::to_string(&state).unwrap().contains("retry_at"));
    }
}
