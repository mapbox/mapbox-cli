//! Runs a checked workflow, one step at a time.
//!
//! A `command` step is this binary run again as a child, with `--output json`
//! and the step's arguments turned into flags. That is the point rather than
//! a shortcut: the child resolves its token, applies its timeouts, encodes
//! its path segments and records its run exactly as a command typed by hand
//! would, so a workflow cannot reach the API any way a person could not.
//!
//! Only the child's stdout is captured, as the step's output. Its stderr and
//! stdin are the terminal's, so progress, warnings and a confirmation prompt
//! reach the person running the workflow — unless the step feeds `stdin`
//! itself.
//!
//! A `script` step runs its file from the installed workflow's `scripts/`
//! under the interpreter the definition settled on. It is given `MAPBOX_CLI`,
//! the path to this binary, so it can call commands of its own.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use anyhow::{anyhow, Context as _, Result};
use clap::{Arg, ArgAction, Command};
use serde_json::{json, Map, Value};

use super::definition::{Action, InputType, Step, Workflow, DRY_RUN_ENV, SCRIPTS_DIR};
use super::progress::{Display, StepView};
use super::template::{self, Context};
use super::workdir::{self, Workdir};
use crate::auth;
use crate::output::{self, CliError};

/// Global options a step may not set, because the runner owns them or
/// because they would put a credential in a file.
const RESERVED: &[(&str, &str)] = &[
    ("output", "the runner reads every step as JSON"),
    ("quiet", "the runner sets it"),
    (
        "schema",
        "a workflow runs commands rather than describing them",
    ),
    (
        "dry-run",
        "use `mapbox workflow run --dry-run` for the whole workflow",
    ),
    (
        "token",
        "a token written into a workflow is a secret in a file; use `profile`",
    ),
    (
        "yes",
        "only the person running it can skip a confirmation, with `mapbox workflow run --yes`",
    ),
];

/// The globals of the `workflow run` line that each command step inherits
/// unless it sets its own.
#[derive(Debug, Default, Clone)]
pub struct Inherited {
    /// Only a token typed as `--token`; one from the environment reaches the
    /// child by being in its environment already.
    pub typed_token: Option<String>,
    pub options: Vec<(&'static str, String)>,
    pub flags: Vec<&'static str>,
    /// `--quiet`: steps show their title and how they ended, and warnings,
    /// but not their details.
    pub quiet: bool,
}

impl Inherited {
    pub fn from_matches(matches: &clap::ArgMatches) -> Self {
        let mut inherited = Inherited {
            typed_token: auth::typed_token(matches),
            quiet: matches.get_flag(output::banner::ARG),
            ..Default::default()
        };
        for option in ["profile", "username", crate::http::TIMEOUT_ARG] {
            let typed =
                matches.value_source(option) == Some(clap::parser::ValueSource::CommandLine);
            // Raw, as typed: `--timeout` is parsed into a `Duration`, and
            // `get_one::<String>` on it panics.
            let value = matches
                .get_raw(option)
                .and_then(|mut values| values.next())
                .and_then(|value| value.to_str());
            if let (true, Some(value)) = (typed, value) {
                inherited.options.push((option, value.to_string()));
            }
        }
        for flag in ["use-login", "debug", crate::confirm::ARG] {
            if matches.get_flag(flag) {
                inherited.flags.push(flag);
            }
        }
        inherited
    }
}

/// The flag an input is given as: its name, dashed, as every other flag in
/// this CLI is spelled. `--style_id` is accepted too, as an alias.
pub fn input_flag(name: &str) -> String {
    name.replace('_', "-")
}

/// Turns the values given for a workflow's inputs, by input name, into its
/// inputs typed as the definition declares, with defaults filled in. The
/// parser has already checked each value's shape; this still does, so it
/// can be trusted on its own.
pub fn read_inputs(workflow: &Workflow, given: &[(String, String)]) -> Result<Map<String, Value>> {
    let mut inputs: Map<String, Value> = Map::new();
    for (key, raw) in given {
        let flag = input_flag(key);
        let Some(spec) = workflow.inputs.get(key) else {
            return Err(invalid_input(
                format!("`{}` has no input called `{key}`", workflow.name),
                workflow,
            ));
        };
        let value = match spec.kind {
            InputType::String => Value::String(raw.to_string()),
            InputType::Number => parse_number(raw).ok_or_else(|| {
                invalid_input(format!("`--{flag}` is a number, not `{raw}`"), workflow)
            })?,
            InputType::Boolean => match raw.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => {
                    return Err(invalid_input(
                        format!("`--{flag}` is `true` or `false`, not `{raw}`"),
                        workflow,
                    ))
                }
            },
        };
        inputs.insert(key.to_string(), value);
    }

    let mut missing = vec![];
    for (name, spec) in &workflow.inputs {
        if inputs.contains_key(name) {
            continue;
        }
        match &spec.default {
            Some(default) => {
                inputs.insert(name.clone(), default.clone());
            }
            None if spec.required => missing.push(format!("--{}", input_flag(name))),
            None => {
                inputs.insert(name.clone(), Value::Null);
            }
        }
    }
    if !missing.is_empty() {
        return Err(invalid_input(
            format!("missing required input: {}", missing.join(" ")),
            workflow,
        ));
    }
    Ok(inputs)
}

pub fn parse_number(raw: &str) -> Option<Value> {
    raw.parse::<i64>()
        .map(Value::from)
        .ok()
        .or_else(|| raw.parse::<f64>().ok().map(Value::from))
}

fn invalid_input(message: String, workflow: &Workflow) -> anyhow::Error {
    CliError::new("invalid_input", message)
        .with_remedy(
            crate::remedy::Remedy::default()
                .with_action(Some(format!("mapbox workflow show {}", workflow.name))),
        )
        .into()
}

/// What `--dry-run` prints: the plan, with the inputs resolved and every
/// later reference left as written, since nothing has run to fill it.
pub fn plan(workflow: &Workflow, inputs: &Map<String, Value>) -> Value {
    let steps: Vec<Value> = workflow
        .steps
        .iter()
        .map(|step| {
            let (kind, args) = match &step.action {
                Action::Command { args, .. } => ("command", Value::Object(args.clone())),
                Action::Script { args, .. } => ("script", Value::Array(args.clone())),
            };
            json!({
                "id": step.id,
                "kind": kind,
                "run": step.label(),
                "args": args,
                "stdin": step.stdin,
                "dry_run": step.dry_run,
            })
        })
        .collect();
    json!({ "workflow": workflow.name, "inputs": inputs, "steps": steps, "outputs": workflow.outputs })
}

/// What a run ends with.
pub struct Finished {
    /// What `outputs` resolves to, or the last step's output when the
    /// workflow declares none.
    pub outputs: Value,
    /// What `result` resolves to, when the workflow declares one.
    pub text: Option<String>,
}

/// Runs every step.
pub fn run(
    app: &Command,
    workflow: &Workflow,
    root: &Path,
    inputs: &Map<String, Value>,
    inherited: &Inherited,
) -> Result<Finished> {
    let outputs = run_steps(app, workflow, root, inputs, inherited, false)?;
    let context = Context {
        inputs,
        steps: &outputs,
    };
    let text = match &workflow.result {
        Some(template) => Some(
            template::resolve(&Value::String(template.clone()), &context)
                .map(|value| template::as_text(&value).unwrap_or_default())
                .map_err(|e| CliError::new("workflow_failed", format!("`result`: {e:#}")))?,
        ),
        None => None,
    };

    let last = workflow
        .steps
        .last()
        .and_then(|step| outputs.get(&step.id))
        .cloned()
        .unwrap_or(Value::Null);
    let outputs = match &workflow.outputs {
        Some(declared) => template::resolve(declared, &context)
            .map_err(|e| CliError::new("workflow_failed", format!("`outputs`: {e:#}")))?,
        None => last,
    };
    Ok(Finished { outputs, text })
}

/// Runs only the steps marked `dry_run`, each with [`DRY_RUN_ENV`] set, and
/// returns their outputs by step id. `outputs` is not resolved: the steps it
/// reads may not have run.
pub fn dry_run(
    app: &Command,
    workflow: &Workflow,
    root: &Path,
    inputs: &Map<String, Value>,
    inherited: &Inherited,
) -> Result<BTreeMap<String, Value>> {
    run_steps(app, workflow, root, inputs, inherited, true)
}

fn run_steps(
    app: &Command,
    workflow: &Workflow,
    root: &Path,
    inputs: &Map<String, Value>,
    inherited: &Inherited,
    dry_run: bool,
) -> Result<BTreeMap<String, Value>> {
    let exe = std::env::current_exe().context("Could not find this program's own path")?;
    catch_interrupt();
    let workdir = Workdir::create()?;
    let mut outputs: BTreeMap<String, Value> = BTreeMap::new();
    let total = workflow.steps.len();

    let display = Display::for_stderr(total, inherited.quiet);
    for (index, step) in workflow.steps.iter().enumerate() {
        // A Ctrl-C between steps reached no child, so nothing else stops the
        // next one from starting.
        if INTERRUPTED.load(Ordering::SeqCst) {
            return Err(cancelled(&format!("before step `{}`", step.id)));
        }
        let title = step.name.as_deref().unwrap_or(&step.id);
        if dry_run && !step.dry_run {
            display.skipped(index, title, &step.label());
            continue;
        }

        let context = Context {
            inputs,
            steps: &outputs,
        };
        let stdin = step
            .stdin
            .as_ref()
            .map(|value| template::resolve(value, &context))
            .transpose()
            .map_err(|e| step_failure(step, e))?;

        let mut process = match &step.action {
            Action::Command { path, args } => {
                let args = match template::resolve(&Value::Object(args.clone()), &context)
                    .map_err(|e| step_failure(step, e))?
                {
                    Value::Object(args) => args,
                    _ => unreachable!("an object resolves to an object"),
                };
                let argv =
                    command_argv(app, path, &args, inherited).map_err(|e| step_failure(step, e))?;
                let mut process = Process::new(&exe);
                process.args(argv);
                process
            }
            Action::Script {
                script,
                interpreter,
                args,
            } => {
                let args = template::resolve(&Value::Array(args.clone()), &context)
                    .map_err(|e| step_failure(step, e))?;
                let mut process = Process::new(interpreter);
                process.arg(root.join(SCRIPTS_DIR).join(script));
                for value in args.as_array().into_iter().flatten() {
                    process.arg(template::as_text(value).unwrap_or_default());
                }
                process
                    .env("MAPBOX_CLI", &exe)
                    .env("MAPBOX_WORKFLOW_ROOT", root)
                    .env(workdir::ENV, workdir.path());
                if dry_run {
                    process.env(DRY_RUN_ENV, "1");
                }
                process
            }
        };
        if let Some(token) = &inherited.typed_token {
            process.env(auth::CLAP_TOKEN_ENV, token);
        }

        let is_script = matches!(step.action, Action::Script { .. });
        // A command step hands its error document back through a file, so
        // the run can carry its code rather than print it raw.
        let error_file =
            (!is_script).then(|| workdir.path().join(format!(".error-{}.json", step.id)));
        if let Some(path) = &error_file {
            process.env(output::STEP_ERROR_ENV, path);
        }
        let view = display.start(index, title, &step.label(), is_script);
        let result = execute(
            &mut process,
            stdin.as_ref(),
            step,
            &view,
            workdir.path(),
            error_file.as_deref(),
        );
        let result = match &inherited.typed_token {
            Some(_) => result.map_err(auth::with_typed_token_fix),
            None => result,
        };
        let interrupted = interrupted(result.is_err());
        view.finish(result.is_ok() && !interrupted);
        if interrupted {
            return Err(cancelled(&format!("during step `{}`", step.id)));
        }
        outputs.insert(step.id.clone(), result?);
    }

    display.finish_run();
    Ok(outputs)
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// The running step's process id, or 0 between steps.
static CURRENT_CHILD: AtomicU32 = AtomicU32::new(0);

/// How long a step gets to stop on its own before the signal is passed on.
#[cfg(unix)]
const FORWARD_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

/// Ctrl-C reaches the running step's child as well as this process, and so
/// does the SIGHUP of a closed terminal; a SIGTERM reaches only this one.
/// The child stops; this process only notes it and fails once the child has
/// gone, so the run's [`Workdir`] is dropped rather than left in place by a
/// process that died holding it.
///
/// For the SIGTERM a CI timeout sends, a step still running after
/// [`FORWARD_AFTER`] is sent a SIGINT: SIGINT rather than SIGTERM so a
/// script can still report what it did, as `copy-style` does on a
/// `KeyboardInterrupt`. `ctrlc` does not say which signal arrived, so
/// nothing is passed on while this process is the terminal's foreground: a
/// Ctrl-C there reached the step already, and a second one could cut that
/// report short. Only the step's own process is signaled, not anything it
/// started.
fn catch_interrupt() {
    // On failure Ctrl-C keeps its default, which stops the run as before
    // but leaves the directory behind; not worth failing the run over.
    let _ = ctrlc::set_handler(|| {
        INTERRUPTED.store(true, Ordering::SeqCst);
        #[cfg(unix)]
        {
            if is_terminal_foreground() {
                return;
            }
            std::thread::sleep(FORWARD_AFTER);
            // A process id can be reused once its process is reaped. The
            // window is the moment between the reap and the store of 0
            // after it, which is as narrow as this gets without `unsafe`.
            let pid = CURRENT_CHILD.load(Ordering::SeqCst);
            if let Ok(pid) = i32::try_from(pid) {
                if pid > 0 {
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(pid),
                        nix::sys::signal::Signal::SIGINT,
                    );
                }
            }
        }
    });
}

/// Whether this process's group is its terminal's foreground, the one a
/// Ctrl-C reaches. No terminal, as in CI, is a no.
#[cfg(unix)]
fn is_terminal_foreground() -> bool {
    use nix::unistd::{getpgrp, tcgetpgrp};
    let foreground = tcgetpgrp(std::io::stdin()).or_else(|_| tcgetpgrp(std::io::stderr()));
    foreground == Ok(getpgrp())
}

fn cancelled(when: &str) -> anyhow::Error {
    CliError::new(
        "cancelled",
        format!("Interrupted {when}; the steps after it did not run."),
    )
    .into()
}

/// Whether Ctrl-C was pressed. `ctrlc` runs the handler on a thread of its
/// own, so a child stopped by the same Ctrl-C can be reaped before the flag
/// is set; a step that failed waits a moment for it, so the run says it was
/// interrupted rather than that the step failed.
fn interrupted(step_failed: bool) -> bool {
    let grace = if step_failed {
        std::time::Duration::from_millis(250)
    } else {
        std::time::Duration::ZERO
    };
    let deadline = std::time::Instant::now() + grace;
    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn size(bytes: usize) -> String {
    match bytes {
        0..=1023 => format!("{bytes} bytes"),
        1024..=1_048_575 => format!("{} KB", bytes / 1024),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

fn step_failure(step: &Step, err: anyhow::Error) -> anyhow::Error {
    CliError::new(
        "workflow_failed",
        format!("Step `{}` ({}): {err:#}", step.id, step.label()),
    )
    .into()
}

/// A command step's failure, built from the error document the child wrote.
/// Still `workflow_failed`, so a caller knows the run stopped; the child's
/// own document is its `cause`, so a caller can branch on its `code`, and its
/// status and advice are lifted onto this one for a person to read.
fn command_failure(step: &Step, cause: Value) -> anyhow::Error {
    let text = |key: &str| cause.get(key).and_then(Value::as_str).map(String::from);
    let list = |key: &str| -> Vec<String> {
        cause
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut error = CliError::new(
        "workflow_failed",
        format!(
            "Step `{}` ({}): {}",
            step.id,
            step.label(),
            text("message").unwrap_or_else(|| "failed".to_string())
        ),
    );
    error.status = cause
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok());
    error.fix = text("fix");
    error.next_actions = list("next_actions");
    error.docs = list("docs");
    error.request_id = text("request_id");
    error.cause = Some(cause);
    error.into()
}

/// Spawns one step and reads its stdout as its output: JSON when it parses,
/// the text otherwise, nothing when there is none.
fn execute(
    process: &mut Process,
    stdin: Option<&Value>,
    step: &Step,
    view: &StepView,
    workdir: &Path,
    error_file: Option<&Path>,
) -> Result<Value> {
    // A script with nothing to read gets nothing, rather than a terminal it
    // might block on. A command keeps the terminal, which is where a
    // confirmation prompt reads its answer.
    let input = match (stdin, &step.action) {
        (Some(_), _) => Stdio::piped(),
        (None, Action::Script { .. }) => Stdio::null(),
        (None, Action::Command { .. }) => Stdio::inherit(),
    };
    // A script's stderr is read line by line, for its progress and details.
    // A command's is the terminal's, so a confirmation prompt still shows.
    let errors = match step.action {
        Action::Script { .. } => Stdio::piped(),
        Action::Command { .. } => Stdio::inherit(),
    };
    let mut child = process
        .stdin(input)
        .stdout(Stdio::piped())
        .stderr(errors)
        .spawn()
        .map_err(|e| {
            let program = process.get_program().to_string_lossy().into_owned();
            let message = if e.kind() == std::io::ErrorKind::NotFound {
                format!("`{program}` is not installed or not on PATH")
            } else {
                format!("could not start `{program}`: {e}")
            };
            step_failure(step, anyhow!(message))
        })?;

    // Written from a thread: a child that fills its stdout pipe before it
    // has read all of stdin would otherwise wait on us while we wait on it.
    let writer = stdin.map(|value| {
        let body = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let mut pipe = child.stdin.take().expect("stdin was piped");
        std::thread::spawn(move || {
            // A child that exits without reading is reported by its status.
            let _ = pipe.write_all(body.as_bytes());
        })
    });
    let reader = child.stderr.take().map(|pipe| {
        let reporter = view.reporter();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                reporter.report(&line);
            }
        })
    });
    CURRENT_CHILD.store(child.id(), Ordering::SeqCst);
    let finished = child.wait_with_output();
    CURRENT_CHILD.store(0, Ordering::SeqCst);
    let finished =
        finished.map_err(|e| step_failure(step, anyhow!("could not wait for it: {e}")))?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    // Every line the step wrote is shown before its outcome is.
    if let Some(reader) = reader {
        let _ = reader.join();
    }

    if !finished.status.success() {
        let cause = error_file
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|cause| cause.get("code").is_some_and(Value::is_string));
        if let Some(cause) = cause {
            return Err(command_failure(step, cause));
        }
        let how = match finished.status.code() {
            Some(code) => format!("exited with {code}"),
            None => "was stopped by a signal".to_string(),
        };
        return Err(step_failure(
            step,
            anyhow!("{how}; the workflow stopped here"),
        ));
    }

    if let Some(save) = &step.save {
        let path = workdir.join(save);
        std::fs::write(&path, &finished.stdout)
            .map_err(|e| step_failure(step, anyhow!("could not save {}: {e}", path.display())))?;
        let bytes = finished.stdout.len();
        view.reporter()
            .report(&format!("Saved {save} ({})", size(bytes)));
        return Ok(json!({ "path": path, "bytes": bytes }));
    }

    let text = String::from_utf8(finished.stdout).map_err(|_| {
        step_failure(
            step,
            anyhow!(
                "its output is binary, which cannot be passed to another step; give it \
                 `save: <file name>` to keep it as a file instead"
            ),
        )
    })?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(Value::Null);
    }
    Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string())))
}

enum ArgShape {
    Flag,
    Option { repeatable: bool },
    Positional { repeatable: bool },
}

/// The leaf command a step names, when it names one a workflow may run.
fn leaf<'a>(app: &'a Command, path: &[String]) -> Result<&'a Command, String> {
    let named = format!("mapbox {}", path.join(" "));
    if path.first().map(String::as_str) == Some(super::COMMAND) {
        return Err(format!("`{named}`: a workflow cannot run another workflow"));
    }
    let mut current = app;
    for segment in path {
        current = current
            .find_subcommand(segment)
            .ok_or_else(|| format!("`{named}` is not a command"))?;
    }
    if current.has_subcommands() {
        return Err(format!("`{named}` is a group; name one of its commands"));
    }
    Ok(current)
}

/// The argument a step's key names: one of the command's own, by its long
/// flag or, for a positional, its name — the same names `mapbox --schema`
/// lists — or a global option.
fn find_arg<'a>(
    app: &'a Command,
    command: &'a Command,
    key: &str,
) -> Result<(&'a Arg, ArgShape), String> {
    if let Some((_, why)) = RESERVED.iter().find(|(name, _)| *name == key) {
        return Err(format!("`{key}` cannot be set by a step: {why}"));
    }
    let own = command
        .get_arguments()
        .find(|arg| arg.get_long() == Some(key) || (arg.is_positional() && arg.get_id() == key));
    let arg = own
        .or_else(|| {
            app.get_arguments()
                .find(|arg| arg.is_global_set() && arg.get_long() == Some(key))
        })
        .ok_or_else(|| format!("`{key}` is not an argument of this command"))?;
    let repeatable = matches!(arg.get_action(), ArgAction::Append);
    // A generated boolean flag also takes `=false`, but a step's `false`
    // still leaves it out: an API that reads presence rather than value
    // would take `x=false` as on, and leaving it out is what `false` has
    // always meant here.
    let takes_false = arg.is_require_equals_set() && arg.get_num_args() == Some((0..=1).into());
    let shape =
        if takes_false || matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse) {
            ArgShape::Flag
        } else if arg.is_positional() {
            ArgShape::Positional { repeatable }
        } else {
            ArgShape::Option { repeatable }
        };
    Ok((arg, shape))
}

/// Whether a CLI at `current` satisfies `requires`, comparing numbers only:
/// a dev build of 0.4.0 is taken as 0.4.0, since a workflow requiring it was
/// most likely written against that build.
fn too_old(requires: &str, current: &str) -> Option<String> {
    let parse = crate::update_check::parse_version;
    let (Some(needed), Some(this)) = (parse(requires), parse(current)) else {
        return None;
    };
    ((this.0, this.1, this.2) < (needed.0, needed.1, needed.2)).then(|| {
        format!("it needs mapbox {requires} or later, and this is {current}; update the CLI")
    })
}

/// Every `command` step's problems against this binary's command tree —
/// checked at install and again before a run, since the CLI may have been
/// upgraded underneath an installed workflow.
pub fn command_problems(app: &Command, workflow: &Workflow) -> Vec<String> {
    // Alone when it fails: an older CLI's other complaints are symptoms of
    // the same thing, and a script step's needs show up nowhere else.
    if let Some(problem) = workflow
        .requires
        .as_deref()
        .and_then(|requires| too_old(requires, env!("CARGO_PKG_VERSION")))
    {
        return vec![problem];
    }

    let mut problems = vec![];
    let read_only = OnceCell::new();

    // An input is given as a flag beside the globals, so it cannot take one
    // of their names — clap would refuse to build the command at all.
    let taken: Vec<&str> = app
        .get_arguments()
        .filter(|arg| arg.is_global_set())
        .filter_map(Arg::get_long)
        .chain([crate::executor::DRY_RUN_ARG, "help", "version"])
        .collect();
    for name in workflow.inputs.keys() {
        let flag = input_flag(name);
        if taken.contains(&flag.as_str()) {
            problems.push(format!(
                "input `{name}` would be given as `--{flag}`, which is already an option \
                 of every command; rename it"
            ));
        }
    }
    for step in &workflow.steps {
        let Action::Command { path, args } = &step.action else {
            continue;
        };
        let command = match leaf(app, path) {
            Ok(command) => command,
            Err(e) => {
                problems.push(format!("step `{}`: {e}", step.id));
                continue;
            }
        };
        if step.dry_run
            && !read_only
                .get_or_init(read_only_commands)
                .contains(&canonical(app, path))
        {
            problems.push(format!(
                "step `{}`: `dry_run` needs a command known to change nothing, and `mapbox {}` \
                 is not one",
                step.id,
                path.join(" ")
            ));
        }
        for (key, value) in args {
            match find_arg(app, command, key) {
                Err(e) => problems.push(format!("step `{}`: {e}", step.id)),
                Ok((_, ArgShape::Flag)) if !(value.is_boolean() || value.is_string()) => problems
                    .push(format!(
                        "step `{}`: `{key}` is a flag; give it true or false",
                        step.id
                    )),
                Ok(_) => {}
            }
        }
    }
    problems
}

/// Hand-written commands a `dry_run` step may run: each reads local state or
/// sends only a `GET`. Kept as a list rather than inferred, because what a
/// command lacks says nothing about what it does — `tilesets-cli` has no
/// `--dry-run` either, and hands its arguments to a program that may write.
const READ_ONLY_COMMANDS: &[&str] = &[
    "auth whoami",
    "auth profiles",
    "config get",
    "config list",
    "history list",
    "history show",
];

/// Every command a dry run may send: [`READ_ONLY_COMMANDS`] and every API
/// operation whose method is `GET`.
fn read_only_commands() -> Vec<String> {
    let mut commands: Vec<String> = READ_ONLY_COMMANDS.iter().map(|c| c.to_string()).collect();
    // The tree being checked was built from these, so a failure here has
    // already been reported by the parse; nothing is then allowed.
    if let Ok(specs) = crate::spec::effective_services() {
        commands.extend(
            specs
                .iter()
                .flat_map(|spec| &spec.operations)
                .filter(|op| op.method.eq_ignore_ascii_case("GET"))
                .map(|op| op.command()),
        );
    }
    commands
}

/// `path` with each alias replaced by the command's own name, so `auth
/// status` is checked as the `auth whoami` it is.
fn canonical(app: &Command, path: &[String]) -> String {
    let mut current = app;
    let mut names = vec![];
    for segment in path {
        let Some(next) = current.find_subcommand(segment) else {
            break;
        };
        names.push(next.get_name().to_string());
        current = next;
    }
    names.join(" ")
}

/// A step's arguments as the child's command line.
///
/// Options are spelled `--name=value`, so a value that starts with a dash —
/// a western longitude — is never read as a flag. Positionals come last,
/// after `--`, for the same reason.
fn command_argv(
    app: &Command,
    path: &[String],
    args: &Map<String, Value>,
    inherited: &Inherited,
) -> Result<Vec<OsString>> {
    let command = leaf(app, path).map_err(|e| anyhow!(e))?;
    let mut argv: Vec<OsString> = path.iter().map(OsString::from).collect();
    let mut positionals: Vec<(usize, OsString)> = vec![];

    for (key, value) in args {
        let (arg, shape) = find_arg(app, command, key).map_err(|e| anyhow!(e))?;
        let values: Vec<&Value> = match value {
            Value::Array(items) if !matches!(shape, ArgShape::Flag) => items.iter().collect(),
            Value::Null => continue,
            other => vec![other],
        };
        match shape {
            ArgShape::Flag => match value {
                Value::Bool(true) => argv.push(format!("--{key}").into()),
                Value::Bool(false) => {}
                Value::String(text) if text == "true" => argv.push(format!("--{key}").into()),
                Value::String(text) if text == "false" => {}
                _ => return Err(anyhow!("`{key}` is a flag; give it true or false")),
            },
            ArgShape::Option { repeatable } | ArgShape::Positional { repeatable }
                if values.len() > 1 && !repeatable =>
            {
                return Err(anyhow!("`{key}` takes one value, not a list"));
            }
            ArgShape::Option { .. } => {
                for value in values {
                    let text = template::as_text(value).unwrap_or_default();
                    argv.push(format!("--{key}={text}").into());
                }
            }
            ArgShape::Positional { .. } => {
                let index = arg.get_index().unwrap_or(usize::MAX);
                for value in values {
                    positionals.push((index, template::as_text(value).unwrap_or_default().into()));
                }
            }
        }
    }

    for (option, value) in &inherited.options {
        if !args.contains_key(*option) {
            argv.push(format!("--{option}={value}").into());
        }
    }
    for flag in &inherited.flags {
        if !args.contains_key(*flag) {
            argv.push(format!("--{flag}").into());
        }
    }
    argv.push(format!("--{}={}", output::ARG, output::JSON).into());
    argv.push(format!("--{}", output::banner::ARG).into());

    if !positionals.is_empty() {
        positionals.sort_by_key(|(index, _)| *index);
        argv.push("--".into());
        argv.extend(positionals.into_iter().map(|(_, value)| value));
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::{parse, Files, DEFINITION_FILE};
    use std::path::PathBuf;

    fn app() -> Command {
        let specs = crate::spec::effective_services().expect("bundled specs");
        crate::build_app(&specs)
    }

    fn workflow(steps: &str) -> Workflow {
        let yaml = format!(
            "schema: 1\nname: t\nsummary: t\ninputs:\n  n: {{ type: number, default: 2 }}\n  flag: {{ type: boolean, default: false }}\nsteps:\n{steps}"
        );
        let files: Files = [(PathBuf::from(DEFINITION_FILE), yaml.into_bytes())].into();
        parse("t", &files).expect("valid")
    }

    /// A dry run sends every step it runs, so a command step can be in one
    /// only if its command is known to change nothing.
    #[test]
    fn only_a_command_known_to_change_nothing_runs_in_a_dry_run() {
        let found = command_problems(
            &app(),
            &workflow(
                "  - id: read\n    command: styles get\n    args: { style-id: x }\n    dry_run: true\n\
                 \x20 - id: local\n    command: config list\n    dry_run: true\n\
                 \x20 - id: alias\n    command: auth status\n    dry_run: true\n\
                 \x20 - id: write\n    command: styles create\n    dry_run: true\n\
                 \x20 - id: passthrough\n    command: tilesets-cli\n    dry_run: true\n",
            ),
        )
        .join("\n");
        for refused in ["write", "passthrough"] {
            assert!(
                found.contains(&format!(
                    "step `{refused}`: `dry_run` needs a command known to change nothing"
                )),
                "{found}"
            );
        }
        for allowed in ["read", "local", "alias"] {
            assert!(!found.contains(&format!("step `{allowed}`")), "{found}");
        }
    }

    /// The list names real commands; one renamed out from under it would
    /// silently stop being allowed.
    #[test]
    fn every_read_only_command_exists() {
        let app = app();
        for command in READ_ONLY_COMMANDS {
            let path: Vec<String> = command.split(' ').map(String::from).collect();
            assert_eq!(canonical(&app, &path), *command, "{command}");
            assert!(leaf(&app, &path).is_ok(), "{command}");
        }
    }

    #[test]
    fn a_step_is_checked_against_the_real_command_tree() {
        let found = command_problems(
            &app(),
            &workflow(
                "  - id: a\n    command: styles nope\n\
                 \x20 - id: b\n    command: styles\n\
                 \x20 - id: c\n    command: styles get\n    args: { bogus: 1, token: x }\n\
                 \x20 - id: d\n    command: workflow list\n\
                 \x20 - id: e\n    command: styles delete\n    args: { style-id: x, yes: true }\n",
            ),
        );
        let joined = found.join("\n");
        assert!(
            joined.contains("step `e`") && joined.contains("only the person running it"),
            "{joined}"
        );
        assert!(
            joined.contains("`mapbox styles nope` is not a command"),
            "{joined}"
        );
        assert!(joined.contains("is a group"), "{joined}");
        assert!(joined.contains("`bogus` is not an argument"), "{joined}");
        assert!(joined.contains("a secret in a file"), "{joined}");
        assert!(joined.contains("cannot run another workflow"), "{joined}");
    }

    /// A step is handed a typed `--token` through the environment, so its
    /// own advice for a 401 names the variable rather than the flag.
    #[test]
    fn a_typed_token_is_blamed_on_the_flag_not_the_environment() {
        let step = &workflow("  - id: list\n    command: styles list\n").steps[0];
        let cause = serde_json::json!({
            "code": "http_401",
            "message": "Not Authorized - Invalid Token",
            "status": 401,
            "fix": "The token came from MAPBOX_ACCESS_TOKEN. Run `mapbox auth login` to sign in with an account instead.",
            "next_actions": ["mapbox auth login"],
        });
        let err = auth::with_typed_token_fix(command_failure(step, cause));
        let err = err.downcast::<CliError>().expect("a CliError");
        let fix = err.fix.as_deref().unwrap_or_default();
        assert!(fix.contains("`--token`"), "{fix}");
        assert!(!fix.contains("came from MAPBOX_ACCESS_TOKEN"), "{fix}");
        assert_eq!(err.next_actions, ["mapbox auth whoami"]);
        assert_eq!(err.cause.as_ref().unwrap()["code"], "http_401");

        let cause = serde_json::json!({
            "code": "http_403",
            "message": "This API requires a token with styles:download scope.",
            "status": 403,
            "fix": "The token from MAPBOX_ACCESS_TOKEN lacks the `styles:download` scope.",
        });
        let err = auth::with_typed_token_fix(command_failure(step, cause));
        let fix = err.downcast::<CliError>().unwrap().fix.unwrap_or_default();
        assert!(
            fix.contains("passed with `--token` lacks the `styles:download`"),
            "{fix}"
        );
    }

    /// Through the real parser, which is where `--timeout` stops being a
    /// string.
    #[test]
    fn typed_globals_are_inherited_as_typed() {
        for args in [
            [
                "mapbox",
                "--timeout",
                "30",
                "--profile",
                "work",
                "workflow",
                "list",
            ],
            [
                "mapbox",
                "workflow",
                "list",
                "--timeout",
                "30",
                "--profile",
                "work",
            ],
        ] {
            let matches = app().try_get_matches_from(args).expect("parses");
            let inherited = Inherited::from_matches(&matches);
            assert_eq!(
                inherited.options,
                [
                    ("profile", "work".to_string()),
                    ("timeout", "30".to_string())
                ],
                "{args:?}"
            );
        }
    }

    #[test]
    fn arguments_become_flags_and_positionals_come_last() {
        let app = app();
        let args = serde_json::json!({
            "style-id": "-abc",
            "optimize": true,
            "download": false,
            "profile": "work",
        });
        let inherited = Inherited {
            options: vec![("profile", "ignored".into()), ("timeout", "30".into())],
            flags: vec!["yes"],
            ..Default::default()
        };
        let argv = command_argv(
            &app,
            &["styles".into(), "get".into()],
            args.as_object().unwrap(),
            &inherited,
        )
        .unwrap();
        let argv: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            [
                "styles",
                "get",
                "--optimize",
                "--profile=work",
                "--timeout=30",
                "--yes",
                "--output=json",
                "--quiet",
                "--",
                "-abc"
            ]
        );
    }

    #[test]
    fn inputs_are_typed_and_defaulted() {
        let wf = workflow("  - id: a\n    command: styles list\n");
        let given = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let inputs = read_inputs(&wf, &given(&[("n", "5")])).unwrap();
        assert_eq!(inputs["n"], serde_json::json!(5));
        assert_eq!(inputs["flag"], serde_json::json!(false));
        assert!(read_inputs(&wf, &given(&[("n", "five")])).is_err());
        assert!(read_inputs(&wf, &given(&[("nope", "1")])).is_err());
    }

    #[test]
    fn an_input_cannot_take_a_global_flag() {
        let yaml = "schema: 1\nname: t\nsummary: t\ninputs:\n  profile: { type: string }\n  dry_run: { type: boolean }\nsteps:\n  - id: a\n    command: styles list\n";
        let files: Files = [(PathBuf::from(DEFINITION_FILE), yaml.as_bytes().to_vec())].into();
        let found = command_problems(&app(), &parse("t", &files).unwrap()).join("\n");
        assert!(found.contains("`--profile`"), "{found}");
        assert!(found.contains("`--dry-run`"), "{found}");
    }

    #[test]
    fn requires_compares_release_numbers() {
        assert!(too_old("0.4.0", "0.3.9").is_some());
        assert!(too_old("0.4.0", "0.4.0").is_none());
        assert!(too_old("0.4.0", "0.4.0-dev.abc1234").is_none());
        assert!(too_old("0.4.0", "1.0.0").is_none());
    }

    /// A workflow for a newer CLI is refused with that reason alone, even
    /// though its unknown command would be refused too.
    #[test]
    fn a_newer_cli_requirement_is_the_only_problem_reported() {
        let yaml = "schema: 1\nname: t\nrequires: '>=999.0.0'\nsummary: t\nsteps:\n  - id: a\n    command: no such\n";
        let files: Files = [(PathBuf::from(DEFINITION_FILE), yaml.as_bytes().to_vec())].into();
        let found = command_problems(&app(), &parse("t", &files).unwrap());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("needs mapbox 999.0.0 or later"),
            "{found:?}"
        );
    }
}
