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
//! `$MAPBOX_CONFIG_DIR`), including the two small state files the event
//! reads — the installation id and the last version seen — so that every
//! write and delete under it is in this file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::auth;

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
        let Some(dir) = dir() else {
            return;
        };
        let (today, _) = utc_date(now_secs());
        let path = dir.join(format!("{today}.jsonl"));
        let is_new_day = !path.exists();
        // One `write` per line. `O_APPEND` places each one at the end, but a
        // line past the platform's atomic-write size (an event near the
        // schema's bounds) is not guaranteed to stay whole against a
        // parallel run writing at the same moment.
        if let Ok(mut file) = open_private(&path, true) {
            let _ = file.write_all(format!("{line}\n").as_bytes());
        }
        if is_new_day {
            prune(&dir, now_secs());
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
    match open_private(&dir.join(name), false) {
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
    if let Ok(mut file) = open_private(&path, false) {
        let _ = file.write_all(contents.as_bytes());
    }
}

/// The directory, created `0700` inside a config directory `auth` has
/// created and hardened, as it would for credentials. `None` when it
/// cannot be.
fn dir() -> Option<PathBuf> {
    auth::config_dir().ok()?;
    let dir = auth::config_dir_path()?.join(DIR);
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

/// Opens `path` `0600`: for appending, or created new and failing if it
/// already exists.
fn open_private(path: &Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    if append {
        options.append(true).create(true);
    } else {
        options.write(true).create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Deletes event files outside the last [`KEEP_DAYS`] days. Only names that
/// are exactly `YYYY-MM-DD.jsonl` are considered, and only inside the
/// telemetry directory itself, so nothing else there can be matched.
fn prune(dir: &Path, now: u64) {
    let (oldest_kept, _) = utc_date(now.saturating_sub((KEEP_DAYS - 1) * 86_400));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(date) = event_file_date(&name) {
            if date < oldest_kept.as_str() {
                let _ = std::fs::remove_file(dir.join(&name));
            }
        }
    }
}

fn event_file_date(name: &str) -> Option<&str> {
    let date = name.strip_suffix(".jsonl")?;
    let shape = date.len() == 10
        && date.char_indices().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        });
    shape.then_some(date)
}

/// `YYYY-MM-DD` and the seconds into that day, in UTC.
pub(crate) fn utc_date(unix_secs: u64) -> (String, u64) {
    let days = (unix_secs / 86_400) as i64;
    let (y, m, d) = crate::account_usage::civil_from_days(days);
    (format!("{y:04}-{m:02}-{d:02}"), unix_secs % 86_400)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dated_event_files_are_pruned() {
        assert_eq!(event_file_date("2026-09-24.jsonl"), Some("2026-09-24"));
        for name in [
            "user-id",
            "last-version",
            "2026-09-24.json",
            "notes.jsonl",
            "2026-9-24.jsonl",
        ] {
            assert_eq!(event_file_date(name), None, "{name}");
        }
    }

    #[test]
    fn prune_keeps_seven_days_and_nothing_else_is_touched() {
        let dir =
            std::env::temp_dir().join(format!("mapbox-telemetry-prune-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "2026-09-17.jsonl",
            "2026-09-18.jsonl",
            "2026-09-24.jsonl",
            "user-id",
            "2026-09-01.txt",
        ] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        // 2026-09-24T12:00:00Z: 09-18 through 09-24 is seven days.
        prune(&dir, 1_790_251_200);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            left,
            [
                "2026-09-01.txt",
                "2026-09-18.jsonl",
                "2026-09-24.jsonl",
                "user-id"
            ]
        );
    }
}
