//! One `cli.command` event per run: what ran and how it ended.
//!
//! Modules report what they know as it happens — the parse, the token, each
//! request, the bytes on stdout, an error code — and `main` calls [`finish`]
//! once on the way out; nothing is written before then. `set_*` functions
//! overwrite a field, last call wins; `add_*` functions accumulate. The
//! finished event goes to [`crate::telemetry_sink`], which decides where it
//! is delivered; this module decides only what it contains.
//!
//! What this refuses to record is the point of it. Argument values leave
//! only when they come from a fixed set (an enum, a boolean, a number the
//! spec types, an allowlisted code); a free string is sent as its length, a
//! file as its size, a coordinate as its name alone. Command names come from
//! the command tree, never from argv. A token is read for its prefix and its
//! account claim and nothing else.
//!
//! Best-effort throughout: nothing here can change a command's output, its
//! exit code, or how long it takes to return. With telemetry off
//! (`MAPBOX_CLI_NO_TELEMETRY`), nothing is recorded or written.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::parser::ValueSource;
use clap::{ArgMatches, Command};
use serde::Serialize;

use crate::spec::ServiceSpec;
use crate::{
    agent_detect, auth, confirm, executor, http, output, schema, telemetry, telemetry_sink,
};

const EVENT: &str = "cli.command";
const SCHEMA_VERSION: &str = "2.0";
const SDK_IDENTIFIER: &str = "mapbox-cli";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The largest `--data @<path>` file read for its top-level keys; above
/// it, only the size is recorded.
const MAX_DATA_TO_PARSE: u64 = 256 * 1024;

/// Set by a workflow on each step it launches, to its own [`event_id`], so
/// a `mapbox` run inside a workflow step records which run started it.
const PARENT_EVENT_ENV: &str = "MAPBOX_CLI_PARENT_EVENT";

const USER_ID_FILE: &str = "user-id";
const LAST_VERSION_FILE: &str = "last-version";

// Bounds from the schema. An event over any of them is rejected whole at
// ingest, so they are enforced here rather than trusted.
const MAX_PARAMS: usize = 50;
const MAX_VALUE: usize = 200;
const MAX_NAME: usize = 64;
const MAX_KEYS: usize = 50;
const MAX_COMMAND_LEVELS: usize = 8;
const MAX_COMMAND_NAME: usize = 32;
const MAX_REQUEST_IDS: usize = 5;
const MAX_REQUEST_ID: usize = 128;
const MAX_CODE: usize = 64;
const MAX_STEPS: usize = 20;
const MAX_VERSION: usize = 32;

/// Options that are top-level fields or `invocation`, so never `params`.
const NOT_PARAMS: &[&str] = &[
    "token",
    "profile",
    "use-login",
    "debug",
    confirm::ARG,
    http::TIMEOUT_ARG,
    output::ARG,
    schema::ARG,
    executor::DRY_RUN_ARG,
    "help",
    "version",
];

/// Global options with no top-level field, sent as free strings. They are
/// read from the root matches: a leaf `Command` does not list the globals
/// it inherits.
const GLOBAL_PARAMS: &[&str] = &["username", output::FILTER_ARG];

/// Commands that record nothing. `completion` runs at every shell startup,
/// usually without anyone typing it, and is promised to touch nothing on
/// disk (`it_needs_no_token_and_touches_no_credentials`).
const NOT_RECORDED: &[&str] = &[crate::completion::COMMAND];

/// Free strings whose values are codes, not user data.
const ALLOWLISTED: &[&str] = &["language", "country", "types"];

/// Numbers that are sent by name only: together they are a location.
const COORDINATES: &[&str] = &["lon", "lat", "longitude", "latitude"];

/// The Python `tilesets` CLI's own commands (`mapbox_tilesets/scripts/cli.py`).
/// A forwarded first word outside this list is recorded as `other`, since it
/// is whatever the user typed.
const TILESETS_COMMANDS: &[&str] = &[
    "add-source",
    "create",
    "delete",
    "delete-changeset",
    "delete-source",
    "estimate-area",
    "job",
    "jobs",
    "list",
    "list-activity",
    "list-sources",
    "publish",
    "publish-changesets",
    "status",
    "tilejson",
    "update",
    "update-recipe",
    "upload-changeset",
    "upload-raster-source",
    "upload-source",
    "validate-recipe",
    "validate-source",
    "view-changeset",
    "view-recipe",
    "view-source",
];

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct Param {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keys: Option<Vec<String>>,
}

/// Where a workflow came from. Only Mapbox names its `Builtin` and
/// `Marketplace` workflows; a `Custom` one is named by the user, so its name
/// is never recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Used by the workflow commands, which don't exist yet.
pub(crate) enum WorkflowSource {
    Builtin,
    Marketplace,
    Custom,
}

impl WorkflowSource {
    fn as_str(self) -> &'static str {
        match self {
            WorkflowSource::Builtin => "builtin",
            WorkflowSource::Marketplace => "marketplace",
            WorkflowSource::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Workflow {
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_count: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    steps: Vec<Step>,
}

/// One step of a workflow run. Built only by [`cli_step`] and
/// [`script_step`], so a script step can never carry a command name or an
/// error category: nothing about a user's script is recorded beyond how it
/// exited and how long it took.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct Auth {
    source: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Network {
    request_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_body_bytes: Option<u64>,
    network_ms: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    request_ids: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    more_pages: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Cli {
    #[serde(skip_serializing_if = "Option::is_none")]
    build_channel: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build_id: Option<&'static str>,
    install_method: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Env {
    arch: &'static str,
    ci: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'static str>,
    stdin_tty: bool,
    stdout_tty: bool,
}

/// The event as sent. Field order is the schema's.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    event: &'static str,
    version: &'static str,
    created: String,
    event_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_event_id: Option<String>,
    user_id: String,
    sdk_identifier: &'static str,
    sdk_version: &'static str,
    operating_system: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invocation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow: Option<Workflow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    params: Vec<Param>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    debug: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    yes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth: Option<Auth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    stdout_bytes: u64,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_step: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    update_notice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<Network>,
    cli: Cli,
    env: Env,
}

/// The global options a parsed run carries, all present or all absent.
#[derive(Debug, Clone, Default, PartialEq)]
struct Options {
    output: &'static str,
    output_source: &'static str,
    dry_run: bool,
    debug: bool,
    yes: bool,
    profile: &'static str,
    timeout_seconds: Option<f64>,
}

/// What the run has reported so far.
#[derive(Debug, Default)]
struct Run {
    command: Option<Vec<String>>,
    invocation: Option<&'static str>,
    workflow: Option<Workflow>,
    params: Vec<Param>,
    usage_error: Option<String>,
    options: Option<Options>,
    auth: Option<Auth>,
    error_code: Option<String>,
    stdout_bytes: u64,
    auth_step: Option<&'static str>,
    update_notice: Option<String>,
    network: Option<Network>,
    finished: bool,
}

impl Run {
    fn skip(&mut self) {
        self.finished = true;
    }

    const fn new() -> Self {
        Run {
            command: None,
            invocation: None,
            workflow: None,
            params: Vec::new(),
            usage_error: None,
            options: None,
            auth: None,
            error_code: None,
            stdout_bytes: 0,
            auth_step: None,
            update_notice: None,
            network: None,
            finished: false,
        }
    }
}

static RUN: Mutex<Run> = Mutex::new(Run::new());
static STARTED: OnceLock<Instant> = OnceLock::new();
static EVENT_ID: OnceLock<String> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

/// Read once: the answer must not change halfway through a run.
fn enabled() -> bool {
    *ENABLED.get_or_init(telemetry::telemetry_allowed)
}

/// A poisoned lock is a panic somewhere else; recording is not worth a
/// second one, so it is skipped.
fn with_run(f: impl FnOnce(&mut Run)) {
    if !enabled() {
        return;
    }
    if let Ok(mut run) = RUN.lock() {
        f(&mut run);
    }
}

/// Marks the start of the run, for `durationMs`, and installs the panic
/// hook that records a crash as `errorCode: "panic"`.
pub fn start() {
    STARTED.get_or_init(Instant::now);
    event_id();

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        // `try_lock`: the panic may have happened while this thread held
        // the lock, and waiting for it would hang instead of exiting.
        if let Ok(mut run) = RUN.try_lock() {
            run.error_code = Some("panic".to_string());
            deliver_locked(&mut run, Some(101));
        }
    }));
}

/// The command line clap parsed. `invocation` is `execute` or `schema`.
pub fn set_parsed(
    app: &Command,
    specs: &[ServiceSpec],
    matches: &ArgMatches,
    invocation: &'static str,
) {
    with_run(|run| {
        let (path, leaf_command, leaf_matches) = leaf(app, matches);
        if path
            .first()
            .is_some_and(|top| NOT_RECORDED.contains(&top.as_str()))
        {
            run.skip();
            return;
        }
        run.command = command_field(&path, matches);
        run.invocation = Some(invocation);
        run.options = Some(options(matches, leaf_matches));
        if invocation == "execute" && path.first().map(String::as_str) != Some(TILESETS) {
            let numeric = numeric_args(specs, &path);
            let mut params = params(leaf_command, leaf_matches, &numeric);
            params.extend(global_params(app, matches));
            params.truncate(MAX_PARAMS);
            run.params = params;
        }
    });
}

/// A command line clap refused, or answered with help or the version.
/// Command names are recovered by walking the tree with argv's words, so
/// only names the tree already has can come out.
pub fn set_unparsed(app: &Command, argv: &[std::ffi::OsString], kind: clap::error::ErrorKind) {
    use clap::error::ErrorKind;
    with_run(|run| {
        let path = command_from_argv(app, argv);
        run.command = (!path.is_empty()).then(|| clip_command(path));
        match kind {
            ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                run.invocation = Some("help");
            }
            ErrorKind::DisplayVersion => run.invocation = Some("version"),
            other => {
                run.invocation = Some("execute");
                run.usage_error = Some(clip(&format!("{other:?}"), MAX_CODE));
            }
        }
    });
}

/// The token a command resolved, for `auth`. Read for its prefix and its
/// `u` claim; the token itself is not kept.
pub fn set_token(source: auth::TokenSource, token: &str) {
    with_run(|run| run.auth = Some(auth_field(source, token)));
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

/// The workflow this run adds, removes or runs. `name` is dropped for a
/// `Custom` workflow, whatever the caller passes.
#[allow(dead_code)] // Called by the workflow commands, which don't exist yet.
pub(crate) fn set_workflow(source: WorkflowSource, name: Option<&str>) {
    with_run(|run| run.workflow = Some(workflow_field(source, name)));
}

fn workflow_field(source: WorkflowSource, name: Option<&str>) -> Workflow {
    let name = match source {
        WorkflowSource::Custom => None,
        _ => name.map(|name| clip(name, MAX_NAME)),
    };
    Workflow {
        source: source.as_str(),
        name,
        step_count: None,
        steps: Vec::new(),
    }
}

/// How many steps the running workflow has, which can be more than the
/// [`MAX_STEPS`] recorded.
#[allow(dead_code)] // Called by the workflow commands, which don't exist yet.
pub(crate) fn set_workflow_step_count(count: usize) {
    with_run(|run| {
        if let Some(workflow) = run.workflow.as_mut() {
            workflow.step_count = Some(u32::try_from(count).unwrap_or(u32::MAX));
        }
    });
}

/// A step that ran a `mapbox` command. `words` is the step's command line;
/// only the names the command tree has are kept, so an argument can't pass
/// for a command name.
#[allow(dead_code)] // Called by the workflow commands, which don't exist yet.
pub(crate) fn add_cli_step(
    app: &Command,
    words: &[&str],
    exit_code: Option<u32>,
    error_code: Option<&str>,
    duration: Duration,
) {
    let step = cli_step(app, words, exit_code, error_code, duration);
    with_run(|run| push_step(run, step));
}

/// A step that ran one of the user's own scripts.
#[allow(dead_code)] // Called by the workflow commands, which don't exist yet.
pub(crate) fn add_script_step(exit_code: Option<u32>, duration: Duration) {
    let step = script_step(exit_code, duration);
    with_run(|run| push_step(run, step));
}

fn cli_step(
    app: &Command,
    words: &[&str],
    exit_code: Option<u32>,
    error_code: Option<&str>,
    duration: Duration,
) -> Step {
    let path = tree_path(app, words.iter().map(|word| word.to_string()));
    Step {
        kind: "cli",
        command: (!path.is_empty()).then(|| clip_command(path)),
        exit_code,
        error_code: error_code.map(|code| clip(code, MAX_CODE)),
        duration_ms: duration.as_millis() as u64,
    }
}

fn script_step(exit_code: Option<u32>, duration: Duration) -> Step {
    Step {
        kind: "script",
        command: None,
        exit_code,
        error_code: None,
        duration_ms: duration.as_millis() as u64,
    }
}

/// Steps belong to a workflow: one reported before [`set_workflow`] has
/// nowhere to go and is dropped.
fn push_step(run: &mut Run, step: Step) {
    if let Some(workflow) = run.workflow.as_mut() {
        if workflow.steps.len() < MAX_STEPS {
            workflow.steps.push(step);
        }
    }
}

/// This run's event id, fixed at [`start`] so a workflow can hand it to the
/// steps it launches before the event itself is built.
pub(crate) fn event_id() -> &'static str {
    EVENT_ID.get_or_init(|| uuid_v4(rand::random()))
}

/// The run that started this one, when a workflow step set it. Read back
/// only when it has the shape of an event id.
fn parent_event_id() -> Option<String> {
    let value = std::env::var(PARENT_EVENT_ENV).ok()?;
    let value = value.trim();
    is_uuid(value).then(|| value.to_string())
}

pub fn set_error_code(code: &str) {
    with_run(|run| run.error_code = Some(clip(code, MAX_CODE)));
}

pub fn set_auth_step(step: &'static str) {
    with_run(|run| run.auth_step = Some(step));
}

pub fn set_update_notice(version: &str) {
    with_run(|run| run.update_notice = Some(clip(version, MAX_VERSION)));
}

pub fn add_stdout_bytes(bytes: usize) {
    with_run(|run| run.stdout_bytes = run.stdout_bytes.saturating_add(bytes as u64));
}

pub fn set_more_pages() {
    with_run(|run| run.network.get_or_insert_with(Network::default).more_pages = true);
}

/// One request, from [`http::send`]. `status` is `None` when no response
/// came back; `request_id` is passed only for a Mapbox response.
pub fn add_request(
    status: Option<u16>,
    response_bytes: Option<u64>,
    request_body_bytes: Option<u64>,
    elapsed: Duration,
    request_id: Option<String>,
) {
    with_run(|run| {
        let network = run.network.get_or_insert_with(Network::default);
        network.request_count = network.request_count.saturating_add(1);
        network.status = status;
        network.response_bytes = sum(network.response_bytes, response_bytes);
        network.request_body_bytes = sum(network.request_body_bytes, request_body_bytes);
        network.network_ms = network
            .network_ms
            .saturating_add(elapsed.as_millis() as u64);
        if let Some(id) = request_id {
            if network.request_ids.len() < MAX_REQUEST_IDS {
                network.request_ids.push(clip(&id, MAX_REQUEST_ID));
            }
        }
    });
}

fn sum(total: Option<u64>, more: Option<u64>) -> Option<u64> {
    match (total, more) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (a, b) => a.or(b),
    }
}

/// Builds the event and hands it to the sink. Once per run; later calls do
/// nothing. `exit_code` is `None` for a `tilesets-cli` run that `exec`s.
pub fn finish(exit_code: Option<u32>) {
    if !enabled() {
        return;
    }
    if let Ok(mut run) = RUN.lock() {
        deliver_locked(&mut run, exit_code);
    }
}

fn deliver_locked(run: &mut Run, exit_code: Option<u32>) {
    if run.finished || !enabled() {
        return;
    }
    run.finished = true;
    let event = build(run, exit_code);
    if let Ok(line) = serde_json::to_string(&event) {
        telemetry_sink::deliver(&line);
    }
}

fn build(run: &Run, exit_code: Option<u32>) -> Event {
    let options = run.options.clone();
    let duration = STARTED.get().map_or(Duration::ZERO, Instant::elapsed);
    Event {
        event: EVENT,
        version: SCHEMA_VERSION,
        created: timestamp(SystemTime::now()),
        event_id: event_id().to_string(),
        parent_event_id: parent_event_id(),
        user_id: user_id(),
        sdk_identifier: SDK_IDENTIFIER,
        sdk_version: CURRENT,
        operating_system: std::env::consts::OS,
        command: run.command.clone(),
        invocation: run.invocation,
        workflow: run.workflow.clone(),
        params: run.params.clone(),
        usage_error: run.usage_error.clone(),
        output: options.as_ref().map(|o| o.output),
        output_source: options.as_ref().map(|o| o.output_source),
        dry_run: options.as_ref().map(|o| o.dry_run),
        debug: options.as_ref().map(|o| o.debug),
        yes: options.as_ref().map(|o| o.yes),
        profile: options.as_ref().map(|o| o.profile),
        timeout_seconds: options.as_ref().and_then(|o| o.timeout_seconds),
        auth: run.auth.clone(),
        exit_code,
        error_code: run.error_code.clone(),
        stdout_bytes: run.stdout_bytes,
        duration_ms: duration.as_millis() as u64,
        auth_step: run.auth_step,
        update_notice: run.update_notice.clone(),
        previous_version: previous_version(),
        network: run.network.clone(),
        cli: Cli {
            build_channel: option_env!("MAPBOX_CLI_BUILD_ENV"),
            build_id: option_env!("MAPBOX_CLI_BUILD_ID"),
            install_method: install_method(std::env::current_exe().ok().as_deref()),
        },
        env: Env {
            arch: telemetry::arch(),
            ci: telemetry::in_ci(),
            agent: agent_detect::detect_agent(),
            stdin_tty: std::io::stdin().is_terminal(),
            stdout_tty: telemetry::stdout_is_terminal(),
        },
    }
}

const TILESETS: &str = crate::tilesets_cli::COMMAND;

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

fn command_field(path: &[String], matches: &ArgMatches) -> Option<Vec<String>> {
    if path.is_empty() {
        return None;
    }
    if path[0] == TILESETS {
        let word = matches
            .subcommand_matches(TILESETS)
            .map(crate::tilesets_cli::forwarded_args)
            .and_then(|args| {
                args.iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .find(|arg| !arg.starts_with('-'))
            });
        let mut command = vec![TILESETS.to_string()];
        if let Some(word) = word {
            command.push(tilesets_word(&word).to_string());
        }
        return Some(command);
    }
    Some(clip_command(path.to_vec()))
}

fn tilesets_word(word: &str) -> &str {
    TILESETS_COMMANDS
        .iter()
        .find(|known| **known == word)
        .copied()
        .unwrap_or("other")
}

fn clip_command(path: Vec<String>) -> Vec<String> {
    path.into_iter()
        .take(MAX_COMMAND_LEVELS)
        .map(|name| clip(&name, MAX_COMMAND_NAME))
        .collect()
}

/// Subcommand names from argv, in order, for as long as each word names a
/// subcommand of the one before. Flags and their values are skipped; the
/// first word that is neither ends the walk.
fn command_from_argv(app: &Command, argv: &[std::ffi::OsString]) -> Vec<String> {
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
fn tree_path(app: &Command, words: impl IntoIterator<Item = String>) -> Vec<String> {
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
    let profile = match matches.get_one::<String>("profile").map(String::as_str) {
        None | Some("default") => "default",
        Some(_) => "named",
    };
    let timeout_seconds = (matches.value_source(http::TIMEOUT_ARG)
        == Some(ValueSource::CommandLine))
    .then(|| matches.get_one::<Duration>(http::TIMEOUT_ARG))
    .flatten()
    .map(Duration::as_secs_f64);
    Options {
        output,
        output_source,
        dry_run: executor::wants_dry_run(leaf_matches),
        debug: matches.get_flag("debug"),
        yes: matches.get_flag(confirm::ARG),
        profile,
        timeout_seconds,
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
/// Only these send a numeric value: a free string that happens to be digits
/// (a postcode) is still a free string.
fn numeric_args(specs: &[ServiceSpec], path: &[String]) -> HashSet<String> {
    let Some((service, rest)) = path.split_first() else {
        return HashSet::new();
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

fn params(command: &Command, matches: &ArgMatches, numeric: &HashSet<String>) -> Vec<Param> {
    let mut out = vec![];
    for arg in command.get_arguments() {
        let id = arg.get_id().as_str();
        if NOT_PARAMS.contains(&id) {
            continue;
        }
        match matches.value_source(id) {
            Some(ValueSource::CommandLine) | Some(ValueSource::EnvVariable) => {}
            _ => continue,
        }
        let Some(raw) = matches.get_raw(id) else {
            continue;
        };
        let values: Vec<String> = raw.map(|v| v.to_string_lossy().into_owned()).collect();
        let name = arg.get_long().unwrap_or(id);
        let takes_values = arg.get_action().takes_values();
        let enumerated = !arg.get_possible_values().is_empty();
        out.push(classify(
            name,
            &values,
            takes_values,
            enumerated,
            numeric.contains(id),
        ));
        if out.len() == MAX_PARAMS {
            break;
        }
    }
    out
}

fn global_params(app: &Command, matches: &ArgMatches) -> Vec<Param> {
    app.get_arguments()
        .filter(|arg| GLOBAL_PARAMS.contains(&arg.get_id().as_str()))
        .filter_map(|arg| {
            let id = arg.get_id().as_str();
            match matches.value_source(id) {
                Some(ValueSource::CommandLine) | Some(ValueSource::EnvVariable) => {}
                _ => return None,
            }
            let value = matches.get_one::<String>(id)?;
            Some(classify(
                arg.get_long().unwrap_or(id),
                std::slice::from_ref(value),
                true,
                false,
                false,
            ))
        })
        .collect()
}

fn classify(
    name: &str,
    values: &[String],
    takes_values: bool,
    enumerated: bool,
    numeric: bool,
) -> Param {
    let joined = values.join(",");
    let mut param = Param {
        name: clip(name, MAX_NAME),
        ..Param::default()
    };
    if COORDINATES.contains(&name) {
        return param;
    }
    if !takes_values || enumerated || numeric || ALLOWLISTED.contains(&name) {
        param.value = Some(clip(&joined, MAX_VALUE));
        return param;
    }
    match name {
        "file" => {
            param.bytes = values
                .iter()
                .filter_map(|path| std::fs::metadata(path).ok())
                .map(|meta| meta.len())
                .reduce(u64::saturating_add);
        }
        "data" => {
            let (bytes, keys) = data_shape(&joined);
            param.bytes = bytes;
            param.keys = keys;
        }
        _ => param.length = Some(joined.chars().count() as u64),
    }
    param
}

/// Size and top-level keys of a `--data` body: inline JSON, `@<path>`, or
/// `@-` (stdin, which is not read twice, so nothing but the name).
fn data_shape(value: &str) -> (Option<u64>, Option<Vec<String>>) {
    if value == "@-" {
        return (None, None);
    }
    let text = match value.strip_prefix('@') {
        Some(path) => {
            let Ok(meta) = std::fs::metadata(path) else {
                return (None, None);
            };
            // Parsed only when small enough to be a request body worth
            // describing; the size alone is still recorded above that.
            if meta.len() > MAX_DATA_TO_PARSE {
                return (Some(meta.len()), None);
            }
            match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(_) => return (Some(meta.len()), None),
            }
        }
        None => value.to_string(),
    };
    let keys = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|json| {
            json.as_object().map(|object| {
                object
                    .keys()
                    .take(MAX_KEYS)
                    .map(|key| clip(key, MAX_NAME))
                    .collect()
            })
        });
    (Some(text.len() as u64), keys)
}

fn auth_field(source: auth::TokenSource, token: &str) -> Auth {
    let kind = match token.split('.').next() {
        Some("pk") => "pk",
        Some("sk") => "sk",
        Some("tk") => "tk",
        _ => "other",
    };
    Auth {
        source: source.as_str(),
        kind,
        account: auth::token_account(token).map(|account| clip(&account, MAX_NAME)),
    }
}

/// How this binary was installed, from where it lives. Only the category
/// leaves; the path does not. `MAPBOX_INSTALL_DIR` moves an install-script
/// binary anywhere, so those read as `other`.
fn install_method(exe: Option<&Path>) -> &'static str {
    let Some(exe) = exe else {
        return "other";
    };
    let path = exe
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    if path.contains("/cellar/") || path.contains("/homebrew/") {
        "homebrew"
    } else if path.contains("/.cargo/bin/") {
        "cargo"
    } else if path.contains("/.local/bin/") || path.contains("/programs/mapbox/") {
        "install-script"
    } else {
        "other"
    }
}

fn clip(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// The installation's random id, created on first use. Two first runs in
/// parallel can each create one; `create_new` makes the second read the
/// first's instead of replacing it. When nothing can be stored, the run
/// still gets an id — just not one the next run will share.
fn user_id() -> String {
    if let Some(existing) = read_user_id() {
        return existing;
    }
    let fresh = uuid_v4(rand::random());
    if telemetry_sink::create_state(USER_ID_FILE, &fresh) {
        return fresh;
    }
    read_user_id().unwrap_or(fresh)
}

fn read_user_id() -> Option<String> {
    telemetry_sink::read_state(USER_ID_FILE).filter(|id| is_uuid(id))
}

fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The version the last run recorded, when it differs from this one — the
/// first run after an upgrade. Checked against the same shape the update
/// check trusts, since it is read back from disk.
fn previous_version() -> Option<String> {
    let last = telemetry_sink::read_state(LAST_VERSION_FILE);
    if last.as_deref() == Some(CURRENT) {
        return None;
    }
    telemetry_sink::replace_state(LAST_VERSION_FILE, CURRENT);
    last.filter(|version| is_version(version))
        .map(|version| clip(&version, MAX_VERSION))
}

fn is_version(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_VERSION
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

fn uuid_v4(mut bytes: [u8; 16]) -> String {
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

/// RFC 3339 in UTC, to the millisecond.
fn timestamp(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (date, secs) = telemetry_sink::utc_date(since.as_secs());
    format!(
        "{date}T{:02}:{:02}:{:02}.{:03}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60,
        since.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_step_records_only_how_it_exited_and_how_long_it_took() {
        let step = script_step(Some(3), Duration::from_millis(2300));
        assert_eq!(
            serde_json::to_value(&step).unwrap(),
            serde_json::json!({ "kind": "script", "exitCode": 3, "durationMs": 2300 })
        );
    }

    #[test]
    fn a_cli_step_keeps_only_names_the_command_tree_has() {
        let app = Command::new("mapbox")
            .subcommand(Command::new("styles").subcommand(Command::new("get")));
        let step = cli_step(
            &app,
            &["styles", "get", "my-secret-style"],
            Some(0),
            None,
            Duration::ZERO,
        );
        assert_eq!(
            step.command,
            Some(vec!["styles".to_string(), "get".to_string()])
        );

        let unknown = cli_step(
            &app,
            &["/Users/someone/run.sh"],
            Some(1),
            Some("error"),
            Duration::ZERO,
        );
        assert_eq!(unknown.command, None);
        assert_eq!(unknown.error_code.as_deref(), Some("error"));
    }

    #[test]
    fn steps_need_a_workflow_and_stop_at_the_bound() {
        let mut run = Run::new();
        push_step(&mut run, script_step(Some(0), Duration::ZERO));
        assert!(
            run.workflow.is_none(),
            "a step without a workflow is dropped"
        );

        run.workflow = Some(workflow_field(WorkflowSource::Builtin, Some("style-clone")));
        for _ in 0..MAX_STEPS + 5 {
            push_step(&mut run, script_step(Some(0), Duration::ZERO));
        }
        assert_eq!(run.workflow.unwrap().steps.len(), MAX_STEPS);
    }

    #[test]
    fn a_custom_workflow_never_records_its_name() {
        assert_eq!(
            workflow_field(WorkflowSource::Custom, Some("acme-client-export")).name,
            None
        );
        assert_eq!(
            workflow_field(WorkflowSource::Marketplace, Some("style-clone")).name,
            Some("style-clone".to_string())
        );
        assert_eq!(workflow_field(WorkflowSource::Builtin, None).name, None);
    }

    #[test]
    fn a_uuid_is_version_4_and_well_formed() {
        let id = uuid_v4([0xff; 16]);
        assert!(is_uuid(&id), "{id}");
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
        assert!(!is_uuid("not-a-uuid"));
    }

    #[test]
    fn timestamps_are_utc_to_the_millisecond() {
        let at = UNIX_EPOCH + Duration::from_millis(1_790_000_000_123);
        assert_eq!(timestamp(at), "2026-09-21T14:13:20.123Z");
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn free_strings_send_their_length_and_codes_send_their_value() {
        let values = |v: &str| vec![v.to_string()];
        let q = classify("q", &values("1600 Pennsylvania Ave"), true, false, false);
        assert_eq!(q.value, None);
        assert_eq!(q.length, Some(21));

        // Digits are still a free string unless the spec types them.
        let postcode = classify("postcode", &values("10001"), true, false, false);
        assert_eq!((postcode.value, postcode.length), (None, Some(5)));

        let limit = classify("limit", &values("5"), true, false, true);
        assert_eq!(limit.value.as_deref(), Some("5"));

        let language = classify("language", &values("en"), true, false, false);
        assert_eq!(language.value.as_deref(), Some("en"));

        let flag = classify("download", &values("true"), false, false, false);
        assert_eq!(flag.value.as_deref(), Some("true"));
    }

    #[test]
    fn coordinates_send_their_name_only() {
        for name in COORDINATES {
            let param = classify(name, &["12.5".to_string()], true, false, true);
            assert_eq!(
                param,
                Param {
                    name: name.to_string(),
                    ..Param::default()
                }
            );
        }
    }

    #[test]
    fn a_data_body_sends_its_size_and_top_level_keys_only() {
        let (bytes, keys) = data_shape(r#"{"name":"secret","layers":[]}"#);
        assert_eq!(bytes, Some(29));
        assert_eq!(keys, Some(vec!["layers".to_string(), "name".to_string()]));

        assert_eq!(data_shape("[1,2]"), (Some(5), None));
        assert_eq!(data_shape("@-"), (None, None));
    }

    #[test]
    fn a_long_value_is_clipped_to_the_schema_bound() {
        let param = classify("types", &["x".repeat(500)], true, false, false);
        assert_eq!(param.value.map(|v| v.len()), Some(MAX_VALUE));
    }

    #[test]
    fn an_unknown_tilesets_word_is_other() {
        assert_eq!(tilesets_word("upload-source"), "upload-source");
        assert_eq!(tilesets_word("my-secret-tileset"), "other");
    }

    #[test]
    fn the_install_method_is_a_category_never_the_path() {
        let method = |p: &str| install_method(Some(Path::new(p)));
        assert_eq!(
            method("/opt/homebrew/Cellar/mapbox/0.3.0/bin/mapbox"),
            "homebrew"
        );
        assert_eq!(method("/Users/a/.cargo/bin/mapbox"), "cargo");
        assert_eq!(method("/home/a/.local/bin/mapbox"), "install-script");
        assert_eq!(
            method(r"C:\Users\a\AppData\Local\Programs\mapbox\mapbox.exe"),
            "install-script"
        );
        assert_eq!(method("/srv/tools/mapbox"), "other");
        assert_eq!(install_method(None), "other");
    }

    #[test]
    fn a_token_is_read_for_its_prefix_and_account_only() {
        let payload = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"u":"example-user","a":"x"}"#,
        );
        let token = format!("sk.{payload}.signature");
        let auth = auth_field(auth::TokenSource::Login, &token);
        assert_eq!(
            auth,
            Auth {
                source: "login",
                kind: "sk",
                account: Some("example-user".to_string())
            }
        );
        let serialized = serde_json::to_string(&auth).unwrap();
        assert!(!serialized.contains("signature"), "{serialized}");

        assert_eq!(auth_field(auth::TokenSource::Flag, "garbage").kind, "other");
    }

    #[test]
    fn command_names_come_from_the_tree_not_from_argv() {
        let app = Command::new("mapbox").subcommand(
            Command::new("styles")
                .subcommand(Command::new("draft").subcommand(Command::new("get"))),
        );
        let argv = |words: &[&str]| {
            words
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        };
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
}
