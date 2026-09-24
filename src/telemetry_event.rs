//! One `cli.command` event per run: what ran and how it ended.
//!
//! Built from the [`crate::run_record::Record`] at the end of each run and
//! handed to [`crate::telemetry_sink`], which decides where it is delivered;
//! this module decides only what it contains. The record holds raw facts —
//! the command line, URLs, error messages — and every field of the event is
//! chosen from it here, explicitly, so this file is the whole of what can
//! leave the machine.
//!
//! What this refuses to send is the point of it. Argument values leave
//! only when they come from a fixed set (an enum, a boolean, a number the
//! spec types, an allowlisted code); a free string is sent as its length, a
//! file as its size, a coordinate as its name alone. Command names come from
//! the command tree, never from argv. A token is read for its prefix and its
//! account claim and nothing else.
//!
//! Best-effort throughout: nothing here can change a command's output, its
//! exit code, or how long it takes to return. With telemetry off
//! (`MAPBOX_CLI_NO_TELEMETRY`, or `mapbox config set telemetry off`),
//! nothing is built or written.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use clap::Command;
use serde::Serialize;

use crate::run_record::{self, uuid_v4, Invocation, Record};
use crate::{agent_detect, confirm, executor, http, output, schema, telemetry, telemetry_sink};

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

/// The workflow this run belongs to. Kept here rather than in the record:
/// the workflow commands that report it don't exist yet.
static WORKFLOW: Mutex<Option<Workflow>> = Mutex::new(None);
static EVENT_ID: OnceLock<String> = OnceLock::new();

fn with_workflow(f: impl FnOnce(&mut Option<Workflow>)) {
    if let Ok(mut workflow) = WORKFLOW.lock() {
        f(&mut workflow);
    }
}

/// Builds the run's event and hands it to the sink, unless telemetry is off.
pub(crate) fn deliver(record: &Record) {
    // Read at the end of the run: the run that turns telemetry off is one
    // that should not report itself.
    if !telemetry::telemetry_allowed() || !crate::config::telemetry_enabled() {
        return;
    }
    let event = build(record);
    if let Ok(line) = serde_json::to_string(&event) {
        telemetry_sink::deliver(&line);
    }
}

fn build(record: &Record) -> Event {
    let options = record.options.as_ref();
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
        command: command_field(record),
        invocation: record.invocation.map(Invocation::as_str),
        workflow: WORKFLOW.lock().ok().and_then(|workflow| workflow.clone()),
        params: params(record),
        usage_error: record
            .usage_error
            .as_deref()
            .map(|kind| clip(kind, MAX_CODE)),
        output: options.map(|o| o.output),
        output_source: options.map(|o| o.output_source),
        dry_run: options.map(|o| o.dry_run),
        debug: options.map(|o| o.debug),
        yes: options.map(|o| o.yes),
        profile: options.map(|o| match o.profile.as_deref() {
            None | Some("default") => "default",
            Some(_) => "named",
        }),
        timeout_seconds: options.and_then(|o| o.timeout).map(|t| t.as_secs_f64()),
        auth: record.token.as_ref().map(|token| Auth {
            source: token.source.as_str(),
            kind: token.kind,
            account: token.account.as_deref().map(|a| clip(a, MAX_NAME)),
        }),
        exit_code: record.exit_code,
        error_code: record.error.as_ref().map(|e| clip(&e.code, MAX_CODE)),
        stdout_bytes: record.stdout_bytes,
        duration_ms: record.duration.as_millis() as u64,
        auth_step: record.auth_step,
        update_notice: record
            .update_notice
            .as_deref()
            .map(|v| clip(v, MAX_VERSION)),
        previous_version: previous_version(),
        network: network(record),
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

fn command_field(record: &Record) -> Option<Vec<String>> {
    let first = record.command.first()?;
    if first == TILESETS {
        let mut command = vec![TILESETS.to_string()];
        if let Some(word) = &record.tilesets_word {
            command.push(tilesets_word(word).to_string());
        }
        return Some(command);
    }
    Some(clip_command(record.command.clone()))
}

/// Arguments of an executed command, as the schema allows them: the leaf's
/// own, less [`NOT_PARAMS`], then the [`GLOBAL_PARAMS`]. `tilesets-cli`'s
/// forwarded words are not arguments of this CLI and are never sent.
fn params(record: &Record) -> Vec<Param> {
    if record.invocation != Some(Invocation::Execute) {
        return vec![];
    }
    let leaf = record
        .args
        .iter()
        .filter(|arg| !arg.global && !NOT_PARAMS.contains(&arg.id.as_str()));
    let global = record
        .args
        .iter()
        .filter(|arg| arg.global && GLOBAL_PARAMS.contains(&arg.id.as_str()));
    leaf.chain(global)
        .take(MAX_PARAMS)
        .map(|arg| {
            classify(
                &arg.name,
                &arg.values,
                arg.takes_values,
                arg.enumerated,
                arg.numeric,
            )
        })
        .collect()
}

fn network(record: &Record) -> Option<Network> {
    if record.requests.is_empty() && !record.more_pages {
        return None;
    }
    let mut network = Network {
        more_pages: record.more_pages,
        ..Network::default()
    };
    for request in &record.requests {
        network.request_count = network.request_count.saturating_add(1);
        network.status = request.status;
        network.response_bytes = sum(network.response_bytes, request.response_bytes);
        network.request_body_bytes = sum(network.request_body_bytes, request.request_body_bytes);
        network.network_ms = network
            .network_ms
            .saturating_add(request.elapsed.as_millis() as u64);
        if let Some(id) = &request.request_id {
            if network.request_ids.len() < MAX_REQUEST_IDS {
                network.request_ids.push(clip(id, MAX_REQUEST_ID));
            }
        }
    }
    Some(network)
}

/// The workflow this run adds, removes or runs. `name` is dropped for a
/// `Custom` workflow, whatever the caller passes.
#[allow(dead_code)] // Called by the workflow commands, which don't exist yet.
pub(crate) fn set_workflow(source: WorkflowSource, name: Option<&str>) {
    with_workflow(|workflow| *workflow = Some(workflow_field(source, name)));
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
    with_workflow(|workflow| {
        if let Some(workflow) = workflow.as_mut() {
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
    with_workflow(|workflow| push_step(workflow, step));
}

/// A step that ran one of the user's own scripts.
#[allow(dead_code)] // Called by the workflow commands, which don't exist yet.
pub(crate) fn add_script_step(exit_code: Option<u32>, duration: Duration) {
    let step = script_step(exit_code, duration);
    with_workflow(|workflow| push_step(workflow, step));
}

fn cli_step(
    app: &Command,
    words: &[&str],
    exit_code: Option<u32>,
    error_code: Option<&str>,
    duration: Duration,
) -> Step {
    let path = run_record::tree_path(app, words.iter().map(|word| word.to_string()));
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
fn push_step(workflow: &mut Option<Workflow>, step: Step) {
    if let Some(workflow) = workflow.as_mut() {
        if workflow.steps.len() < MAX_STEPS {
            workflow.steps.push(step);
        }
    }
}

/// This run's event id, fixed on first use so a workflow can hand it to the
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

fn sum(total: Option<u64>, more: Option<u64>) -> Option<u64> {
    match (total, more) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (a, b) => a.or(b),
    }
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

fn timestamp(at: SystemTime) -> String {
    crate::dated_jsonl::timestamp(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

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
        let mut workflow = None;
        push_step(&mut workflow, script_step(Some(0), Duration::ZERO));
        assert!(workflow.is_none(), "a step without a workflow is dropped");

        workflow = Some(workflow_field(WorkflowSource::Builtin, Some("style-clone")));
        for _ in 0..MAX_STEPS + 5 {
            push_step(&mut workflow, script_step(Some(0), Duration::ZERO));
        }
        assert_eq!(workflow.unwrap().steps.len(), MAX_STEPS);
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
}
