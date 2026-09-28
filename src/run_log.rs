//! Diagnostic logs: for a run that [`crate::run_history`] recorded, one
//! line of detail in `~/.mapbox/logs/<UTC date>.jsonl` (or under
//! `$MAPBOX_CONFIG_DIR`), linked to the history record by its `id` and
//! shown by `mapbox history show`.
//!
//! History keeps only what is safe without anyone having asked; this keeps
//! what answers "why did that command fail": the command line, each request
//! and the error message. So it is off unless turned on — `mapbox config
//! set log on`, or `MAPBOX_LOG=1` for a session (`=0` turns it off over the
//! setting) — and it requires history: with history off it never runs,
//! rather than writing detail that nothing could lead back to.
//!
//! Kept for [`RETENTION_DAYS`] days and at most [`LIMIT_BYTES`] in total,
//! the oldest dropped first. The history record stays when its detail is
//! dropped, and says it was captured, so `history show` can tell "not
//! captured" from "no longer available". A day of detail goes when that
//! day of history does.
//!
//! It still never records a token. The command line goes through the
//! redaction `--debug` applies to `tilesets-cli`'s arguments, URLs arrive
//! with the access token already redacted by [`crate::http::send`], and
//! every string is then scrubbed of token-shaped words, because an error
//! message can quote a URL that a module other than `executor` built.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;
use serde_json::Value;

use crate::run_record::{self, Record};
use crate::{auth, config, dated_jsonl, run_history, telemetry, tilesets_cli};

const DIR: &str = "logs";
const LOG_ENV: &str = "MAPBOX_LOG";
pub(crate) const RETENTION_DAYS: u64 = run_history::RETENTION_DAYS;
pub(crate) const LIMIT_BYTES: u64 = 100 * 1024 * 1024;

/// A `--all` run can page hundreds of times; past this, requests are counted
/// rather than listed.
const MAX_REQUESTS: usize = 100;
/// Long enough for an error message or a URL, short enough that an inline
/// `--data` body does not make one line of the log most of the file.
const MAX_TEXT: usize = 2000;
const REDACTED: &str = "<redacted>";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    id: String,
    time: String,
    version: &'static str,
    argv: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invocation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<u32>,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth: Option<Auth>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    requests: Vec<Request>,
    #[serde(skip_serializing_if = "is_zero")]
    requests_not_listed: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    more_pages: bool,
    stdout_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_step: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    update_notice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Failure>,
}

#[derive(Serialize)]
struct Auth {
    source: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    method: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct Failure {
    code: String,
    message: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Appends the run's line. The caller has checked [`enabled`] and that
/// history recorded the run.
pub(crate) fn write(record: &Record) {
    let Ok(text) = serde_json::to_string(&line(record)) else {
        return;
    };
    if let Some(dir) = dated_jsonl::private_dir(DIR) {
        dated_jsonl::append(&dir, &text, RETENTION_DAYS);
        dated_jsonl::shed(&dir, LIMIT_BYTES);
    }
}

/// Whether this run writes diagnostics: never with history off, otherwise
/// `MAPBOX_LOG` when it is set and the persisted setting when it is not.
pub(crate) fn enabled() -> bool {
    run_history::enabled() && telemetry::env_switch(LOG_ENV).unwrap_or_else(config::log_enabled)
}

/// Where the logs live, without creating them.
fn dir_path() -> Option<PathBuf> {
    Some(auth::config_dir_path()?.join(DIR))
}

/// Deletes each day of detail whose day of history has expired or gone.
/// Run on every run that finishes, logging on or off, so detail never
/// outlives the record it belongs to. Creates nothing.
pub(crate) fn expire_with_history() {
    let Some(dir) = dir_path().filter(|dir| dir.is_dir()) else {
        return;
    };
    let oldest = dated_jsonl::oldest_kept(RETENTION_DAYS);
    dated_jsonl::prune_where(&dir, |date| {
        date >= oldest.as_str() && run_history::has_day(date)
    });
}

/// The detail logged for the run `id` that history recorded at `time`.
/// Only that day's file is read — and the next, for a run that finished
/// across midnight.
pub(crate) fn find(id: &str, time: &str) -> Option<Value> {
    let dir = dir_path()?;
    let date = time.get(..10)?;
    let next = dated_jsonl::next_date(date);
    [Some(date.to_string()), next]
        .into_iter()
        .flatten()
        .flat_map(|day| dated_jsonl::read_day(&dir, &day))
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
}

fn line(record: &Record) -> Line {
    Line {
        id: record.id.clone(),
        time: dated_jsonl::timestamp(SystemTime::now()),
        version: env!("CARGO_PKG_VERSION"),
        argv: tilesets_cli::redacted_argv(&record.argv)
            .iter()
            .map(|arg| clean(arg))
            .collect(),
        command: record.command.clone(),
        invocation: record.invocation.map(run_record::Invocation::as_str),
        exit_code: record.exit_code,
        duration_ms: record.duration.as_millis() as u64,
        auth: record.token.as_ref().map(|token| Auth {
            source: token.source.as_str(),
            kind: token.kind,
            account: token.account.as_deref().map(clean),
        }),
        requests: record
            .requests
            .iter()
            .take(MAX_REQUESTS)
            .map(request)
            .collect(),
        requests_not_listed: record.requests.len().saturating_sub(MAX_REQUESTS),
        more_pages: record.more_pages,
        stdout_bytes: record.stdout_bytes,
        auth_step: record.auth_step,
        update_notice: record.update_notice.as_deref().map(clean),
        error: record.error.as_ref().map(|failure| Failure {
            code: clean(&failure.code),
            message: clean(&failure.message),
        }),
    }
}

fn request(request: &run_record::Request) -> Request {
    Request {
        method: request.method.clone(),
        url: clean(&request.url),
        status: request.status,
        request_id: request.request_id.as_deref().map(clean),
        duration_ms: request.elapsed.as_millis() as u64,
        error: request.error.as_deref().map(clean),
    }
}

/// Scrubbed of token-shaped words, then clipped to [`MAX_TEXT`].
fn clean(text: &str) -> String {
    let scrubbed = scrub_tokens(text);
    match scrubbed.char_indices().nth(MAX_TEXT) {
        Some((end, _)) => format!("{}…", &scrubbed[..end]),
        None => scrubbed,
    }
}

/// `text` with every word that looks like a Mapbox token replaced. A word
/// here is a run of the characters a token is made of, so a token inside a
/// URL, after `=`, or in quotes is still found.
fn scrub_tokens(text: &str) -> String {
    let is_token_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(is_token_char) {
        out.push_str(&rest[..start]);
        let word_len = rest[start..]
            .find(|c: char| !is_token_char(c))
            .unwrap_or(rest.len() - start);
        let word = &rest[start..start + word_len];
        if tilesets_cli::looks_like_a_token(word) {
            out.push_str(REDACTED);
        } else {
            out.push_str(word);
        }
        rest = &rest[start + word_len..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "pk.eyJ1IjoiZXhhbXBsZS11c2VyIiwiYSI6IngifQ.SIGNATURE-NOT-FOR-LOGS";

    #[test]
    fn tokens_are_scrubbed_wherever_they_sit() {
        for (text, expected) in [
            (
                format!("GET https://api.mapbox.com/x?access_token={TOKEN}&a=1"),
                "GET https://api.mapbox.com/x?access_token=<redacted>&a=1".to_string(),
            ),
            (
                format!("token \"{TOKEN}\"."),
                "token \"<redacted>\".".to_string(),
            ),
            (TOKEN.to_string(), REDACTED.to_string()),
        ] {
            assert_eq!(scrub_tokens(&text), expected);
        }
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for text in [
            "",
            "styles get my-style --username pk",
            "pk.short",
            "No such style: ckabc123.",
            "héllo wörld",
        ] {
            assert_eq!(scrub_tokens(text), text);
        }
    }

    #[test]
    fn long_text_is_clipped_on_a_character_boundary() {
        let text = "é".repeat(MAX_TEXT + 10);
        let clipped = clean(&text);
        assert_eq!(clipped.chars().count(), MAX_TEXT + 1);
        assert!(clipped.ends_with('…'));
        assert_eq!(clean("short"), "short");
    }
}
