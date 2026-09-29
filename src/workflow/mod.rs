//! `mapbox workflow` — install and run workflows: named, multi-step recipes
//! of `mapbox` commands and scripts. Beta, in development, and not
//! recommended for use: every subcommand says so on stderr.
//!
//! A workflow is a directory holding a `workflow.yaml` and the scripts it
//! runs (see [`definition`] for the schema and the layout rules). None ships
//! inside the binary. `install` copies one in, from a local directory or a
//! GitHub repository's `workflow/beta/<name>/`, and `run` runs only what
//! is installed.
//!
//! What this refuses to be: a scheduler, a retry engine, or a language.
//! Steps run in order and the first failure stops the run. Logic belongs in
//! a script step.

pub mod definition;
pub mod runner;
pub mod store;
pub mod template;

use anyhow::Result;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{json, Value};

use crate::confirm;
use crate::executor;
use crate::output::{self, field_lines, Mode};

pub const COMMAND: &str = "workflow";

/// Printed by every subcommand. Nothing here is a promise yet, and a
/// person who found the command in `--help` should not build on it.
const NOTICE: &str = "`mapbox workflow` is beta and in development, and not recommended for use: \
                      its commands, the workflow format and the published workflows may change \
                      or be removed without notice.";

const NAME_ARG: &str = "name";
const SOURCE_ARG: &str = "source";
const REPO_ARG: &str = "repo";
const REF_ARG: &str = "ref";
const FORCE_ARG: &str = "force";
const INPUT_ARG: &str = "input";

pub fn command() -> Command {
    let name = || {
        Arg::new(NAME_ARG)
            .value_name("NAME")
            .required(true)
            .help("Name of an installed workflow")
    };
    Command::new(COMMAND)
        .about(
            "Install and run multi-step workflows of mapbox commands and scripts \
             (beta, in development, not recommended for use)",
        )
        .long_about(
            "Install and run workflows: named, multi-step recipes of mapbox commands and \
             scripts, defined in a workflow.yaml.\n\n\
             A workflow runs only once it is installed, from a local directory or from a \
             GitHub repository's workflow/beta/<name>/ directory.\n\n\
             Beta and in development, and not recommended for use: the commands, the \
             workflow format and the published workflows may change or be removed \
             without notice.",
        )
        .subcommand_required(true)
        .subcommand(Command::new("list").about("List installed workflows"))
        .subcommand(
            Command::new("show")
                .about("Describe an installed workflow: its inputs and steps")
                .arg(name()),
        )
        .subcommand(
            Command::new("install")
                .about("Install a workflow from a local directory or from GitHub")
                .arg(
                    Arg::new(SOURCE_ARG)
                        .value_name("SOURCE")
                        .required(true)
                        .help(
                            "A workflow name, fetched from GitHub, or a path to a local \
                             workflow directory (anything with a `/` or starting with `.`)",
                        ),
                )
                .arg(
                    Arg::new(REPO_ARG)
                        .long(REPO_ARG)
                        .value_name("OWNER/REPO")
                        .help(format!(
                            "GitHub repository to install from [default: {}]. Reads \
                             GH_TOKEN or GITHUB_TOKEN for a private one",
                            store::DEFAULT_REPO
                        )),
                )
                .arg(
                    Arg::new(REF_ARG)
                        .long(REF_ARG)
                        .value_name("REF")
                        .help(format!(
                            "Branch, tag or commit to install from [default: {}]",
                            store::DEFAULT_REF
                        )),
                )
                .arg(
                    Arg::new(FORCE_ARG)
                        .long(FORCE_ARG)
                        .action(ArgAction::SetTrue)
                        .help("Replace a workflow that is already installed"),
                )
                .arg(executor::dry_run_arg(
                    "Check the workflow and list the files it would write, then exit",
                )),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Remove an installed workflow")
                .arg(name())
                .arg(executor::dry_run_arg(
                    "Say what it would remove, then exit without removing it",
                )),
        )
        .subcommand(
            Command::new("run")
                .about("Run an installed workflow")
                .arg(name())
                .arg(
                    Arg::new(INPUT_ARG)
                        .long(INPUT_ARG)
                        .short('i')
                        .value_name("KEY=VALUE")
                        .action(ArgAction::Append)
                        .help("A value for one of the workflow's inputs, repeatable"),
                )
                .arg(executor::dry_run_arg(
                    "Check the workflow and its inputs and print the plan, then exit \
                     without running a step",
                )),
        )
}

/// What the globals say, for the subcommands that need them.
pub struct RunFlags<'a> {
    pub globals: &'a ArgMatches,
    pub debug: bool,
    pub assume_yes: bool,
}

pub fn run(app: &Command, matches: &ArgMatches, flags: RunFlags, mode: Mode) -> Result<()> {
    output::progress(NOTICE);
    match matches.subcommand() {
        Some(("list", _)) => list(mode),
        Some(("show", m)) => show(app, name(m), mode),
        Some(("install", m)) => install(app, m, flags.debug, mode),
        Some(("uninstall", m)) => {
            uninstall(name(m), executor::wants_dry_run(m), flags.assume_yes, mode)
        }
        Some(("run", m)) => run_workflow(app, m, flags.globals, mode),
        _ => unreachable!("`workflow` sets subcommand_required(true)"),
    }
}

fn name(matches: &ArgMatches) -> &str {
    matches.get_one::<String>(NAME_ARG).expect("required")
}

fn source_label(meta: &store::Meta) -> String {
    match &meta.git_ref {
        Some(git_ref) => format!("{}@{git_ref}", meta.source),
        None => meta.source.clone(),
    }
}

fn list(mode: Mode) -> Result<()> {
    let installed = store::list()?;
    let mut rows = vec![];
    let mut text = String::new();
    for (name, loaded) in &installed {
        match loaded {
            Ok(found) => {
                rows.push(json!({
                    "name": name,
                    "summary": found.workflow.summary,
                    "source": source_label(&found.meta),
                    "path": found.root,
                }));
                text.push_str(&format!("{name}\n  {}\n", found.workflow.summary));
            }
            Err(e) => {
                rows.push(json!({ "name": name, "error": format!("{e:#}") }));
                text.push_str(&format!("{name}  (broken)\n  {e:#}\n"));
            }
        }
    }
    if installed.is_empty() {
        text = "No workflows installed. Install one with `mapbox workflow install <name>`.\n"
            .to_string();
    }
    output::emit(mode, text.trim_end(), Value::Array(rows))
}

fn show(app: &Command, name: &str, mode: Mode) -> Result<()> {
    let found = store::load(name)?;
    let workflow = &found.workflow;
    let problems = runner::command_problems(app, workflow);

    let inputs: Vec<Value> = workflow
        .inputs
        .iter()
        .map(|(name, input)| {
            json!({
                "name": name,
                "type": input.kind.as_str(),
                "required": input.required,
                "default": input.default,
                "description": input.description,
            })
        })
        .collect();
    let steps: Vec<Value> = workflow
        .steps
        .iter()
        .map(|step| json!({ "id": step.id, "name": step.name, "run": step.label() }))
        .collect();

    let mut text = field_lines(
        &[
            ("Name", workflow.name.clone()),
            ("Summary", workflow.summary.clone()),
            ("Source", source_label(&found.meta)),
            ("Path", found.root.display().to_string()),
        ],
        output::result_in_color(),
    );
    if let Some(description) = &workflow.description {
        text.push_str(&format!("\n\n{}", description.trim_end()));
    }
    text.push_str("\n\nInputs:");
    if workflow.inputs.is_empty() {
        text.push_str(" none");
    }
    for (name, input) in &workflow.inputs {
        let mut line = format!("\n  {name} ({}", input.kind.as_str());
        if input.required {
            line.push_str(", required");
        }
        if let Some(default) = &input.default {
            line.push_str(&format!(", default {default}"));
        }
        line.push(')');
        if let Some(description) = &input.description {
            line.push_str(&format!("  {description}"));
        }
        text.push_str(&line);
    }
    text.push_str("\n\nSteps:");
    for (index, step) in workflow.steps.iter().enumerate() {
        let title = step.name.as_deref().unwrap_or(&step.id);
        text.push_str(&format!("\n  {}. {title}  {}", index + 1, step.label()));
    }
    if !problems.is_empty() {
        text.push_str("\n\nThis CLI cannot run it:");
        for problem in &problems {
            text.push_str(&format!("\n  - {problem}"));
        }
    }

    output::emit(
        mode,
        &text,
        json!({
            "name": workflow.name,
            "summary": workflow.summary,
            "description": workflow.description,
            "source": source_label(&found.meta),
            "path": found.root,
            "inputs": inputs,
            "steps": steps,
            "problems": problems,
        }),
    )
}

/// A source with a path separator or a leading `.` is a directory; anything
/// else is a name to look up on GitHub. A workflow name can hold neither,
/// so the two never overlap.
fn is_local_source(source: &str) -> bool {
    source.starts_with('.') || source.contains('/') || source.contains('\\')
}

fn install(app: &Command, matches: &ArgMatches, debug: bool, mode: Mode) -> Result<()> {
    let source = matches.get_one::<String>(SOURCE_ARG).expect("required");
    let repo = matches.get_one::<String>(REPO_ARG);
    let git_ref = matches.get_one::<String>(REF_ARG);
    let force = matches.get_flag(FORCE_ARG);
    let dry_run = executor::wants_dry_run(matches);

    let package = if is_local_source(source) {
        if repo.is_some() || git_ref.is_some() {
            return Err(output::CliError::new(
                "invalid_arguments",
                "`--repo` and `--ref` name a GitHub source; this one is a local directory.",
            )
            .into());
        }
        store::read_local(std::path::Path::new(source))?
    } else {
        store::fetch_github(
            source,
            repo.map_or(store::DEFAULT_REPO, String::as_str),
            git_ref.map_or(store::DEFAULT_REF, String::as_str),
            debug,
        )?
    };
    let workflow = &package.workflow;
    let problems = runner::command_problems(app, workflow);
    if !problems.is_empty() {
        return Err(store::invalid_workflow(&workflow.name, &problems));
    }

    let files: Vec<String> = package
        .files
        .keys()
        .map(|path| definition::slash_path(path))
        .collect();
    let target = store::root_path()?.join(&workflow.name);
    let summary = json!({
        "name": workflow.name,
        "source": source_label(&package.meta),
        "path": target,
        "files": files,
        "dry_run": dry_run,
    });

    if dry_run {
        if target.exists() && !force {
            return Err(store::already_installed(&workflow.name, &target));
        }
        let text = format!(
            "Would install `{}` from {} into {}:\n{}",
            workflow.name,
            source_label(&package.meta),
            target.display(),
            files
                .iter()
                .map(|file| format!("  {file}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        return output::emit(mode, &text, summary);
    }

    let target = store::install(&package, force)?;
    let text = format!(
        "Installed `{}` from {} into {}.\nRun it with `mapbox workflow run {}`.",
        workflow.name,
        source_label(&package.meta),
        target.display(),
        workflow.name
    );
    output::emit(mode, &text, summary)
}

fn uninstall(name: &str, dry_run: bool, assume_yes: bool, mode: Mode) -> Result<()> {
    let found = store::installed_dir(name)?;
    let Some(dir) = found else {
        // `load` words the error, with the commands to try instead.
        return store::load(name).map(|_| ());
    };
    if dry_run {
        return output::emit(
            mode,
            &format!("Would remove {}.", dir.display()),
            json!({ "name": name, "path": dir, "dry_run": true }),
        );
    }
    confirm::destructive_local_action(
        &format!("Remove the workflow at {}?", dir.display()),
        assume_yes,
    )?;
    let dir = store::uninstall(name)?;
    output::emit(
        mode,
        &format!("Removed `{name}` from {}.", dir.display()),
        json!({ "name": name, "path": dir, "dry_run": false }),
    )
}

fn run_workflow(
    app: &Command,
    matches: &ArgMatches,
    globals: &ArgMatches,
    mode: Mode,
) -> Result<()> {
    let found = store::load(name(matches))?;
    let workflow = &found.workflow;
    let problems = runner::command_problems(app, workflow);
    if !problems.is_empty() {
        return Err(store::invalid_workflow(&workflow.name, &problems));
    }

    let pairs: Vec<String> = matches
        .get_many::<String>(INPUT_ARG)
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let inputs = runner::read_inputs(workflow, &pairs)?;

    if executor::wants_dry_run(matches) {
        let plan = runner::plan(workflow, &inputs);
        let text =
            std::iter::once(format!("Would run `{}`:", workflow.name))
                .chain(
                    workflow.steps.iter().enumerate().map(|(index, step)| {
                        format!("  {}. {}  {}", index + 1, step.id, step.label())
                    }),
                )
                .collect::<Vec<_>>()
                .join("\n");
        return output::emit(mode, &text, plan);
    }

    let inherited = runner::Inherited::from_matches(globals);
    let result = runner::run(app, workflow, &found.root, &inputs, &inherited)?;
    output::emit_value(mode, &result, None, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_name_and_a_path_never_look_alike() {
        for local in [
            "./copy-style",
            "../x",
            "workflow/beta/copy-style",
            "/abs",
            ".",
            "a\\b",
        ] {
            assert!(is_local_source(local), "{local}");
        }
        assert!(!is_local_source("copy-style"));
    }

    /// Every workflow this repository publishes is one `install` accepts.
    /// The rules are the same as for anybody's, so this is also what
    /// catches a stray file or a step naming a command that was renamed.
    #[test]
    fn every_published_workflow_is_valid() {
        let app = crate::build_app(&crate::spec::effective_services().expect("bundled specs"));
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(store::REPO_PREFIX);
        let mut checked = 0;
        for dir in std::fs::read_dir(&root)
            .expect("workflow/beta/ exists")
            .flatten()
        {
            let package = store::read_local(&dir.path())
                .unwrap_or_else(|e| panic!("{}: {e:#}", dir.path().display()));
            let problems = runner::command_problems(&app, &package.workflow);
            assert!(
                problems.is_empty(),
                "{}: {problems:#?}",
                dir.path().display()
            );
            checked += 1;
        }
        assert!(checked > 0, "no workflows found under workflow/beta/");
    }
}
