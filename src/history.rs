//! `mapbox history` — the runs [`crate::run_history`] recorded.
//!
//! `list` is one line per run, newest first; `show` is everything recorded
//! about one run, the newest when no id is given. An id can be shortened to
//! any prefix that names one run, the way `list` prints them.
//!
//! Reads history and nothing else: no token, no request, and nothing
//! created on disk. It is not itself recorded.

use anyhow::Result;
use clap::{value_parser, Arg, ArgMatches, Command};
use serde_json::Value;

use crate::output::{self, CliError, Mode};
use crate::remedy::Remedy;
use crate::run_history;

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

    let text = entries
        .iter()
        .map(|entry| {
            format!(
                "{}  {}  {:>4}  {}",
                short_id(entry),
                field(entry, "time"),
                exit_code(entry),
                command_line(entry)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
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
    output::emit(mode, &detail(&entry), entry)
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
    use serde_json::json;

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
