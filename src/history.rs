//! `mapbox history` — the runs [`crate::run_history`] recorded.
//!
//! `list` is one line per run, newest first; `show` is everything recorded
//! about one run, the newest when no id is given, with its diagnostic log
//! when one was captured and is still kept ([`crate::run_log`]). An id can
//! be shortened to any prefix that names one run, the way `list` prints
//! them. There is no separate command for the logs: history is the one way
//! in to both.
//!
//! Reads history and nothing else: no token, no request, and nothing
//! created on disk. It is not itself recorded.

use anyhow::Result;
use clap::{value_parser, Arg, ArgMatches, Command};
use serde_json::{json, Value};

use crate::output::{self, CliError, Mode};
use crate::remedy::Remedy;
use crate::{run_history, run_log};

pub const COMMAND: &str = "history";

const DEFAULT_LIMIT: usize = 20;
/// How much of an id `list` prints: enough to tell runs apart, short enough
/// to type back into `show`.
const SHORT_ID: usize = 8;

pub fn command() -> Command {
    Command::new(COMMAND)
        .about("List and show recent command runs")
        .long_about(
            "List and show recent command runs: which command ran, how it ended and \
             how long it took, kept for 30 days on this machine. Argument values are \
             never recorded. Turn it off with `mapbox config set history off`.",
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("list")
                .about("List the most recent runs, newest first")
                .arg(
                    Arg::new("limit")
                        .long("limit")
                        .value_parser(value_parser!(usize))
                        .default_value(DEFAULT_LIMIT.to_string())
                        .help("How many runs to list; 0 lists every run recorded"),
                ),
        )
        .subcommand(
            Command::new("show")
                .about("Show everything recorded about one run")
                .arg(
                    Arg::new("id")
                        .help("The run's id, or a prefix of it; the newest run when left out"),
                ),
        )
}

pub fn list(matches: &ArgMatches, mode: Mode) -> Result<()> {
    let limit = *matches.get_one::<usize>("limit").expect("has a default");
    let mut entries = run_history::entries();
    entries.reverse();
    if limit > 0 {
        entries.truncate(limit);
    }
    if entries.is_empty() {
        hint_when_off();
    }

    let rows = entries.iter().map(|entry| {
        let mut line = format!(
            "{:SHORT_ID$}  {:24}  {:>4}  {}",
            short_id(entry),
            field(entry, "time"),
            exit_code(entry),
            command_line(entry)
        );
        if captured(entry) {
            line.push_str("  [log]");
        }
        line
    });
    let text = if entries.is_empty() {
        String::new()
    } else {
        std::iter::once(format!(
            "{:SHORT_ID$}  {:24}  {:>4}  COMMAND",
            "ID", "TIME", "EXIT"
        ))
        .chain(rows)
        .collect::<Vec<_>>()
        .join("\n")
    };
    let json = entries
        .iter()
        .map(|entry| {
            let mut summary = serde_json::Map::new();
            for key in [
                "id",
                "time",
                "command",
                "exitCode",
                "errorCode",
                "durationMs",
                "diagnosticsCaptured",
            ] {
                if let Some(value) = entry.get(key) {
                    summary.insert(key.to_string(), value.clone());
                }
            }
            Value::Object(summary)
        })
        .collect();

    output::emit(mode, &text, Value::Array(json))
}

pub fn show(matches: &ArgMatches, mode: Mode) -> Result<()> {
    let entries = run_history::entries();
    let entry = match matches.get_one::<String>("id") {
        None => entries.last().cloned().ok_or_else(|| {
            hint_when_off();
            CliError::new("history_empty", "No runs have been recorded yet.")
        })?,
        Some(prefix) => find(&entries, prefix)?,
    };
    let diagnostics = diagnostics(&entry);
    let mut text = detail(&entry);
    text.push('\n');
    text.push_str(&diagnostics_detail(&diagnostics));
    let mut json = entry;
    if let Some(object) = json.as_object_mut() {
        object.remove("diagnosticsCaptured");
        object.insert("diagnostics".to_string(), diagnostics.json());
    }
    output::emit(mode, &text, json)
}

/// What became of a run's diagnostic log.
enum Diagnostics {
    /// Logging was off for the run.
    NotCaptured,
    /// Captured, then expired or dropped to stay under the size limit.
    Unavailable,
    Captured(Value),
}

impl Diagnostics {
    /// `status` is a machine-readable value: `not_captured`, `unavailable`
    /// or `captured`.
    fn json(&self) -> Value {
        match self {
            Diagnostics::NotCaptured => json!({ "status": "not_captured" }),
            Diagnostics::Unavailable => json!({ "status": "unavailable" }),
            Diagnostics::Captured(log) => json!({ "status": "captured", "log": log }),
        }
    }
}

fn captured(entry: &Value) -> bool {
    entry.get("diagnosticsCaptured").and_then(Value::as_bool) == Some(true)
}

fn diagnostics(entry: &Value) -> Diagnostics {
    if !captured(entry) {
        return Diagnostics::NotCaptured;
    }
    match run_log::find(field(entry, "id"), field(entry, "time")) {
        Some(mut log) => {
            // Already in the record it belongs to.
            if let Some(object) = log.as_object_mut() {
                for key in ["id", "time"] {
                    object.remove(key);
                }
            }
            Diagnostics::Captured(log)
        }
        None => Diagnostics::Unavailable,
    }
}

fn diagnostics_detail(diagnostics: &Diagnostics) -> String {
    let log = match diagnostics {
        Diagnostics::NotCaptured => {
            return "Log       not captured: diagnostic logging was off for this run \
                    (`mapbox config set log on` captures the next ones)"
                .to_string()
        }
        Diagnostics::Unavailable => {
            return "Log       no longer available: it expired or was removed to keep \
                    diagnostic logs under 100 MB"
                .to_string()
        }
        Diagnostics::Captured(log) => log,
    };
    let argv: Vec<&str> = log["argv"]
        .as_array()
        .map(|args| args.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = vec![format!("Log       mapbox {}", argv.join(" "))];
    if let Some(auth) = log.get("auth") {
        let mut line = format!("  token {}, {}", field(auth, "source"), field(auth, "type"));
        if let Some(account) = auth.get("account").and_then(Value::as_str) {
            line.push_str(&format!(", account {account}"));
        }
        out.push(line);
    }
    if let Some(step) = log.get("authStep").and_then(Value::as_str) {
        out.push(format!("  auth step {step}"));
    }
    for request in log["requests"].as_array().into_iter().flatten() {
        let outcome = match request.get("status").and_then(Value::as_u64) {
            Some(status) => status.to_string(),
            None => field(request, "error").to_string(),
        };
        let mut line = format!(
            "  {} {} -> {} in {} ms",
            field(request, "method"),
            field(request, "url"),
            outcome,
            request["durationMs"].as_u64().unwrap_or(0)
        );
        if let Some(id) = request.get("requestId").and_then(Value::as_str) {
            line.push_str(&format!(" (request id {id})"));
        }
        out.push(line);
    }
    if let Some(n) = log.get("requestsNotListed").and_then(Value::as_u64) {
        out.push(format!("  and {n} more requests not listed"));
    }
    if log.get("morePages").and_then(Value::as_bool) == Some(true) {
        out.push("  stopped with pages left".to_string());
    }
    out.push(format!(
        "  {} bytes to stdout",
        log["stdoutBytes"].as_u64().unwrap_or(0)
    ));
    if let Some(version) = log.get("updateNotice").and_then(Value::as_str) {
        out.push(format!("  update notice for {version}"));
    }
    if let Some(error) = log.get("error") {
        out.push(format!(
            "  error {}: {}",
            field(error, "code"),
            field(error, "message")
        ));
    }
    out.join("\n")
}

/// The one run whose id starts with `prefix`.
fn find(entries: &[Value], prefix: &str) -> Result<Value> {
    let matching: Vec<&Value> = entries
        .iter()
        .filter(|entry| !prefix.is_empty() && field(entry, "id").starts_with(prefix))
        .collect();
    match matching.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(CliError::new(
            "history_not_found",
            format!("No recorded run has an id starting with `{prefix}`."),
        )
        .with_remedy(Remedy::default().with_action(Some("mapbox history list".to_string())))
        .into()),
        many => Err(CliError::new(
            "history_ambiguous_id",
            format!(
                "`{prefix}` starts {} run ids; give more of the id.",
                many.len()
            ),
        )
        .into()),
    }
}

/// A person-readable view of one run. JSON gets the line as it was recorded.
fn detail(entry: &Value) -> String {
    let mut out = vec![
        format!("Run       {}", field(entry, "id")),
        format!(
            "Time      {} (mapbox {})",
            field(entry, "time"),
            field(entry, "version")
        ),
        format!("Command   {}", command_line(entry)),
        format!(
            "Exit      {} after {} ms",
            exit_code(entry),
            entry["durationMs"].as_u64().unwrap_or(0)
        ),
    ];
    if let Some(code) = entry.get("errorCode").and_then(Value::as_str) {
        out.push(format!("Error     {code}"));
    }
    if let Some(count) = entry.get("requestCount").and_then(Value::as_u64) {
        out.push(format!("Requests  {count}"));
    }
    for id in entry["requestIds"].as_array().into_iter().flatten() {
        if let Some(id) = id.as_str() {
            out.push(format!("  request id {id}"));
        }
    }
    out.join("\n")
}

/// Says why there is nothing to read, on stderr, when history is off.
fn hint_when_off() {
    if !run_history::enabled() {
        output::progress(
            "History is off. Turn it on with `mapbox config set history on`, \
             or `MAPBOX_HISTORY=1` for this shell.",
        );
    }
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("-")
}

fn short_id(entry: &Value) -> String {
    field(entry, "id").chars().take(SHORT_ID).collect()
}

fn exit_code(entry: &Value) -> String {
    entry
        .get("exitCode")
        .and_then(Value::as_u64)
        .map_or_else(|| "-".to_string(), |code| code.to_string())
}

/// `mapbox` and the command path: what ran, never what was typed after it.
fn command_line(entry: &Value) -> String {
    let path: Vec<&str> = entry["command"]
        .as_array()
        .map(|words| words.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if path.is_empty() {
        "mapbox".to_string()
    } else {
        format!("mapbox {}", path.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs() -> Vec<Value> {
        vec![
            json!({ "id": "abc12345-0000-4000-8000-000000000001" }),
            json!({ "id": "abc19999-0000-4000-8000-000000000002" }),
            json!({ "id": "def00000-0000-4000-8000-000000000003" }),
        ]
    }

    #[test]
    fn a_prefix_that_names_one_run_finds_it() {
        let found = find(&runs(), "abc1234").unwrap();
        assert_eq!(field(&found, "id"), "abc12345-0000-4000-8000-000000000001");
    }

    #[test]
    fn a_prefix_that_names_several_or_none_is_an_error() {
        let code = |prefix: &str| {
            find(&runs(), prefix)
                .unwrap_err()
                .downcast::<CliError>()
                .unwrap()
                .code
        };
        assert_eq!(code("abc1"), "history_ambiguous_id");
        assert_eq!(code("fff"), "history_not_found");
        assert_eq!(code(""), "history_not_found");
    }
}
