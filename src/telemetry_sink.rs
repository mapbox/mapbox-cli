//! Where the run's telemetry event goes once [`crate::telemetry_event`] has
//! built it.
//!
//! This module never decides what an event contains; it is handed one
//! finished JSON line. That keeps the privacy rules in one file and lets a
//! delivery change — a new endpoint, batching — happen without touching them.
//!
//! [`deliver`] hands the event to [`HttpSink`], which sends it to Mapbox
//! Events, when [`crate::cli_token`] has a CLI token for it. Never the
//! user's token; that module explains why. Otherwise the event is dropped
//! rather than saved, because nothing would ever send a saved copy. The state
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

/// Production Mapbox Events, for every build. The CLI's token is a
/// production token on every channel, so a build from source sends where a
/// release does; staging would not accept the token.
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

/// Turns on the delivery log: one line when an event is handed to the child
/// and one with how the send ended, in `deliveries/<UTC date>.jsonl` under
/// this module's directory. For measuring the send's success rate while
/// debugging, so it is off unless set. Counting the hand-offs, not only the
/// outcomes, is what shows a child that died before reporting — a CI
/// container torn down right after the command, say.
const DELIVERY_LOG_ENV: &str = "MAPBOX_INTERNAL_TELEMETRY_LOG";
const DELIVERY_DIR: &str = "deliveries";

/// Bounds the POST only. The token fetch and the Tokens API check around it
/// keep their own timeouts, so the child can run several times this; nobody
/// waits for it.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Ample for one event; anything larger on stdin is not one of ours.
const MAX_EVENT_BYTES: u64 = 64 * 1024;

/// Sends each event to Mapbox Events from a detached child, so the command
/// never waits on the network.
///
/// The event goes to the child on stdin rather than argv, which is visible
/// in `ps`. The child sends through [`cli_token::send`], which may check a
/// refused token and fetch a new one; that runs in the child because the
/// parent is about to exit. One attempt; the event is dropped on
/// any failure. Best-effort by contract: it reports nothing and must not fail
/// the command, block its exit, or write to stdout.
struct HttpSink {
    url: String,
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
        // The child never sends with the user's token, so don't pass it one.
        command
            .env(SEND_ENV, "1")
            .env(URL_ENV, &self.url)
            .env_remove(auth::CLAP_TOKEN_ENV)
            .env_remove("MapboxAccessToken")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::detach::detach(&mut command);

        if serde_json::from_str::<serde_json::Value>(line).is_err() {
            log_delivery(line, json!({ "stage": "bad_event" }));
            return;
        }
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
            let _ = stdin.write_all(line.as_bytes());
        }
        // Not waited on: the parent is about to exit.
    }
}

pub(crate) fn deliver(line: &str) {
    match cli_token::no_token_for(&cli_token::TELEMETRY_CALLER) {
        None => HttpSink { url: send_url() }.deliver(line),
        Some(reason) => log_delivery(line, json!({ "stage": "no_token", "reason": reason })),
    }
}

/// Whether this process is the sender rather than a command.
pub fn is_send_child() -> bool {
    from_environment(SEND_ENV).is_some()
}

/// The whole of the child: read the event from stdin and post it.
///
/// Always succeeds, and says nothing; nobody reads its exit code. Both
/// opt-outs are checked again here, because this is the process that makes
/// the request.
pub fn run_send_child() -> ExitCode {
    if telemetry::telemetry_allowed() {
        // Through `send_url` again, so a production child cannot be pointed
        // elsewhere either.
        let url = send_url();
        let mut input = String::new();
        let read = std::io::stdin()
            .take(MAX_EVENT_BYTES)
            .read_to_string(&mut input);
        match (
            read,
            serde_json::from_str::<serde_json::Value>(input.trim()),
        ) {
            (Ok(_), Ok(event)) => send(&url, &event),
            _ => log_delivery("", json!({ "stage": "bad_envelope" })),
        }
    }
    ExitCode::SUCCESS
}

/// Posts `[<line>]`, the batch shape Mapbox Events takes. The `User-Agent`
/// is the product token alone: Mapbox Events stores it in every record, and
/// the markers `http::client` adds (an `agent/` among them) must not ride
/// along.
fn send(url: &str, event: &serde_json::Value) {
    let outcome = match post(url, event) {
        Ok((status, elapsed)) => json!({
            "stage": "responded",
            "status": status,
            "elapsedMs": elapsed.as_millis() as u64,
        }),
        Err(error) => json!({ "stage": "failed", "error": error }),
    };
    log_delivery(&event.to_string(), outcome);
}

/// The response's status and how long it took, or why there was none.
fn post(url: &str, event: &serde_json::Value) -> Result<(u16, Duration), String> {
    let body = serde_json::to_string(&[event]).map_err(|err| err.to_string())?;
    let client = http::client().map_err(|err| err.to_string())?;
    // Reset when the request is built, so the time covers only the POST, not
    // a token fetch before it.
    let started = std::cell::Cell::new(Instant::now());
    let response = cli_token::send(&cli_token::TELEMETRY_CALLER, || {
        started.set(Instant::now());
        client
            .post(url)
            .header(USER_AGENT, telemetry::PRODUCT_TOKEN)
            .header(CONTENT_TYPE, "application/json")
            .body(body.clone())
            .timeout(SEND_TIMEOUT)
    })
    .ok_or_else(|| "no token to send with".to_string())?
    .map_err(|err| http::failure(&err))?;
    Ok((response.status().as_u16(), started.get().elapsed()))
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
/// not a production release (see [`http::url_override`]).
fn send_url() -> String {
    http::url_override(URL_ENV).unwrap_or_else(|| ENDPOINT.to_string())
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
