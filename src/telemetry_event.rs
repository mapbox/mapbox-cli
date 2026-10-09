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
//! file as its size, a coordinate as its name alone, and a JSON body as its
//! size and the top-level field names its spec declares. Command names come
//! from the command tree, never from argv. A token is read for its prefix and
//! its account claim and nothing else. No response's request id is sent: it
//! leads to the request in CloudFront logs, which hold the caller's IP.
//!
//! Best-effort throughout: nothing here can change a command's output, its
//! exit code, or how long it takes to return. With telemetry off
//! (`MAPBOX_CLI_NO_TELEMETRY`, or `mapbox config set telemetry off`), nothing
//! is built or written.

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

/// How long a `userId` lives before the next run replaces it, so it is never
/// a permanent identifier.
const USER_ID_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

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
const MAX_VALUE: usize = 64;
const MAX_KEYS: usize = 50;
const MAX_COMMAND_LEVELS: usize = 8;
const MAX_STEPS: usize = 20;

// The schema leaves these strings unbounded; they are clipped here so that
// an unexpected input can't make the event large.
const MAX_NAME: usize = 64;
const MAX_COMMAND_NAME: usize = 32;
const MAX_CODE: usize = 64;
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

/// Free strings whose values are codes, not user data — sent only when every
/// comma-separated item looks like one (see [`is_code`]). The specs type them
/// as plain strings, so anything else is text the user chose and goes out as
/// a length like any other free string.
const ALLOWLISTED: &[&str] = &["language", "country", "types"];

/// Feature types the geocoding and search APIs define, for `types`.
const FEATURE_TYPES: &[&str] = &[
    "address",
    "block",
    "brand",
    "category",
    "city",
    "country",
    "district",
    "locality",
    "neighborhood",
    "place",
    "poi",
    "postcode",
    "region",
    "secondary_address",
    "street",
];

/// Numbers that are sent by name only: together they are a location. A tile
/// address is one too — `z`/`x`/`y` at zoom 18 pins about 150 m.
const COORDINATES: &[&str] = &["lon", "lat", "longitude", "latitude", "x", "y", "z", "zoom"];

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
#[serde(rename_all = "camelCase")]
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

/// Builds the run's event and hands it to the sink, unless telemetry is off
/// or the run is under `sudo`.
///
/// `sudo` keeps `HOME`, so a root run would create `~/.mapbox/.telemetry` and
/// its state files as root, and every later run as the user could read none
/// of them — a fresh `userId` each time. Skipped for the same reason
/// `run_history` skips it (aws/aws-cli#10031).
///
/// `uninstall` on Windows is skipped too: its helper deletes `mapbox.exe` a
/// second after exit, and a sender still running from that image would keep
/// the file locked and the delete would fail.
pub(crate) fn deliver(record: &Record) {
    // Read at the end of the run, so the run that turns telemetry off with
    // `config set` does not report itself.
    if !telemetry::telemetry_allowed() || std::env::var_os("SUDO_USER").is_some() {
        return;
    }
    if cfg!(windows)
        && record.command.first().map(String::as_str) == Some(crate::uninstall::COMMAND)
    {
        return;
    }
    let event = build(record);
    if let Ok(line) = serde_json::to_string(&event) {
        let profile = record.options.as_ref().and_then(|o| o.profile.as_deref());
        telemetry_sink::deliver(&line, profile);
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
                &record.body_fields,
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
    body_fields: &[String],
) -> Param {
    let joined = values.join(",");
    let mut param = Param {
        name: clip(name, MAX_NAME),
        ..Param::default()
    };
    if COORDINATES.contains(&name) {
        return param;
    }
    if !takes_values || enumerated || numeric || is_code(name, values) {
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
            let (bytes, keys) = data_shape(&joined, body_fields);
            param.bytes = bytes;
            param.keys = keys;
        }
        _ => param.length = Some(joined.chars().count() as u64),
    }
    param
}

/// Whether every item of an [`ALLOWLISTED`] value is a code: a country code
/// (`us`), a language code with an optional script or region (`en`,
/// `zh-Hans`, `pt-BR`, `es-419`), or a known feature type. The shapes are
/// tight on purpose: anything looser lets a short word through.
fn is_code(name: &str, values: &[String]) -> bool {
    fn letters(part: &str, len: usize) -> bool {
        part.len() == len && part.chars().all(|c| c.is_ascii_alphabetic())
    }
    fn language(item: &str) -> bool {
        let mut parts = item.splitn(2, ['-', '_']);
        letters(parts.next().unwrap_or_default(), 2)
            && parts.next().is_none_or(|tail| {
                letters(tail, 4)
                    || letters(tail, 2)
                    || (tail.len() == 3 && tail.chars().all(|c| c.is_ascii_digit()))
            })
    }
    if !ALLOWLISTED.contains(&name) {
        return false;
    }
    let items = || values.iter().flat_map(|v| v.split(',')).map(str::trim);
    match name {
        "country" => items().all(|item| letters(item, 2)),
        "language" => items().all(language),
        "types" => items().all(|item| FEATURE_TYPES.contains(&item)),
        _ => false,
    }
}

/// Size and top-level keys of a `--data` body: inline JSON, `@<path>`, or
/// `@-` (stdin, which is not read twice, so nothing but the name). Only keys
/// in `declared` — the fields the operation's spec names — are kept; any
/// other is text the user chose.
fn data_shape(value: &str, declared: &[String]) -> (Option<u64>, Option<Vec<String>>) {
    if value == "@-" {
        return (None, None);
    }
    let text = match value.strip_prefix('@') {
        Some(path) => {
            let Ok(meta) = std::fs::metadata(path) else {
                return (None, None);
            };
            // The command has already read it, so only a regular file can be
            // read again: a FIFO or `/dev/stdin` would block the exit.
            if !meta.is_file() {
                return (None, None);
            }
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
                    .filter(|key| declared.contains(key))
                    .take(MAX_KEYS)
                    .map(|key| clip(key, MAX_NAME))
                    .collect::<Vec<_>>()
            })
        })
        .filter(|keys| !keys.is_empty());
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

/// A random id, stored as `<id> <unix seconds created>` and replaced by the
/// first run at least [`USER_ID_LIFETIME`] after it was created. Nothing
/// rotates it in the background; an unused CLI sends nothing either.
///
/// Two first runs in parallel can each create one; `create_new` makes the
/// second read the first's instead of replacing it. When nothing can be
/// stored, the run still gets an id — just not one the next run will share.
fn user_id() -> String {
    let now = SystemTime::now();
    let stored = telemetry_sink::read_state(USER_ID_FILE);
    if let Some(id) = stored.as_deref().and_then(|s| live_user_id(s, now)) {
        return id;
    }
    let fresh = uuid_v4(rand::random());
    let contents = format!("{fresh} {}", unix_seconds(now));
    if stored.is_some() {
        telemetry_sink::replace_state(USER_ID_FILE, &contents);
        return fresh;
    }
    if telemetry_sink::create_state(USER_ID_FILE, &contents) {
        return fresh;
    }
    telemetry_sink::read_state(USER_ID_FILE)
        .and_then(|s| live_user_id(&s, now))
        .unwrap_or(fresh)
}

/// The stored id, unless it is malformed or due for replacement. One dated
/// in the future, from a clock that moved back, is replaced too: its age
/// can't be trusted.
fn live_user_id(stored: &str, now: SystemTime) -> Option<String> {
    let (id, created) = stored.split_once(' ')?;
    let created = created.parse::<u64>().ok()?;
    let age = unix_seconds(now).checked_sub(created)?;
    (is_uuid(id) && age < USER_ID_LIFETIME.as_secs()).then(|| id.to_string())
}

fn unix_seconds(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
        let q = classify(
            "q",
            &values("1600 Pennsylvania Ave"),
            true,
            false,
            false,
            &[],
        );
        assert_eq!(q.value, None);
        assert_eq!(q.length, Some(21));

        // Digits are still a free string unless the spec types them.
        let postcode = classify("postcode", &values("10001"), true, false, false, &[]);
        assert_eq!((postcode.value, postcode.length), (None, Some(5)));

        let limit = classify("limit", &values("5"), true, false, true, &[]);
        assert_eq!(limit.value.as_deref(), Some("5"));

        let language = classify("language", &values("en"), true, false, false, &[]);
        assert_eq!(language.value.as_deref(), Some("en"));

        let flag = classify("download", &values("true"), false, false, false, &[]);
        assert_eq!(flag.value.as_deref(), Some("true"));
    }

    #[test]
    fn coordinates_send_their_name_only() {
        for name in COORDINATES {
            let param = classify(name, &["12.5".to_string()], true, false, true, &[]);
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
    fn a_data_body_sends_its_size_and_only_the_keys_its_spec_declares() {
        let declared = ["layers".to_string(), "name".to_string()];
        let (bytes, keys) = data_shape(
            r#"{"name":"secret","layers":[],"acme-client-x":1}"#,
            &declared,
        );
        assert_eq!(bytes, Some(47));
        assert_eq!(keys, Some(vec!["layers".to_string(), "name".to_string()]));

        // With nothing declared, no key the user wrote can pass.
        assert_eq!(data_shape(r#"{"name":"x"}"#, &[]), (Some(12), None));
        assert_eq!(data_shape("[1,2]", &declared), (Some(5), None));
        assert_eq!(data_shape("@-", &declared), (None, None));
    }

    /// A FIFO was already drained by the command; opening it again at exit
    /// would block until something else wrote to it.
    #[cfg(unix)]
    #[test]
    fn a_data_path_that_is_not_a_regular_file_is_not_read_again() {
        let dir = std::env::temp_dir().join(format!("mapbox-fifo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let fifo = dir.join(format!("body-{nanos}"));
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo");
        assert!(made.success());

        let (done, finished) = std::sync::mpsc::channel();
        let arg = format!("@{}", fifo.display());
        std::thread::spawn(move || done.send(data_shape(&arg, &[])));
        let shape = finished.recv_timeout(std::time::Duration::from_secs(5));
        assert_eq!(shape.expect("data_shape returned"), (None, None));
    }

    #[test]
    fn the_user_id_is_replaced_once_it_is_a_day_old() {
        let id = uuid_v4([0x11; 16]);
        let created = 1_790_000_000;
        let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
        let stored = format!("{id} {created}");

        assert_eq!(live_user_id(&stored, at(created)), Some(id.clone()));
        assert_eq!(
            live_user_id(&stored, at(created + USER_ID_LIFETIME.as_secs() - 1)),
            Some(id.clone())
        );
        assert_eq!(
            live_user_id(&stored, at(created + USER_ID_LIFETIME.as_secs())),
            None
        );
        assert_eq!(
            live_user_id(&stored, at(created - 1)),
            None,
            "from the future"
        );
        assert_eq!(live_user_id(&id, at(created)), None, "no creation time");
        assert_eq!(live_user_id("not-a-uuid 1790000000", at(created)), None);
    }

    #[test]
    fn no_request_id_is_sent() {
        let mut record = Record::default();
        record.requests = vec![run_record::Request {
            method: "GET".to_string(),
            url: "https://api.mapbox.com/styles/v1/x".to_string(),
            status: Some(200),
            request_id: Some("Hbq3kQ2x9mTzL0pWvR7yN4cD8sF1gJ6aE5uK".to_string()),
            request_body_bytes: None,
            response_bytes: Some(10),
            elapsed: Duration::from_millis(5),
            error: None,
        }];
        let sent = serde_json::to_string(&network(&record)).unwrap();
        assert!(
            !sent.contains("Hbq3kQ2x9mTzL0pWvR7yN4cD8sF1gJ6aE5uK"),
            "{sent}"
        );
        assert!(sent.contains("\"requestCount\":1"), "{sent}");
    }

    #[test]
    fn a_long_value_is_clipped_to_the_schema_bound() {
        let param = classify("mode", &["x".repeat(500)], true, true, false, &[]);
        assert_eq!(param.value.map(|v| v.len()), Some(MAX_VALUE));
    }

    #[test]
    fn allowlisted_values_go_out_only_when_they_are_codes() {
        for (name, value) in [
            ("language", "en"),
            ("language", "zh-Hans,pt-BR,es-419"),
            ("country", "us,cn"),
            ("types", "address,poi"),
        ] {
            let param = classify(name, &[value.to_string()], true, false, false, &[]);
            assert_eq!(param.value.as_deref(), Some(value), "{name}={value}");
        }
        for (name, value) in [
            ("country", "my secret project name"),
            ("country", "us,my-secret"),
            ("country", "joe"),
            ("language", "acme-corporation-internal"),
            ("language", "ab-project1"),
            ("language", "joe"),
            ("types", "notes about client acme"),
            ("types", "address,acme"),
        ] {
            let param = classify(name, &[value.to_string()], true, false, false, &[]);
            assert_eq!(
                (param.value, param.length),
                (None, Some(value.chars().count() as u64)),
                "{name}={value}"
            );
        }
    }

    /// A number's value is sent unless its name is in [`COORDINATES`], so a
    /// location parameter a spec adds later, such as `proximity-lng`, would
    /// leak. This fails until it is listed.
    #[test]
    fn every_numeric_location_parameter_is_a_coordinate() {
        let specs = crate::spec::effective_services().expect("the bundled specs");
        let mut unlisted: Vec<String> = specs
            .iter()
            .flat_map(|spec| &spec.operations)
            .flat_map(|op| op.path_params.iter().chain(&op.query_params))
            .filter(|param| param.numeric.is_some())
            .map(|param| param.arg_name.clone())
            .filter(|name| {
                ["lon", "lat", "lng", "coord"]
                    .iter()
                    .any(|part| name.contains(part))
                    && !COORDINATES.contains(&name.as_str())
            })
            .collect();
        unlisted.sort();
        unlisted.dedup();
        assert!(
            unlisted.is_empty(),
            "numeric location parameters not in COORDINATES: {unlisted:?}"
        );
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

    /// Every field path the `cli.command` schema in mapbox/event-schema
    /// declares. Ingestion accepts undeclared fields, so this list is what
    /// holds "only declared fields are sent": add a path here only together
    /// with the schema.
    const DECLARED_FIELDS: &[&str] = &[
        "event",
        "version",
        "created",
        "eventId",
        "parentEventId",
        "userId",
        "sdkIdentifier",
        "sdkVersion",
        "operatingSystem",
        "command",
        "invocation",
        "workflow",
        "workflow.source",
        "workflow.name",
        "workflow.stepCount",
        "workflow.steps",
        "workflow.steps[].kind",
        "workflow.steps[].command",
        "workflow.steps[].exitCode",
        "workflow.steps[].errorCode",
        "workflow.steps[].durationMs",
        "params",
        "params[].name",
        "params[].value",
        "params[].length",
        "params[].bytes",
        "params[].keys",
        "usageError",
        "output",
        "outputSource",
        "dryRun",
        "debug",
        "yes",
        "profile",
        "timeoutSeconds",
        "auth",
        "auth.source",
        "auth.type",
        "auth.account",
        "exitCode",
        "errorCode",
        "stdoutBytes",
        "durationMs",
        "authStep",
        "updateNotice",
        "previousVersion",
        "network",
        "network.requestCount",
        "network.status",
        "network.responseBytes",
        "network.requestBodyBytes",
        "network.networkMs",
        "network.morePages",
        "cli",
        "cli.buildChannel",
        "cli.buildId",
        "cli.installMethod",
        "env",
        "env.arch",
        "env.ci",
        "env.agent",
        "env.stdinTty",
        "env.stdoutTty",
    ];

    /// An event with every optional field set, so `skip_serializing_if`
    /// hides nothing from the check below.
    fn fully_populated_event() -> Event {
        let words = || Some(vec!["styles".to_string(), "get".to_string()]);
        Event {
            event: EVENT,
            version: SCHEMA_VERSION,
            created: "2026-09-21T14:13:20.123Z".to_string(),
            event_id: uuid_v4([0x11; 16]),
            parent_event_id: Some(uuid_v4([0x22; 16])),
            user_id: uuid_v4([0x33; 16]),
            sdk_identifier: SDK_IDENTIFIER,
            sdk_version: CURRENT,
            operating_system: "macos",
            command: words(),
            invocation: Some("execute"),
            workflow: Some(Workflow {
                source: "builtin",
                name: Some("style-clone".to_string()),
                step_count: Some(1),
                steps: vec![Step {
                    kind: "cli",
                    command: words(),
                    exit_code: Some(1),
                    error_code: Some("not_found".to_string()),
                    duration_ms: 10,
                }],
            }),
            params: vec![Param {
                name: "data".to_string(),
                value: Some("x".to_string()),
                length: Some(1),
                bytes: Some(2),
                keys: Some(vec!["name".to_string()]),
            }],
            usage_error: Some("missing_argument".to_string()),
            output: Some("json"),
            output_source: Some("flag"),
            dry_run: Some(false),
            debug: Some(false),
            yes: Some(false),
            profile: Some("default"),
            timeout_seconds: Some(30.0),
            auth: Some(Auth {
                source: "env",
                kind: "pk",
                account: Some("someone".to_string()),
            }),
            exit_code: Some(1),
            error_code: Some("not_found".to_string()),
            stdout_bytes: 3,
            duration_ms: 4,
            auth_step: Some("login"),
            update_notice: Some("9.9.9".to_string()),
            previous_version: Some("0.1.0".to_string()),
            network: Some(Network {
                request_count: 1,
                status: Some(404),
                response_bytes: Some(5),
                request_body_bytes: Some(6),
                network_ms: 7,
                more_pages: true,
            }),
            cli: Cli {
                build_channel: Some("release"),
                build_id: Some("abc123"),
                install_method: "cargo",
            },
            env: Env {
                arch: "aarch64",
                ci: false,
                agent: Some("claude-code"),
                stdin_tty: false,
                stdout_tty: false,
            },
        }
    }

    /// Every object key in `value` as a dotted path, with `[]` for an array's
    /// elements. An array of strings is a leaf: its elements have no keys.
    fn field_paths(value: &serde_json::Value, prefix: &str, paths: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(object) => {
                for (key, child) in object {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    field_paths(child, &path, paths);
                    paths.push(path);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    field_paths(item, &format!("{prefix}[]"), paths);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn the_event_sends_only_fields_the_schema_declares() {
        let sent = serde_json::to_value(fully_populated_event()).unwrap();
        let mut paths = vec![];
        field_paths(&sent, "", &mut paths);
        paths.sort();
        paths.dedup();

        let undeclared: Vec<_> = paths
            .iter()
            .filter(|path| !DECLARED_FIELDS.contains(&path.as_str()))
            .collect();
        assert!(
            undeclared.is_empty(),
            "the cli.command event sends fields its schema does not declare:\n  {}\n\n\
             Ingestion accepts undeclared fields, so this test is the only thing holding \
             the promise that nothing else leaves the machine. Add each field to the \
             cli.command schema in mapbox/event-schema first, then to DECLARED_FIELDS.",
            undeclared
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );

        let unset: Vec<_> = DECLARED_FIELDS
            .iter()
            .filter(|path| !paths.iter().any(|p| p == *path))
            .copied()
            .collect();
        assert!(
            unset.is_empty(),
            "these declared fields are missing from the fully populated event:\n  {}\n\n\
             Either fully_populated_event leaves them unset, so the check above can't see \
             them, or the event no longer has them. Set them in the fixture, or remove them \
             from DECLARED_FIELDS once the event has really dropped them.",
            unset.join("\n  ")
        );
    }
}
