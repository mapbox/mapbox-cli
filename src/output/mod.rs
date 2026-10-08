//! How a command's result reaches its caller.
//!
//! Two audiences read this CLI's output and they want opposite things: a
//! person at a terminal wants prose, a script or an agent wants something
//! `jq` can parse. `--output` picks between them, and its default — `auto` —
//! decides by asking whether stdout is a terminal.
//!
//! The split is by *stream*, not by mode: stdout carries the result and
//! nothing else, stderr carries progress, warnings and errors. That holds in
//! both modes, so `mapbox ... > out.json` is always a clean document and
//! never has a "Waiting for authorization..." line wedged into it.
//!
//! Errors go to stderr in both modes, JSON-shaped under `json`. Keeping them
//! off stdout means a consumer never has to tell a result from a failure by
//! inspecting it — the exit code already says which it got.
//!
//! Split by what each part answers: [`error`] is how a failure is shaped
//! and printed, [`render`] turns a response into a table or a field list,
//! [`style`] is terminal color, and [`banner`] is the line a run opens with.
//! This file is the entry point every other module calls, and the only one
//! here that writes to stdout.

pub mod banner;
mod error;
mod render;
pub mod style;
pub mod theme;

pub use error::{emit_error, CliError};
pub use render::field_lines;
pub(crate) use render::LINE;

use std::ffi::OsString;
use std::io::{IsTerminal, Write};

use anyhow::Result;
use clap::parser::ValueSource;
use clap::ArgMatches;
use serde_json::Value;

use render::{list_rendering, render_human, styled};

/// `--id`'s arg id. Not `id`: see the comment where it is declared.
pub const FILTER_ARG: &str = "filter-id";

/// Picks one row out of a list response.
///
/// The Tokens API has no way to fetch one token by id, and neither do
/// several other listings — but the row is right there in the response the
/// listing already returned. Filtering it here is the difference between
/// "the data exists somewhere" and "you can see it", and it needs no query
/// language and no `jq` on the machine.
///
/// Matches on `id`, or on `name` when the rows are keyed that way instead.
/// A miss is an error rather than an empty result: asking for one row and
/// silently getting none reads like the row exists and is empty.
pub fn pick_row(value: &Value, wanted: &str) -> Result<Value> {
    let rows = value.as_array().ok_or_else(|| {
        CliError::new(
            "not_a_list",
            "`--id` only applies to a command that returns a list.",
        )
    })?;

    for key in ["id", "name"] {
        let found = rows
            .iter()
            .find(|row| row.get(key).and_then(Value::as_str) == Some(wanted));
        if let Some(row) = found {
            return Ok(row.clone());
        }
    }

    Err(CliError::new("not_found", format!("No row has the id `{wanted}`.")).into())
}

/// The `--output` arg's id, and its three accepted values.
pub const ARG: &str = "output";
pub const AUTO: &str = "auto";
pub const TEXT: &str = "text";
pub const JSON: &str = "json";
pub const ENV: &str = "MAPBOX_OUTPUT";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Text,
    Json {
        /// Indented, because a person is reading it. JSON asked for at a
        /// terminal is being looked at; JSON in a pipe is being parsed, and
        /// there one document per line is the more useful promise. The bytes
        /// differ, the document does not.
        pretty: bool,
    },
}

impl Mode {
    /// Resolves a requested value against the terminal-ness of stdout.
    ///
    /// Takes that as an argument rather than probing: it is the only input
    /// that cannot be arranged in a test, and every interesting case here is
    /// a combination of the two.
    ///
    /// An unrecognized `requested` is treated as `auto`. Clap rejects those
    /// before they reach us for both the flag and `MAPBOX_OUTPUT`; the one
    /// caller that can pass one is [`Mode::early`], which reads argv and the
    /// environment itself, before clap has had a chance to complain.
    pub fn resolve(requested: &str, stdout_is_terminal: bool) -> Self {
        match requested {
            JSON => Mode::Json {
                pretty: stdout_is_terminal,
            },
            TEXT => Mode::Text,
            _ if stdout_is_terminal => Mode::Text,
            _ => Mode::Json { pretty: false },
        }
    }

    /// The mode for a parsed command line.
    ///
    /// `MAPBOX_OUTPUT` is read here rather than declared as clap's `.env()`
    /// on the arg, because clap would *validate* it: `export MAPBOX_OUTPUT=`
    /// is a common way to clear a variable, and under `.env()` it made every
    /// command — including the ones needed to recover — fail with a usage
    /// error. An unusable value earns a warning and `auto`, never a dead CLI.
    pub fn from_matches(matches: &ArgMatches) -> Self {
        let typed = matches.value_source(ARG) == Some(ValueSource::CommandLine);
        let requested = if typed {
            matches
                .get_one::<String>(ARG)
                .cloned()
                .unwrap_or_else(|| AUTO.to_string())
        } else {
            environment_request().unwrap_or_else(|| AUTO.to_string())
        };

        Self::resolve(&requested, std::io::stdout().is_terminal())
    }

    /// The mode for failures raised before clap has produced any matches —
    /// a spec that will not parse, or a command line clap rejects.
    ///
    /// An explicit `--output` has to win even then: a usage error is exactly
    /// the moment a caller who asked for one shape and got the other has no
    /// way to recover. So argv is read directly, by
    /// [`requested_in_argv`], in clap's own precedence: command line, then
    /// environment, then `auto`.
    pub fn early(argv: &[OsString]) -> Self {
        let requested = requested_in_argv(argv)
            .or_else(environment_request)
            .unwrap_or_default();
        Self::resolve(&requested, std::io::stdout().is_terminal())
    }

    pub fn is_json(self) -> bool {
        matches!(self, Mode::Json { .. })
    }
}

/// `MAPBOX_OUTPUT`, if it holds anything at all.
///
/// An unrecognized value is passed through to [`Mode::resolve`], which falls
/// back to `auto` — but it is worth saying so, since the caller plainly meant
/// something by it.
fn environment_request() -> Option<String> {
    let value = std::env::var(ENV).ok()?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return None;
    }
    if ![AUTO, TEXT, JSON].contains(&value.as_str()) {
        eprintln!(
            "Warning: {ENV}={value} is not one of {AUTO}, {TEXT}, {JSON} — falling back to {AUTO}."
        );
    }
    Some(value)
}

/// Reads `--output`'s value straight off argv.
///
/// A deliberately small re-implementation of one flag's parsing, used only
/// when clap has already refused to parse the line — never in place of it.
/// It accepts the spellings clap does (`--output json`, `--output=json`,
/// `-o json`, `-ojson`, `-o=json`) and stops at `--`, past which nothing is
/// ours. It does not understand short-flag groups (`-do json`), it skips a
/// non-UTF-8 argument rather than stopping at it, and it does not know that
/// everything after `tilesets-cli` belongs to the child. Every one of those
/// misreads resolves to `auto`, which is what reading nothing would have
/// given — and all three only arise on a line clap already rejected, where a
/// best guess at the caller's intent beats ignoring what they wrote.
fn requested_in_argv(argv: &[OsString]) -> Option<String> {
    let mut args = argv.iter().filter_map(|arg| arg.to_str());

    while let Some(arg) = args.next() {
        if arg == "--" {
            return None;
        }
        if arg == "--output" || arg == "-o" {
            return args.next().map(String::from);
        }
        if let Some(value) = arg
            .strip_prefix("--output=")
            .or_else(|| arg.strip_prefix("-o="))
            .or_else(|| arg.strip_prefix("-o"))
        {
            if !value.is_empty() && !value.starts_with('-') {
                return Some(value.to_string());
            }
        }
    }

    None
}

/// Prints a command's result. The only thing that writes to stdout.
///
/// `text` is the prose a person should see; `json` the object a program
/// should. Both are built eagerly — every result here is small enough that
/// deferring one behind a closure costs more at the call site than it saves.
pub fn emit(mode: Mode, text: &str, json: Value) -> Result<()> {
    match mode {
        Mode::Text => write_stdout(text),
        Mode::Json { pretty } => write_stdout(&encode(&json, pretty)?),
    }
}

/// One document, indented or not.
fn encode(value: &Value, pretty: bool) -> Result<String> {
    Ok(if pretty {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    })
}

/// Prints an API response.
///
/// Under `json` it goes out as one compact line, untouched. Under `text` it
/// is rendered as a table or a field list when the response has a shape that
/// suits one, and pretty-printed otherwise.
///
/// `service` gates the exception — see [`list_rendering`]: `search`'s,
/// `geocoder`'s and `tilequery`'s GeoJSON render as a list instead. Every
/// other value takes the path it always has.
///
/// `page` is the note that this response is one page of several. It goes to
/// stderr **in both modes**, unlike the other notes here: the result is just
/// as incomplete under `json`, and the API's own answer cannot carry the
/// fact without wrapping it in an envelope this CLI has promised not to add.
pub fn emit_value(
    mode: Mode,
    value: &Value,
    footer: Option<&str>,
    service: Option<&str>,
    page: Option<&str>,
) -> Result<()> {
    if let Mode::Json { pretty } = mode {
        write_stdout(&encode(value, pretty)?)?;
        // The only thing `json` prints to stderr on a success. A consumer
        // reading stdout alone is unaffected; one that would otherwise
        // believe it had the whole list is told. Through `print_tips` like
        // every other note, so the one thing `json` says on stderr is not
        // also the one thing shaped differently.
        print_tips(page.map(String::from).as_slice());
        return Ok(());
    }

    match list_rendering(value, service).or_else(|| render_human(value)) {
        Some(rendered) => {
            write_stdout(&styled(&rendered, result_in_color()))?;
            // Advice about the result, so stderr — a `-o text > file` keeps
            // the table alone, and a reader still sees where to go next.
            // A blank line first. These are notes about the table, not more
            // of it, and butted against the last row they read as one.
            let next = match (footer, &rendered.identifier) {
                (Some(command), _) => Some(format!("To see one row: {command}")),
                // Every listing can answer this, so none of them has to send
                // the reader to a tool that ships with no operating system.
                // An earlier version suggested a `jq` pipeline here.
                (None, Some(example)) => Some(format!("To see one row: add `--id {example}`")),
                (None, None) => None,
            };

            // Every human rendering says where the machine one is. A table
            // that clipped something has a stronger reason to; a field list
            // shows every value already, so for it this is discoverability
            // rather than a warning, and the wording says which.
            let mut tips = vec![if rendered.shortened {
                "Values are shortened to fit; `-o json` prints each row whole.".to_string()
            } else {
                "`-o json` for the response as the API sent it.".to_string()
            }];
            tips.extend(next);
            // Last, because it is about the response as a whole rather than
            // about the rendering above it.
            tips.extend(page.map(String::from));
            print_tips(&tips);
            Ok(())
        }
        None => write_stdout(&serde_json::to_string_pretty(value)?),
    }
}

/// Prints an API response body that did not parse as JSON.
///
/// Under `json` the body becomes a JSON string, which is a valid document on
/// its own — a caller piping into `jq` gets something parseable rather than
/// a syntax error, and the response really is just text.
pub fn emit_text_body(mode: Mode, body: &str) -> Result<()> {
    match mode {
        Mode::Text => write_stdout(body),
        Mode::Json { pretty } => write_stdout(&encode(&Value::String(body.to_string()), pretty)?),
    }
}

/// Progress and diagnostics. Always stderr, in both modes: this is not the
/// result, and a caller redirecting stdout must not collect it.
pub fn progress(message: &str) {
    eprintln!("{message}");
}

/// Prints one or more tips on stderr, in the one shape every command uses:
/// a single tip reads `Tip: …`; two or more get a `Tips:` header with each
/// one indented on its own line below it. Nothing for an empty list, so the
/// caller needs no guard. A blank line first — these are notes about
/// whatever was just printed, not more of it, and butted against the last
/// line they'd read as one.
fn print_tips(tips: &[String]) {
    if tips.is_empty() {
        return;
    }
    eprintln!();
    for line in tip_lines(tips, style::enabled(std::io::stderr().is_terminal())) {
        eprintln!("{line}");
    }
}

/// The lines [`print_tips`] writes, colored or not. The label is bold and the
/// advice dimmed, so the tips read as secondary to the result above them.
pub(crate) fn tip_lines(tips: &[String], color: bool) -> Vec<String> {
    if let [tip] = tips {
        return vec![format!(
            "{} {}",
            style::paint("Tip:", style::BOLD, color),
            style::dim_prose(tip, color)
        )];
    }
    std::iter::once(style::paint("Tips:", style::BOLD, color))
        .chain(
            tips.iter()
                .map(|tip| format!("  {}", style::dim_prose(tip, color))),
        )
        .collect()
}

/// Whether either output stream is a terminal — something that can be asked
/// for its background color (see `theme::allow_background_query`).
pub fn on_a_terminal() -> bool {
    std::io::stdout().is_terminal() || std::io::stderr().is_terminal()
}

/// Whether a result written to stdout may carry color. Only a terminal:
/// `-o text > file` must leave a file with no escapes in it.
pub fn result_in_color() -> bool {
    style::enabled(std::io::stdout().is_terminal())
}

fn write_stdout(line: &str) -> Result<()> {
    crate::run_record::add_stdout_bytes(line.len() + 1);
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(json: &str) -> Value {
        serde_json::from_str(json).expect("test fixture parses")
    }

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn tips_in_color_say_what_they_say_in_plain_text() {
        let tips = vec![
            "Values are shortened to fit; `-o json` prints each row whole.".to_string(),
            "To see one row: mapbox styles get <style-id>".to_string(),
        ];
        let plain = tip_lines(&tips, false);
        assert_eq!(plain[0], "Tips:");
        let colored = tip_lines(&tips, true);
        assert_eq!(colored[0], format!("{}Tips:{}", style::BOLD, style::RESET));
        let stripped: Vec<String> = colored.iter().map(|line| style::strip(line)).collect();
        assert_eq!(stripped, plain);

        let one = vec!["`-o json` for the response.".to_string()];
        assert_eq!(
            style::strip(&tip_lines(&one, true)[0]),
            tip_lines(&one, false)[0]
        );
    }

    #[test]
    fn a_row_can_be_picked_out_of_a_list_by_id_or_name() {
        let list = rows(r#"[{"id":"a1","note":"first"},{"id":"b2","note":"second"}]"#);
        assert_eq!(pick_row(&list, "b2").unwrap()["note"], "second");

        let named = rows(r#"[{"name":"streets"},{"name":"dark"}]"#);
        assert_eq!(pick_row(&named, "dark").unwrap()["name"], "dark");
    }

    /// A miss is an error, not an empty result: asking for one row and
    /// silently getting none reads like the row exists and is empty.
    #[test]
    fn picking_reports_a_miss_and_a_shape_it_cannot_search() {
        let list = rows(r#"[{"id":"a1"}]"#);
        let missing = pick_row(&list, "nope").unwrap_err();
        assert_eq!(
            missing.downcast_ref::<CliError>().expect("a CliError").code,
            "not_found"
        );

        let single = rows(r#"{"id":"a1"}"#);
        let wrong_shape = pick_row(&single, "a1").unwrap_err();
        assert_eq!(
            wrong_shape
                .downcast_ref::<CliError>()
                .expect("a CliError")
                .code,
            "not_a_list"
        );
    }

    #[test]
    fn auto_follows_the_terminal() {
        assert_eq!(Mode::resolve(AUTO, true), Mode::Text);
        assert_eq!(Mode::resolve(AUTO, false), Mode::Json { pretty: false });
    }

    #[test]
    fn an_explicit_value_ignores_the_terminal() {
        for is_tty in [true, false] {
            assert_eq!(Mode::resolve(JSON, is_tty), Mode::Json { pretty: is_tty });
            assert_eq!(Mode::resolve(TEXT, is_tty), Mode::Text);
        }
    }

    /// `-o json` is asked for by a person as often as by a program, and the
    /// two want different whitespace out of the same document.
    #[test]
    fn json_is_indented_at_a_terminal_and_one_line_in_a_pipe() {
        assert_eq!(Mode::resolve(JSON, true), Mode::Json { pretty: true });
        assert_eq!(Mode::resolve(JSON, false), Mode::Json { pretty: false });
        // `auto` only ever reaches JSON by way of a pipe, so never indented.
        assert_eq!(Mode::resolve(AUTO, false), Mode::Json { pretty: false });
    }

    #[test]
    fn an_unrecognized_value_falls_back_to_auto() {
        assert_eq!(Mode::resolve("", true), Mode::Text);
        assert_eq!(Mode::resolve("yaml", false), Mode::Json { pretty: false });
    }

    #[test]
    fn every_spelling_clap_accepts_is_recognized_on_argv() {
        for line in [
            &["mapbox", "--output", "json", "styles", "list"][..],
            &["mapbox", "--output=json", "styles", "list"][..],
            &["mapbox", "-o", "json", "styles", "list"][..],
            &["mapbox", "-ojson", "styles", "list"][..],
            &["mapbox", "-o=json", "styles", "list"][..],
            &["mapbox", "styles", "list", "-o", "json"][..],
        ] {
            assert_eq!(
                requested_in_argv(&argv(line)).as_deref(),
                Some("json"),
                "{line:?}"
            );
        }
    }

    #[test]
    fn a_line_without_the_flag_requests_nothing() {
        assert_eq!(
            requested_in_argv(&argv(&["mapbox", "styles", "list"])),
            None
        );
        // A bare `-o` with nothing after it, and a value that is another flag.
        assert_eq!(requested_in_argv(&argv(&["mapbox", "-o"])), None);
        assert_eq!(
            requested_in_argv(&argv(&["mapbox", "-o", "--debug"])).as_deref(),
            Some("--debug")
        );
    }

    #[test]
    fn nothing_past_a_double_dash_is_ours() {
        assert_eq!(
            requested_in_argv(&argv(&["mapbox", "tilesets-cli", "--", "-o", "json"])),
            None
        );
    }
}
