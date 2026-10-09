//! What a run did, collected in one place for everything that reports on it.
//!
//! Modules report facts as they happen and `main` calls [`finish`] once on
//! the way out, so a consumer is built from the finished [`Record`] rather
//! than from call sites of its own. `set_*` overwrites, `add_*` accumulates.
//!
//! The record holds raw facts — the command line, URLs, error messages — and
//! never leaves this process. A consumer that sends anything off the machine
//! chooses field by field what it takes. Best-effort: nothing here can change
//! a command's output or exit code.

// Some facts are read only by consumers not in this tree yet.
#![allow(dead_code)]

use std::ffi::OsString;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use clap::parser::ValueSource;
use clap::{ArgMatches, Command};

use crate::spec::ServiceSpec;
use crate::{
    auth, completion, confirm, executor, http, output, run_history, run_log, telemetry_event,
    tilesets_cli,
};

const TILESETS: &str = tilesets_cli::COMMAND;

/// How the command line was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Invocation {
    Execute,
    Schema,
    Help,
    Version,
}

impl Invocation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Invocation::Execute => "execute",
            Invocation::Schema => "schema",
            Invocation::Help => "help",
            Invocation::Version => "version",
        }
    }
}

/// One argument given on the command line or through its environment
/// variable, as clap parsed it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Arg {
    /// clap's id, which is what option names like [`output::ARG`] are.
    pub id: String,
    /// The long name, or the id when there is none.
    pub name: String,
    pub values: Vec<String>,
    pub takes_values: bool,
    /// Restricted to a fixed set of values.
    pub enumerated: bool,
    /// Typed as a number by the operation's spec.
    pub numeric: bool,
    /// Defined on the root command rather than the one that ran.
    pub global: bool,
}

/// The global options a parsed run carries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Options {
    /// `auto`, `text` or `json`, as asked.
    pub output: &'static str,
    /// `flag`, `env` or `default`.
    pub output_source: &'static str,
    pub dry_run: bool,
    pub debug: bool,
    pub yes: bool,
    pub profile: Option<String>,
    /// Only when `--timeout` was typed.
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub source: auth::TokenSource,
    /// `pk`, `sk`, `tk` or `other`.
    pub kind: &'static str,
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Request {
    pub method: String,
    /// With the access token redacted.
    pub url: String,
    /// `None` when no response came back.
    pub status: Option<u16>,
    /// Only for a Mapbox host.
    pub request_id: Option<String>,
    pub request_body_bytes: Option<u64>,
    pub response_bytes: Option<u64>,
    pub elapsed: Duration,
    /// Why no response came back.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Failure {
    pub code: String,
    pub message: String,
}

/// Everything the run reported.
#[derive(Debug, Default)]
pub(crate) struct Record {
    /// A random id for this run, set at [`start`].
    pub id: String,
    /// The command line, without the binary's own path. Raw: tokens are
    /// still in it.
    pub argv: Vec<OsString>,
    /// Command names from the command tree, never from argv.
    pub command: Vec<String>,
    pub invocation: Option<Invocation>,
    /// The first word forwarded to `tilesets`, as typed.
    pub tilesets_word: Option<String>,
    /// clap's name for why it refused the command line.
    pub usage_error: Option<String>,
    pub args: Vec<Arg>,
    /// Top-level fields the operation's spec declares for a JSON body.
    pub body_fields: Vec<String>,
    pub options: Option<Options>,
    pub token: Option<Token>,
    pub requests: Vec<Request>,
    pub more_pages: bool,
    pub error: Option<Failure>,
    pub stdout_bytes: u64,
    pub auth_step: Option<&'static str>,
    pub update_notice: Option<String>,
    pub duration: Duration,
    /// `None` for a `tilesets-cli` run that `exec`s, which never learns it.
    pub exit_code: Option<u32>,
    finished: bool,
}

static RECORD: Mutex<Record> = Mutex::new(Record {
    id: String::new(),
    argv: Vec::new(),
    command: Vec::new(),
    invocation: None,
    tilesets_word: None,
    usage_error: None,
    args: Vec::new(),
    body_fields: Vec::new(),
    options: None,
    token: None,
    requests: Vec::new(),
    more_pages: false,
    error: None,
    stdout_bytes: 0,
    auth_step: None,
    update_notice: None,
    duration: Duration::ZERO,
    exit_code: None,
    finished: false,
});
static STARTED: OnceLock<Instant> = OnceLock::new();

/// A poisoned lock is a panic somewhere else; recording is not worth a
/// second one, so it is skipped.
fn with_record(f: impl FnOnce(&mut Record)) {
    if let Ok(mut record) = RECORD.lock() {
        f(&mut record);
    }
}

/// Marks the start of the run and keeps its command line. Installs the
/// panic hook that finishes the run as `panic`, exit code 101.
pub fn start(argv: &[OsString]) {
    STARTED.get_or_init(Instant::now);
    let argv = argv.get(1..).unwrap_or_default().to_vec();
    let id = uuid_v4(rand::random());
    with_record(|record| {
        record.id = id;
        record.argv = argv;
    });

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        // `try_lock`: the panic may have happened while this thread held
        // the lock, and waiting for it would hang instead of exiting.
        if let Ok(mut record) = RECORD.try_lock() {
            record.error = Some(Failure {
                code: "panic".to_string(),
                message: info.to_string(),
            });
            finish_locked(&mut record, Some(101));
        }
    }));
}

/// The command line clap parsed, for `invocation` `Execute` or `Schema`.
pub fn set_parsed(
    app: &Command,
    specs: &[ServiceSpec],
    matches: &ArgMatches,
    invocation: Invocation,
) {
    let (path, leaf_command, leaf_matches) = leaf(app, matches);
    // `completion` runs at every shell startup and is promised to touch
    // nothing on disk (`it_needs_no_token_and_touches_no_credentials`).
    if path.first().map(String::as_str) == Some(completion::COMMAND) {
        with_record(|record| record.finished = true);
        return;
    }
    let tilesets_word = (path.first().map(String::as_str) == Some(TILESETS))
        .then(|| {
            matches
                .subcommand_matches(TILESETS)
                .map(tilesets_cli::forwarded_args)
                .and_then(|args| {
                    args.iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .find(|arg| !arg.starts_with('-'))
                })
        })
        .flatten();
    let mut args = vec![];
    let mut fields = vec![];
    if path.first().map(String::as_str) != Some(TILESETS) {
        let numeric = numeric_args(specs, &path);
        args.extend(given_args(leaf_command, leaf_matches, &numeric, false));
        args.extend(given_args(app, matches, &[], true));
        fields = body_fields(specs, &path);
    }
    let options = options(matches, leaf_matches);
    with_record(|record| {
        record.command = path;
        record.invocation = Some(invocation);
        record.tilesets_word = tilesets_word;
        record.args = args;
        record.body_fields = fields;
        record.options = Some(options);
    });
}

/// A command line clap refused, or answered with help or the version.
/// Command names are recovered by walking the tree with argv's words, so
/// only names the tree already has can come out.
pub fn set_unparsed(app: &Command, argv: &[OsString], kind: clap::error::ErrorKind) {
    use clap::error::ErrorKind;
    let path = command_from_argv(app, argv);
    with_record(|record| {
        record.command = path;
        match kind {
            ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                record.invocation = Some(Invocation::Help);
            }
            ErrorKind::DisplayVersion => record.invocation = Some(Invocation::Version),
            other => {
                record.invocation = Some(Invocation::Execute);
                record.usage_error = Some(format!("{other:?}"));
            }
        }
    });
}

/// The token a command resolved. Read for its prefix and its `u` claim;
/// the token itself is not kept.
pub fn set_token(source: auth::TokenSource, token: &str) {
    let token = token_fact(source, token);
    with_record(|record| record.token = Some(token));
}

fn token_fact(source: auth::TokenSource, token: &str) -> Token {
    let kind = match token.split('.').next() {
        Some("pk") => "pk",
        Some("sk") => "sk",
        Some("tk") => "tk",
        _ => "other",
    };
    Token {
        source,
        kind,
        account: auth::token_account(token),
    }
}

/// [`set_token`] for the service arms' resolution: a typed `--token`,
/// then the environment unless `--use-login`, then the stored login.
pub fn set_resolved_token(matches: &ArgMatches, use_login: bool, token: &str) {
    let source = if auth::typed_token(matches).is_some() {
        auth::TokenSource::Flag
    } else if !use_login && matches.get_one::<String>("token").is_some() {
        auth::TokenSource::Environment
    } else {
        auth::TokenSource::Login
    };
    set_token(source, token);
}

/// The error the run ended with, as it was reported to the user.
pub fn set_error(code: &str, message: &str) {
    let failure = Failure {
        code: code.to_string(),
        message: message.to_string(),
    };
    with_record(|record| record.error = Some(failure));
}

/// How far `auth login`, `logout` or `refresh` got.
pub fn set_auth_step(step: &'static str) {
    with_record(|record| record.auth_step = Some(step));
}

/// The newer version the update notice named.
pub fn set_update_notice(version: &str) {
    with_record(|record| record.update_notice = Some(version.to_string()));
}

pub fn add_stdout_bytes(bytes: usize) {
    with_record(|record| record.stdout_bytes = record.stdout_bytes.saturating_add(bytes as u64));
}

/// A paginated result stopped with pages left.
pub fn set_more_pages() {
    with_record(|record| record.more_pages = true);
}

/// One request, from [`http::send`].
pub fn add_request(request: Request) {
    with_record(|record| record.requests.push(request));
}

/// Hands the record to each consumer. Once per run; later calls do nothing.
pub fn finish(exit_code: Option<u32>) {
    if let Ok(mut record) = RECORD.lock() {
        finish_locked(&mut record, exit_code);
    }
}

fn finish_locked(record: &mut Record, exit_code: Option<u32>) {
    if record.finished {
        return;
    }
    record.finished = true;
    record.duration = STARTED.get().map_or(Duration::ZERO, Instant::elapsed);
    record.exit_code = exit_code;
    // Diagnostics only for a run history records: detail with no record
    // would be unreachable, and the record says whether detail exists.
    let history = run_history::will_record(record);
    let diagnostics = history && run_log::enabled();
    // One time for both lines, so they land in the same day's file even
    // across midnight.
    let at = SystemTime::now();
    if history {
        run_history::write(record, diagnostics, at);
    }
    if diagnostics {
        run_log::write(record, at);
    }
    run_log::expire_with_history();
    telemetry_event::deliver(record);
}

pub(crate) fn uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The command path, the leaf `Command` and the leaf matches.
fn leaf<'a>(
    app: &'a Command,
    matches: &'a ArgMatches,
) -> (Vec<String>, &'a Command, &'a ArgMatches) {
    let mut path = vec![];
    let mut command = app;
    let mut current = matches;
    while let Some((name, sub)) = current.subcommand() {
        path.push(name.to_string());
        if name == TILESETS && path.len() == 1 {
            // Its forwarded words come back as subcommands of their own.
            return (path, command.find_subcommand(name).unwrap_or(command), sub);
        }
        match command.find_subcommand(name) {
            Some(found) => command = found,
            None => break,
        }
        current = sub;
    }
    (path, command, current)
}

fn command_from_argv(app: &Command, argv: &[OsString]) -> Vec<String> {
    tree_path(
        app,
        argv.iter()
            .skip(1)
            .map(|word| word.to_string_lossy().into_owned()),
    )
}

/// Subcommand names from `words`, in order, for as long as each word names
/// a subcommand of the one before. Flags and their values are skipped; the
/// first word that is neither ends the walk.
pub(crate) fn tree_path(app: &Command, words: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut path = vec![];
    let mut command = app;
    for word in words {
        if word.starts_with('-') {
            continue;
        }
        match command.find_subcommand(&word) {
            Some(found) => {
                path.push(found.get_name().to_string());
                if found.get_name() == TILESETS {
                    break;
                }
                command = found;
            }
            None if path.is_empty() => continue,
            None => break,
        }
    }
    path
}

fn options(matches: &ArgMatches, leaf_matches: &ArgMatches) -> Options {
    let (output, output_source) = output_requested(matches);
    let timeout = (matches.value_source(http::TIMEOUT_ARG) == Some(ValueSource::CommandLine))
        .then(|| matches.get_one::<Duration>(http::TIMEOUT_ARG).copied())
        .flatten();
    Options {
        output,
        output_source,
        dry_run: executor::wants_dry_run(leaf_matches),
        debug: matches.get_flag("debug"),
        yes: matches.get_flag(confirm::ARG),
        profile: matches.get_one::<String>("profile").cloned(),
        timeout,
    }
}

/// `--output` as asked, in `Mode::from_matches`'s precedence, without its
/// warning — that has already been printed once by the time this runs.
fn output_requested(matches: &ArgMatches) -> (&'static str, &'static str) {
    let known = |value: &str| {
        [output::AUTO, output::TEXT, output::JSON]
            .into_iter()
            .find(|known| *known == value)
    };
    if matches.value_source(output::ARG) == Some(ValueSource::CommandLine) {
        let value = matches.get_one::<String>(output::ARG).map(String::as_str);
        return (value.and_then(known).unwrap_or(output::AUTO), "flag");
    }
    match std::env::var(output::ENV)
        .ok()
        .map(|v| v.trim().to_string())
    {
        Some(value) if !value.is_empty() => (known(&value).unwrap_or(output::AUTO), "env"),
        _ => (output::AUTO, "default"),
    }
}

/// The spec parameters of the operation at `path` that are typed as numbers.
/// A free string that happens to be digits (a postcode) is not one.
fn numeric_args(specs: &[ServiceSpec], path: &[String]) -> Vec<String> {
    let Some((service, rest)) = path.split_first() else {
        return vec![];
    };
    specs
        .iter()
        .filter(|spec| &spec.name == service)
        .flat_map(|spec| &spec.operations)
        .filter(|op| op.command_path == rest)
        .flat_map(|op| op.path_params.iter().chain(&op.query_params))
        .filter(|param| param.numeric.is_some())
        .map(|param| param.arg_name.clone())
        .collect()
}

/// The top-level fields the spec declares for the JSON body of the
/// operation at `path`.
fn body_fields(specs: &[ServiceSpec], path: &[String]) -> Vec<String> {
    let Some((service, rest)) = path.split_first() else {
        return vec![];
    };
    specs
        .iter()
        .filter(|spec| &spec.name == service)
        .flat_map(|spec| &spec.operations)
        .find(|op| op.command_path == rest)
        .and_then(|op| op.body.as_ref())
        .map(|body| body.json_fields.clone())
        .unwrap_or_default()
}

/// `command`'s arguments that were given on the command line or through
/// their environment variable. A leaf `Command` does not list the globals it
/// inherits, so those are read separately from the root, as `global`.
///
/// `--token` is left out: the record keeps what [`set_token`] reads from a
/// token, never the token.
fn given_args(
    command: &Command,
    matches: &ArgMatches,
    numeric: &[String],
    global: bool,
) -> Vec<Arg> {
    command
        .get_arguments()
        .filter(|arg| arg.get_id() != "token")
        .filter_map(|arg| {
            let id = arg.get_id().as_str();
            match matches.value_source(id) {
                Some(ValueSource::CommandLine) | Some(ValueSource::EnvVariable) => {}
                _ => return None,
            }
            let values = matches
                .get_raw(id)?
                .map(|v| v.to_string_lossy().into_owned())
                .collect();
            Some(Arg {
                id: id.to_string(),
                name: arg.get_long().unwrap_or(id).to_string(),
                values,
                takes_values: arg.get_action().takes_values(),
                enumerated: !arg.get_possible_values().is_empty(),
                numeric: numeric.iter().any(|n| n == id),
                global,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_names_come_from_the_tree_not_from_argv() {
        let app = Command::new("mapbox").subcommand(
            Command::new("styles")
                .subcommand(Command::new("draft").subcommand(Command::new("get"))),
        );
        let argv = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            command_from_argv(
                &app,
                &argv(&["mapbox", "-o", "json", "styles", "draft", "get", "my-style"])
            ),
            ["styles", "draft", "get"]
        );
        assert_eq!(
            command_from_argv(&app, &argv(&["mapbox", "styles", "typo", "draft"])),
            ["styles"]
        );
        assert!(command_from_argv(&app, &argv(&["mapbox", "/secret/path"])).is_empty());
    }

    #[test]
    fn a_token_is_read_for_its_prefix_and_account_only() {
        let payload = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"u":"example-user","a":"x"}"#,
        );
        let token = format!("sk.{payload}.signature");
        let fact = token_fact(auth::TokenSource::Login, &token);
        assert_eq!(
            fact,
            Token {
                source: auth::TokenSource::Login,
                kind: "sk",
                account: Some("example-user".to_string())
            }
        );
        assert!(!format!("{fact:?}").contains("signature"), "{fact:?}");

        assert_eq!(token_fact(auth::TokenSource::Flag, "garbage").kind, "other");
    }
}
