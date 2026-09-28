//! Command history: one line of execution metadata per run in
//! `~/.mapbox/history/<UTC date>.jsonl` (or under `$MAPBOX_CONFIG_DIR`),
//! kept for [`RETENTION_DAYS`] days and at most [`LIMIT_BYTES`], oldest
//! first, and read back by `mapbox history`.
//!
//! On by default, so it keeps only what is safe to keep without anyone
//! having asked: the command path from the command tree (`search forward`,
//! never what was typed after it), how the run ended, how long it took and
//! the request ids support can look up. No argument values, URLs, error
//! messages or account — arguments carry search terms, file paths and ids.
//!
//! `mapbox config set history off` turns it off, `MAPBOX_HISTORY=0` or `=1`
//! for a session over the setting. Turned off, nothing is created on disk.
//! Turned on, the config directory is created if it is missing, so history
//! works the same for someone who only ever set `MAPBOX_ACCESS_TOKEN`.
//!
//! Not recorded:
//! - `--help` and `--version`, which answer a question rather than run a
//!   command;
//! - `history` itself, which would push out what it was reading;
//! - a run under `sudo`, whose files would belong to root inside the
//!   user's home and stop the user's own runs appending to them — the
//!   failure AWS CLI shipped in 2.33.9 (aws/aws-cli#10031);
//! - `completion`, which [`crate::run_record`] already skips.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;

use crate::run_record::{Invocation, Record};
use crate::{auth, config, dated_jsonl, history, telemetry};

const DIR: &str = "history";
const HISTORY_ENV: &str = "MAPBOX_HISTORY";
pub(crate) const RETENTION_DAYS: u64 = 30;
/// Tens of thousands of runs: a script calling this in a loop must not fill
/// the disk before thirty days are up.
const LIMIT_BYTES: u64 = 10 * 1024 * 1024;
/// The last few are enough to hand to support; a paginated run can make
/// hundreds of requests.
const MAX_REQUEST_IDS: usize = 5;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    id: String,
    time: String,
    version: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invocation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    duration_ms: u64,
    #[serde(skip_serializing_if = "is_zero")]
    request_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    request_ids: Vec<String>,
    /// Whether a diagnostic log was written for this run. Kept with the
    /// record so that detail dropped later reads as "no longer available",
    /// not "never captured".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    diagnostics_captured: bool,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Whether this run gets a history line: history is on and the run is one
/// it records.
pub(crate) fn will_record(record: &Record) -> bool {
    enabled() && recorded(record, std::env::var_os("SUDO_USER").is_some())
}

/// Appends the run's line. The caller has checked [`will_record`];
/// `diagnostics` says whether a diagnostic log is written for it too.
pub(crate) fn write(record: &Record, diagnostics: bool, at: SystemTime) {
    let Ok(text) = serde_json::to_string(&line(record, diagnostics, at)) else {
        return;
    };
    if let Some(dir) = dated_jsonl::private_dir(DIR) {
        dated_jsonl::append(&dir, &text, at, RETENTION_DAYS);
        dated_jsonl::shed(&dir, LIMIT_BYTES);
    }
}

/// Whether this run records history: `MAPBOX_HISTORY` when it is set, the
/// persisted setting otherwise.
pub(crate) fn enabled() -> bool {
    telemetry::env_switch(HISTORY_ENV).unwrap_or_else(config::history_enabled)
}

fn recorded(record: &Record, under_sudo: bool) -> bool {
    !under_sudo
        && !matches!(
            record.invocation,
            Some(Invocation::Help | Invocation::Version)
        )
        && record.command.first().map(String::as_str) != Some(history::COMMAND)
}

fn line(record: &Record, diagnostics: bool, at: SystemTime) -> Line {
    let ids: Vec<String> = record
        .requests
        .iter()
        .filter_map(|request| request.request_id.clone())
        .collect();
    Line {
        id: record.id.clone(),
        time: dated_jsonl::timestamp(at),
        version: env!("CARGO_PKG_VERSION"),
        command: record.command.clone(),
        invocation: record.invocation.map(Invocation::as_str),
        exit_code: record.exit_code,
        error_code: record.error.as_ref().map(|error| error.code.clone()),
        duration_ms: record.duration.as_millis() as u64,
        request_count: record.requests.len(),
        request_ids: ids[ids.len().saturating_sub(MAX_REQUEST_IDS)..].to_vec(),
        diagnostics_captured: diagnostics,
    }
}

/// Where history lives, without creating it.
fn dir_path() -> Option<PathBuf> {
    Some(auth::config_dir_path()?.join(DIR))
}

/// Whether history still has a file for `date` (`YYYY-MM-DD`).
pub(crate) fn has_day(date: &str) -> bool {
    dir_path().is_some_and(|dir| dir.join(format!("{date}.jsonl")).is_file())
}

/// Every run in history, oldest first, skipping any line that doesn't parse.
pub(crate) fn entries() -> Vec<serde_json::Value> {
    let Some(dir) = dir_path() else {
        return vec![];
    };
    dated_jsonl::read_all(&dir)
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(command: &[&str], invocation: Invocation) -> Record {
        let mut record = Record::default();
        record.command = command.iter().map(|c| c.to_string()).collect();
        record.invocation = Some(invocation);
        record
    }

    #[test]
    fn help_version_history_and_sudo_are_not_recorded() {
        let run = record(&["styles", "list"], Invocation::Execute);
        assert!(recorded(&run, false));
        assert!(!recorded(&run, true), "under sudo");
        assert!(!recorded(&record(&["styles"], Invocation::Help), false));
        assert!(!recorded(&record(&[], Invocation::Version), false));
        assert!(!recorded(
            &record(&["history", "list"], Invocation::Execute),
            false
        ));
    }

    #[test]
    fn a_line_keeps_the_command_path_and_no_argument() {
        let mut run = record(&["search", "forward"], Invocation::Execute);
        run.argv = ["search", "forward", "--q", "1600 Pennsylvania Ave"]
            .iter()
            .map(std::ffi::OsString::from)
            .collect();
        let text = serde_json::to_string(&line(&run, false, SystemTime::now())).unwrap();
        assert!(text.contains(r#""command":["search","forward"]"#), "{text}");
        assert!(!text.contains("Pennsylvania"), "{text}");
    }
}
