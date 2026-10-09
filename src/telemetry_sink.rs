//! Where the run's telemetry event goes once [`crate::telemetry_event`] has
//! built it.
//!
//! This module never decides what an event contains; it is handed one
//! finished JSON line. That keeps the privacy rules in one file and lets a
//! delivery change — a new endpoint, batching — happen without touching them.
//!
//! [`deliver`] hands the event to [`HttpSink`], which sends it to Mapbox
//! Events, when [`crate::cli_token`] has a token it could send it with.
//! Otherwise the event is dropped: nothing would ever send a copy kept on
//! disk, so keeping one would only store usage data for nobody. The state
//! files below are still written, since building the event reads them.
//!
//! It also owns the directory, `~/.mapbox/.telemetry` (or under
//! `$MAPBOX_CONFIG_DIR`), written only once the config directory exists: the
//! two small state files the event reads — the installation id and the last
//! version seen — and the delivery log, written and pruned through
//! [`crate::dated_jsonl`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use reqwest::header::{CONTENT_TYPE, USER_AGENT};
use serde_json::json;

use crate::auth;
use crate::cli_token;
use crate::dated_jsonl;
use crate::http;
use crate::telemetry;

const DIR: &str = ".telemetry";
/// Days of delivery-log files kept, today included.
const KEEP_DAYS: u64 = 7;

/// Production Mapbox Events, for every build. A login is a production login,
/// so a build from source sends where a release does rather than to a
/// staging host its tokens mean nothing to.
const ENDPOINT: &str = "https://events.mapbox.com/events/v2";

/// Set by [`HttpSink`] on the child it spawns, and read by [`is_send_child`]
/// at the top of `main` — a mode of this binary rather than a hidden
/// subcommand, for the reason `update_check` gives.
const SEND_ENV: &str = "MAPBOX_INTERNAL_TELEMETRY_SEND";

/// The URL to send to, overriding [`ENDPOINT`] — in a build that is not a
/// production release only (see [`send_url`]). The token is added by
/// [`cli_token::send`], never carried here. The parent hands it to the child;
/// it is also the seam `tests/telemetry_events.rs` points at a loopback
/// server, and how a developer reaches staging
/// (`https://api-events-staging.tilestream.net/events/v2`).
const URL_ENV: &str = "MAPBOX_INTERNAL_TELEMETRY_URL";

/// The token the user gave this run, from [`remember_user_token`]. Kept
/// apart from the run record, which goes to history and must never hold a
/// token.
static USER_TOKEN: OnceLock<Option<String>> = OnceLock::new();

/// Turns on the delivery log: one line when an event is handed to the child
/// and one with how the send ended, in `deliveries/<UTC date>.jsonl` under
/// this module's directory. For measuring the send's success rate while
/// debugging, so it is off unless set. Counting the hand-offs, not only the
/// outcomes, is what shows a child that died before reporting — a CI
/// container torn down right after the command, say.
const DELIVERY_LOG_ENV: &str = "MAPBOX_INTERNAL_TELEMETRY_LOG";
const DELIVERY_DIR: &str = "deliveries";

/// Each attempt's budget. Nobody is waiting for the child; with every token
/// in [`cli_token::TELEMETRY`] tried it can run several of these.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Ample for one event and its envelope; anything larger on stdin is not one
/// of ours.
const MAX_EVENT_BYTES: u64 = 64 * 1024;

/// Sends each event to Mapbox Events from a detached child, so the command
/// never waits on the network.
///
/// The event goes to the child on stdin rather than argv, which is visible
/// in `ps`, and so does the user's token, which would otherwise sit in the
/// child's environment. The child sends through [`cli_token::send`], which
/// may refresh the login: the reason it runs there and not in the parent,
/// which is about to exit. One attempt per token; the event is dropped on
/// any failure. Best-effort by contract: it reports nothing and must not fail
/// the command, block its exit, or write to stdout.
struct HttpSink {
    url: String,
    user_token: Option<String>,
    profile: Option<String>,
}

/// What the parent writes to the child's stdin.
#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope {
    event: serde_json::Value,
    user_token: Option<String>,
    profile: Option<String>,
}

impl HttpSink {
    /// Detached the way `update_check::spawn_refresh` is, and for the same
    /// reasons; stdout and stderr are `/dev/null` so nothing the child does
    /// can land on the parent's terminal after it has exited.
    fn deliver(&self, line: &str) {
        let Ok(exe) = std::env::current_exe() else {
            log_delivery(line, json!({ "stage": "spawn_failed" }));
            return;
        };
        let mut command = Command::new(exe);
        // The user's token travels in the envelope; the child has no use for
        // it in its environment.
        command
            .env(SEND_ENV, "1")
            .env(URL_ENV, &self.url)
            .env_remove(auth::CLAP_TOKEN_ENV)
            .env_remove("MapboxAccessToken")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::detach::detach(&mut command);

        let Ok(event) = serde_json::from_str(line) else {
            log_delivery(line, json!({ "stage": "bad_event" }));
            return;
        };
        let envelope = Envelope {
            event,
            user_token: self.user_token.clone(),
            profile: self.profile.clone(),
        };
        let Ok(envelope) = serde_json::to_string(&envelope) else {
            log_delivery(line, json!({ "stage": "bad_event" }));
            return;
        };
        let Ok(mut child) = command.spawn() else {
            log_delivery(line, json!({ "stage": "spawn_failed" }));
            return;
        };
        // Before the write: once the child has its input it can log its
        // outcome, and the hand-off must come first in the log.
        log_delivery(line, json!({ "stage": "queued" }));
        // One event fits in a pipe buffer, so this does not wait on the
        // child. Dropping `stdin` closes it, which is the child's end of input.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(envelope.as_bytes());
        }
        // Not waited on: the parent is about to exit.
    }
}

/// Keeps the token the user gave this run for [`deliver`], resolved by
/// [`cli_token::user_token`] while the arguments are still at hand.
pub(crate) fn remember_user_token(token: Option<String>) {
    let _ = USER_TOKEN.set(token);
}

/// Hands `line` to [`HttpSink`] when [`cli_token`] has a token it could send
/// it with, and drops it otherwise. `profile` is the run's `--profile`, whose
/// login it may send with.
pub(crate) fn deliver(line: &str, profile: Option<&str>) {
    let url = send_url(is_production_build(), from_environment(URL_ENV));
    // Help, `--version` and usage errors return before the arguments are
    // parsed, so nothing was remembered; the environment is still the user's.
    let user_token = match USER_TOKEN.get() {
        Some(remembered) => remembered.clone(),
        None => from_environment(auth::CLAP_TOKEN_ENV),
    };
    if cli_token::has_token_for(cli_token::TELEMETRY, user_token.as_deref(), profile) {
        HttpSink {
            url,
            user_token,
            profile: profile.map(str::to_owned),
        }
        .deliver(line)
    } else {
        log_delivery(line, json!({ "stage": "no_token" }));
    }
}

/// Whether this process is the sender rather than a command.
pub fn is_send_child() -> bool {
    from_environment(SEND_ENV).is_some()
}

/// The whole of the child: read the event from stdin and post it.
///
/// Always succeeds, and says nothing; nobody reads its exit code. The opt-out
/// is checked again here, because this is the process that makes the request.
pub fn run_send_child() -> ExitCode {
    if telemetry::telemetry_allowed() {
        // Through `send_url` again, so a production child cannot be pointed
        // elsewhere either.
        let url = send_url(is_production_build(), from_environment(URL_ENV));
        let mut input = String::new();
        let read = std::io::stdin()
            .take(MAX_EVENT_BYTES)
            .read_to_string(&mut input);
        match (read, serde_json::from_str::<Envelope>(input.trim())) {
            (Ok(_), Ok(envelope)) => send(&url, &envelope),
            _ => log_delivery("", json!({ "stage": "bad_envelope" })),
        }
    }
    ExitCode::SUCCESS
}

/// Posts `[<line>]`, the batch shape Mapbox Events takes. The `User-Agent`
/// is the product token alone: Mapbox Events stores it in every record, and
/// the markers `http::client` adds (an `agent/` among them) must not ride
/// along.
fn send(url: &str, envelope: &Envelope) {
    let line = envelope.event.to_string();
    let outcome = match post(url, envelope) {
        Ok((status, elapsed)) => json!({
            "stage": "responded",
            "status": status,
            "elapsedMs": elapsed.as_millis() as u64,
        }),
        Err(error) => json!({ "stage": "failed", "error": error }),
    };
    log_delivery(&line, outcome);
}

/// The response's status and how long it took, or why there was none.
fn post(url: &str, envelope: &Envelope) -> Result<(u16, Duration), String> {
    let body = serde_json::to_string(&[&envelope.event]).map_err(|err| err.to_string())?;
    let client = http::client().map_err(|err| err.to_string())?;
    let started = Instant::now();
    let response = cli_token::send(
        cli_token::TELEMETRY,
        envelope.user_token.as_deref(),
        envelope.profile.as_deref(),
        || {
            client
                .post(url)
                .header(USER_AGENT, telemetry::PRODUCT_TOKEN)
                .header(CONTENT_TYPE, "application/json")
                .body(body.clone())
                .timeout(SEND_TIMEOUT)
        },
    )
    .ok_or_else(|| "no token to send with".to_string())?
    .map_err(|err| http::failure(&err))?;
    Ok((response.status().as_u16(), started.elapsed()))
}

/// Appends one line to the delivery log, when [`DELIVERY_LOG_ENV`] is set.
/// Keyed by the event's `eventId`, so a hand-off and its outcome pair up.
fn log_delivery(line: &str, mut entry: serde_json::Value) {
    if from_environment(DELIVERY_LOG_ENV).is_none() {
        return;
    }
    let Some(dir) = dir().map(|dir| dir.join(DELIVERY_DIR)) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = SystemTime::now();
    let event_id = serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|event| event.get("eventId").cloned());
    entry["eventId"] = event_id.unwrap_or(serde_json::Value::Null);
    entry["at"] = json!(now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64));
    dated_jsonl::append(&dir, &entry.to_string(), now, KEEP_DAYS);
}

fn from_environment(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Where to send: [`ENDPOINT`], or [`URL_ENV`]'s override in a build that is
/// not a production release. A release ignores the override, so an injected
/// variable cannot send a user's token to a host of its choosing.
fn send_url(production: bool, override_url: Option<String>) -> String {
    match override_url {
        Some(url) if !production => url,
        _ => ENDPOINT.to_string(),
    }
}

/// `MAPBOX_CLI_BUILD_ENV` is how the release pipeline marks a production
/// build; see `update_check`.
const fn is_production_build() -> bool {
    match option_env!("MAPBOX_CLI_BUILD_ENV") {
        Some(env) => same(env, "production"),
        None => false,
    }
}

/// `==` on `&str`, which is not available in a `const` context.
const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The contents of a state file, trimmed, if it is there.
pub(crate) fn read_state(name: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir()?.join(name)).ok()?;
    Some(text.trim().to_string())
}

/// Creates a state file, only if it does not exist yet. `false` when it
/// already did or could not be written — two first runs racing each get
/// one answer this way, rather than the second replacing the first.
pub(crate) fn create_state(name: &str, contents: &str) -> bool {
    let Some(dir) = dir() else {
        return false;
    };
    match create_private(&dir.join(name)) {
        Ok(mut file) => file.write_all(contents.as_bytes()).is_ok(),
        Err(_) => false,
    }
}

/// Replaces a state file's contents.
pub(crate) fn replace_state(name: &str, contents: &str) {
    let Some(dir) = dir() else {
        return;
    };
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    if let Ok(mut file) = create_private(&path) {
        let _ = file.write_all(contents.as_bytes());
    }
}

/// Creates `path` `0600`, failing if it already exists.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// The directory, created `0700` inside the config directory — only when
/// that already exists. Recording must not be what creates `~/.mapbox` or
/// changes its permissions: read-only commands promise to leave it alone
/// (`tests/auth_profiles.rs`, `tests/non_interactive.rs`). `None` otherwise,
/// and nothing is written.
///
/// Stricter than command history, which creates a missing config directory
/// because history has to work for someone who has never logged in; this
/// directory only holds the event's state files and the delivery log.
fn dir() -> Option<PathBuf> {
    if !auth::config_dir_path()?.is_dir() {
        return None;
    }
    dated_jsonl::private_dir(DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_build_that_is_not_production_takes_the_url_override() {
        let custom = || Some("http://127.0.0.1:1/events/v2".to_string());
        assert_eq!(send_url(false, custom()), "http://127.0.0.1:1/events/v2");
        assert_eq!(send_url(true, custom()), ENDPOINT);
        assert_eq!(send_url(false, None), ENDPOINT);
        assert_eq!(send_url(true, None), ENDPOINT);
    }
}
