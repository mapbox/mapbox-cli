//! Where the run's telemetry event goes once [`crate::telemetry_event`] has
//! built it.
//!
//! This module never decides what an event contains; it is handed one
//! finished JSON line. That keeps the privacy rules in one file and lets a
//! delivery change — a new endpoint, batching — happen without touching them.
//!
//! [`selected`] is the one place that picks the [`Sink`]. Today that is
//! [`FileSink`], which keeps events on this machine for local testing;
//! [`HttpSink`] is the interface for sending them, not implemented yet.
//!
//! It also owns the directory, `~/.mapbox/.telemetry` (or under
//! `$MAPBOX_CONFIG_DIR`), written only once the config directory exists,
//! including the two small state files the event reads — the installation
//! id and the last version seen. The dated event files are written and
//! pruned through [`crate::dated_jsonl`].

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::auth;
use crate::dated_jsonl;

const DIR: &str = ".telemetry";
/// Days of event files [`FileSink`] keeps, today included.
const KEEP_DAYS: u64 = 7;

/// Something that takes one finished event line and delivers it.
///
/// Best-effort by contract: `deliver` reports nothing and must not fail the
/// command, block its exit, or write to stdout.
pub(crate) trait Sink {
    fn deliver(&self, line: &str);
}

/// Appends each event to `<UTC date>.jsonl`, one line per run.
pub(crate) struct FileSink;

/// Sends each event to Mapbox Events. **Not implemented yet**: `deliver`
/// drops the event.
///
/// What an implementation is expected to do:
///
/// - `POST https://events.mapbox.com/events/v2?access_token=<token>` with
///   the body `[<line>]`, using a CLI-owned `pk.` token compiled into
///   release builds — never the user's.
/// - Send from a detached child so the command never waits, the way
///   `update_check` detaches its refresher, with the event on the child's
///   stdin rather than argv (argv is visible in `ps`).
/// - One attempt with a short timeout; drop the event on any failure.
/// - Send through [`crate::http::send`] with a `User-Agent` of
///   `mapbox-cli/<version>` alone: Mapbox Events stores it in every record,
///   so the `agent/` marker must not ride along.
// Not constructed until `selected` switches to it.
#[allow(dead_code)]
pub(crate) struct HttpSink;

impl Sink for FileSink {
    fn deliver(&self, line: &str) {
        if let Some(dir) = dir() {
            dated_jsonl::append(&dir, line, KEEP_DAYS);
        }
    }
}

impl Sink for HttpSink {
    fn deliver(&self, _line: &str) {}
}

/// Hands `line` to the [`selected`] sink.
pub(crate) fn deliver(line: &str) {
    selected().deliver(line);
}

/// The sink every event goes to. `HttpSink` replaces `FileSink` here once
/// it is implemented and `cli.command` is registered with Mapbox Events.
fn selected() -> &'static dyn Sink {
    &FileSink
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

/// The directory, created `0700` inside the config directory — only when
/// that already exists. Recording must not be what creates `~/.mapbox` or
/// changes its permissions: read-only commands promise to leave it alone
/// (`tests/auth_profiles.rs`, `tests/non_interactive.rs`). `None` otherwise,
/// and nothing is written.
///
/// Stricter than command history, which creates a missing config directory
/// because history has to work for someone who has never logged in; this
/// file is a stand-in until the event is sent.
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

fn dir() -> Option<PathBuf> {
    if !auth::config_dir_path()?.is_dir() {
        return None;
    }
    dated_jsonl::private_dir(DIR)
}
