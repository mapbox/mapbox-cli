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

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::{Command as Process, Stdio};

use anyhow::{anyhow, Context as _, Result};
use clap::{Arg, ArgAction, Command};
use serde_json::{json, Map, Value};

use super::definition::{Action, InputType, Step, Workflow, DRY_RUN_ENV, SCRIPTS_DIR};
use super::template::{self, Context};
use crate::auth;
use crate::output::{self, style, CliError};

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
}

impl Inherited {
    pub fn from_matches(matches: &clap::ArgMatches) -> Self {
        let mut inherited = Inherited {
            typed_token: auth::typed_token(matches),
            ..Default::default()
        };
        for option in ["profile", "username", crate::http::TIMEOUT_ARG] {
            let typed =
                matches.value_source(option) == Some(clap::parser::ValueSource::CommandLine);
            if let (true, Some(value)) = (typed, matches.get_one::<String>(option)) {
                inherited.options.push((option, value.clone()));
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

/// Runs every step and returns what `outputs` resolves to, or the last
/// step's output when the workflow declares none.
pub fn run(
    app: &Command,
    workflow: &Workflow,
    root: &Path,
    inputs: &Map<String, Value>,
    inherited: &Inherited,
) -> Result<Value> {
    let outputs = run_steps(app, workflow, root, inputs, inherited, false)?;

    let last = workflow
        .steps
        .last()
        .and_then(|step| outputs.get(&step.id))
        .cloned()
        .unwrap_or(Value::Null);
    match &workflow.outputs {
        Some(declared) => template::resolve(
            declared,
            &Context {
                inputs,
                steps: &outputs,
            },
        )
        .map_err(|e| CliError::new("workflow_failed", format!("`outputs`: {e:#}")).into()),
        None => Ok(last),
    }
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
    let mut outputs: BTreeMap<String, Value> = BTreeMap::new();
    let total = workflow.steps.len();

    let color = style::enabled(std::io::stderr().is_terminal());
    for (index, step) in workflow.steps.iter().enumerate() {
        let title = step.name.as_deref().unwrap_or(&step.id);
        let skipped = dry_run && !step.dry_run;
        let note = if skipped {
            format!("({}, skipped in a dry run)", step.label())
        } else {
            format!("({})", step.label())
        };
        output::progress(&format!(
            "{} {} {}",
            style::paint(&format!("[{}/{total}]", index + 1), style::DIM, color),
            style::paint(title, style::BOLD, color),
            style::paint(&note, style::DIM, color),
        ));
        if skipped {
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
                    .env("MAPBOX_WORKFLOW_ROOT", root);
                if dry_run {
                    process.env(DRY_RUN_ENV, "1");
                }
                process
            }
        };
        if let Some(token) = &inherited.typed_token {
            process.env(auth::CLAP_TOKEN_ENV, token);
        }

        let value = execute(&mut process, stdin.as_ref(), step)?;
        outputs.insert(step.id.clone(), value);
    }

    Ok(outputs)
}

fn step_failure(step: &Step, err: anyhow::Error) -> anyhow::Error {
    CliError::new(
        "workflow_failed",
        format!("Step `{}` ({}): {err:#}", step.id, step.label()),
    )
    .into()
}

/// Spawns one step and reads its stdout as its output: JSON when it parses,
/// the text otherwise, nothing when there is none.
fn execute(process: &mut Process, stdin: Option<&Value>, step: &Step) -> Result<Value> {
    // A script with nothing to read gets nothing, rather than a terminal it
    // might block on. A command keeps the terminal, which is where a
    // confirmation prompt reads its answer.
    let input = match (stdin, &step.action) {
        (Some(_), _) => Stdio::piped(),
        (None, Action::Script { .. }) => Stdio::null(),
        (None, Action::Command { .. }) => Stdio::inherit(),
    };
    let mut child = process
        .stdin(input)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
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
    let finished = child
        .wait_with_output()
        .map_err(|e| step_failure(step, anyhow!("could not wait for it: {e}")))?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }

    if !finished.status.success() {
        let how = match finished.status.code() {
            Some(code) => format!("exited with {code}"),
            None => "was stopped by a signal".to_string(),
        };
        return Err(step_failure(
            step,
            anyhow!("{how}; the workflow stopped here"),
        ));
    }

    let text = String::from_utf8(finished.stdout).map_err(|_| {
        step_failure(
            step,
            anyhow!("its output is binary, which cannot be passed to another step"),
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
    let shape = if matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse) {
        ArgShape::Flag
    } else if arg.is_positional() {
        ArgShape::Positional { repeatable }
    } else {
        ArgShape::Option { repeatable }
    };
    Ok((arg, shape))
}

/// Every `command` step's problems against this binary's command tree —
/// checked at install and again before a run, since the CLI may have been
/// upgraded underneath an installed workflow.
pub fn command_problems(app: &Command, workflow: &Workflow) -> Vec<String> {
    let mut problems = vec![];

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
            "version: 1\nname: t\nsummary: t\ninputs:\n  n: {{ type: number, default: 2 }}\n  flag: {{ type: boolean, default: false }}\nsteps:\n{steps}"
        );
        let files: Files = [(PathBuf::from(DEFINITION_FILE), yaml.into_bytes())].into();
        parse("t", &files).expect("valid")
    }

    #[test]
    fn a_step_is_checked_against_the_real_command_tree() {
        let found = command_problems(
            &app(),
            &workflow(
                "  - id: a\n    command: styles nope\n\
                 \x20 - id: b\n    command: styles\n\
                 \x20 - id: c\n    command: styles get\n    args: { bogus: 1, token: x }\n\
                 \x20 - id: d\n    command: workflow list\n",
            ),
        );
        let joined = found.join("\n");
        assert!(
            joined.contains("`mapbox styles nope` is not a command"),
            "{joined}"
        );
        assert!(joined.contains("is a group"), "{joined}");
        assert!(joined.contains("`bogus` is not an argument"), "{joined}");
        assert!(joined.contains("a secret in a file"), "{joined}");
        assert!(joined.contains("cannot run another workflow"), "{joined}");
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
        let yaml = "version: 1\nname: t\nsummary: t\ninputs:\n  profile: { type: string }\n  dry_run: { type: boolean }\nsteps:\n  - id: a\n    command: styles list\n";
        let files: Files = [(PathBuf::from(DEFINITION_FILE), yaml.as_bytes().to_vec())].into();
        let found = command_problems(&app(), &parse("t", &files).unwrap()).join("\n");
        assert!(found.contains("`--profile`"), "{found}");
        assert!(found.contains("`--dry-run`"), "{found}");
    }
}
