//! `mapbox workflow` — install and run workflows: named, multi-step recipes
//! of `mapbox` commands and scripts. Beta, in development, and not
//! recommended for use: every subcommand says so on stderr.
//!
//! A workflow is a directory holding a `workflow.yaml` and the scripts it
//! runs (see [`definition`] for the schema and the layout rules). None ships
//! inside the binary. `install` copies one in, from a local directory or a
//! GitHub repository's `workflow/<name>/`, and `run` runs only what
//! is installed.
//!
//! What this refuses to be: a scheduler, a retry engine, or a language.
//! Steps run in order and the first failure stops the run. Logic belongs in
//! a script step.

pub mod definition;
pub mod runner;
pub mod store;
pub mod template;

use std::io::IsTerminal;
use std::path::Path;

use anyhow::Result;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{json, Value};

use crate::confirm;
use crate::executor;
use crate::output::{self, field_lines, style, Mode};

pub const COMMAND: &str = "workflow";

/// Printed by every subcommand. Nothing here is a promise yet, and a
/// person who found the command in `--help` should not build on it. One
/// line on purpose: `--help` and the docs carry the rest.
const NOTICE: &str = "`mapbox workflow` is in development and not recommended for use.";

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
             GitHub repository's workflow/<name>/ directory.\n\n\
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
    let terminal = std::io::stderr().is_terminal();
    let color = style::enabled(terminal);
    output::progress(&format!(
        "{} {}",
        style::paint("Beta:", style::BOLD, color),
        style::dim_prose(NOTICE, color)
    ));
    // At a terminal the notice sits right above the result; a blank line
    // keeps it from reading as the result's first line.
    if terminal {
        output::progress("");
    }
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

/// A path as a person reads it: under the home directory as `~/…`. Text
/// output only; JSON keeps the absolute path a program can open.
fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn source_label(meta: &store::Meta) -> String {
    match &meta.git_ref {
        Some(git_ref) => format!("{}@{git_ref}", meta.source),
        None => meta.source.clone(),
    }
}

fn source_text(meta: &store::Meta) -> String {
    match &meta.git_ref {
        Some(_) => source_label(meta),
        None => tilde(Path::new(&meta.source)),
    }
}

/// Rows of cells, each column padded to its widest cell. Padding is
/// measured before painting, so escapes never skew the alignment.
fn columns(rows: &[Vec<(String, &'static str)>], color: bool) -> String {
    let widths: Vec<usize> = (0..rows.iter().map(Vec::len).max().unwrap_or(0))
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|(cell, _)| cell.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            let last = row.len().saturating_sub(1);
            row.iter()
                .enumerate()
                .map(|(column, (cell, paint))| {
                    let pad = if column == last {
                        String::new()
                    } else {
                        " ".repeat(widths[column] - cell.chars().count() + 2)
                    };
                    format!(
                        "{}{pad}",
                        style::paint(cell, paint, color && !paint.is_empty())
                    )
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn heading(text: &str, color: bool) -> String {
    style::paint(text, style::BOLD, color)
}

/// The `run` line a person would type, with every required input spelled
/// out so the next command is one they only have to fill in.
fn run_example(workflow: &definition::Workflow) -> String {
    let mut line = format!("mapbox workflow run {}", workflow.name);
    for (name, input) in &workflow.inputs {
        if input.required {
            line.push_str(&format!(" -i {name}=…"));
        }
    }
    line
}

/// Tips go with the text rendering only, as every other command's do.
fn tips(mode: Mode, tips: &[String]) {
    if !mode.is_json() {
        output::print_tips(tips);
    }
}

fn list(mode: Mode) -> Result<()> {
    let installed = store::list()?;
    let color = output::result_in_color();
    let mut rows = vec![];
    let mut table = vec![vec![
        ("NAME".to_string(), style::BOLD),
        ("SUMMARY".to_string(), style::BOLD),
    ]];
    for (name, loaded) in &installed {
        match loaded {
            Ok(found) => {
                rows.push(json!({
                    "name": name,
                    "summary": found.workflow.summary,
                    "source": source_label(&found.meta),
                    "path": found.root,
                }));
                table.push(vec![
                    (name.clone(), ""),
                    (found.workflow.summary.clone(), ""),
                ]);
            }
            Err(e) => {
                rows.push(json!({ "name": name, "error": format!("{e:#}") }));
                table.push(vec![
                    (name.clone(), ""),
                    (format!("cannot load: {e:#}"), style::DIM),
                ]);
            }
        }
    }

    if installed.is_empty() {
        output::emit(mode, "No workflows installed.", Value::Array(rows))?;
        tips(
            mode,
            &["Install one with `mapbox workflow install <name>` or `mapbox workflow install ./<dir>`."
                .to_string()],
        );
        return Ok(());
    }
    output::emit(mode, &columns(&table, color), Value::Array(rows))?;
    tips(
        mode,
        &["`mapbox workflow show <name>` for a workflow's inputs and steps.".to_string()],
    );
    Ok(())
}

fn show(app: &Command, name: &str, mode: Mode) -> Result<()> {
    let found = store::load(name)?;
    let workflow = &found.workflow;
    let problems = runner::command_problems(app, workflow);
    let color = output::result_in_color();

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

    let mut text = format!(
        "{}\n{}\n\n{}",
        heading(&workflow.name, color),
        workflow.summary,
        field_lines(
            &[
                ("Source", source_text(&found.meta)),
                ("Path", tilde(&found.root)),
            ],
            color,
        )
    );
    if let Some(description) = &workflow.description {
        text.push_str(&format!("\n\n{}", description.trim_end()));
    }

    text.push_str(&format!("\n\n{}", heading("Inputs", color)));
    if workflow.inputs.is_empty() {
        text.push_str("\n  none");
    } else {
        // Required inputs first: they are the ones a run cannot do without.
        let mut ordered: Vec<_> = workflow.inputs.iter().collect();
        ordered.sort_by_key(|(name, input)| (!input.required, name.as_str()));
        let rows: Vec<Vec<(String, &'static str)>> = ordered
            .into_iter()
            .map(|(name, input)| {
                let mut kind = input.kind.as_str().to_string();
                if input.required {
                    kind.push_str(", required");
                }
                if let Some(default) = &input.default {
                    kind.push_str(&format!(", default {default}"));
                }
                vec![
                    (format!("  {name}"), ""),
                    (kind, style::DIM),
                    (input.description.clone().unwrap_or_default(), ""),
                ]
            })
            .collect();
        text.push('\n');
        text.push_str(&columns(&rows, color));
    }

    text.push_str(&format!("\n\n{}\n", heading("Steps", color)));
    let rows: Vec<Vec<(String, &'static str)>> = workflow
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            vec![
                (
                    format!(
                        "  {}. {}",
                        index + 1,
                        step.name.as_deref().unwrap_or(&step.id)
                    ),
                    "",
                ),
                (step.label(), style::DIM),
            ]
        })
        .collect();
    text.push_str(&columns(&rows, color));

    if !problems.is_empty() {
        text.push_str(&format!("\n\n{}", heading("This CLI cannot run it", color)));
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
    )?;
    if problems.is_empty() {
        tips(mode, &[format!("Run it with `{}`.", run_example(workflow))]);
    }
    Ok(())
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

    let color = output::result_in_color();
    let fields = field_lines(
        &[
            ("Source", source_text(&package.meta)),
            ("Path", tilde(&target)),
            ("Files", files.join(", ")),
        ],
        color,
    );

    if dry_run {
        if target.exists() && !force {
            return Err(store::already_installed(&workflow.name, &target));
        }
        let text = format!(
            "Dry run — nothing was written. Would install {}:\n\n{fields}",
            heading(&workflow.name, color)
        );
        return output::emit(mode, &text, summary);
    }

    store::install(&package, force)?;
    let text = format!("Installed {}\n\n{fields}", heading(&workflow.name, color));
    output::emit(mode, &text, summary)?;
    tips(
        mode,
        &[
            format!("Run it with `{}`.", run_example(workflow)),
            format!(
                "`mapbox workflow show {}` describes its inputs.",
                workflow.name
            ),
        ],
    );
    Ok(())
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
            &format!(
                "Dry run — nothing was removed. Would remove {}.",
                tilde(&dir)
            ),
            json!({ "name": name, "path": dir, "dry_run": true }),
        );
    }
    confirm::destructive_local_action(
        &format!("Remove the workflow at {}?", tilde(&dir)),
        assume_yes,
    )?;
    let dir = store::uninstall(name)?;
    output::emit(
        mode,
        &format!("Removed {name} ({}).", tilde(&dir)),
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
        let color = output::result_in_color();
        let input_rows: Vec<Vec<(String, &'static str)>> = inputs
            .iter()
            .map(|(name, value)| {
                let shown = template::as_text(value).unwrap_or_else(|| "(none)".to_string());
                vec![(format!("  {name}"), ""), (shown, "")]
            })
            .collect();
        let step_rows: Vec<Vec<(String, &'static str)>> = workflow
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                vec![
                    (
                        format!(
                            "  {}. {}",
                            index + 1,
                            step.name.as_deref().unwrap_or(&step.id)
                        ),
                        "",
                    ),
                    (step.label(), style::DIM),
                ]
            })
            .collect();
        let mut text = format!(
            "Dry run — nothing was run. Would run {}:",
            heading(&workflow.name, color)
        );
        if !input_rows.is_empty() {
            text.push_str(&format!(
                "\n\n{}\n{}",
                heading("Inputs", color),
                columns(&input_rows, color)
            ));
        }
        text.push_str(&format!(
            "\n\n{}\n{}",
            heading("Steps", color),
            columns(&step_rows, color)
        ));
        return output::emit(mode, &text, plan);
    }

    let inherited = runner::Inherited::from_matches(globals);
    let result = runner::run(app, workflow, &found.root, &inputs, &inherited)?;
    output::emit_value(mode, &result, None, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_line_up_with_color_on_or_off() {
        let rows = vec![
            vec![
                ("NAME".to_string(), style::BOLD),
                ("SUMMARY".to_string(), style::BOLD),
            ],
            vec![
                ("copy-style".to_string(), ""),
                ("Copy a style".to_string(), ""),
            ],
        ];
        let plain = columns(&rows, false);
        assert_eq!(plain, "NAME        SUMMARY\ncopy-style  Copy a style");
        assert_eq!(style::strip(&columns(&rows, true)), plain);
    }

    #[test]
    fn a_path_under_home_is_shown_with_a_tilde() {
        let home = dirs::home_dir().expect("a home directory");
        assert_eq!(
            tilde(&home.join(".mapbox/workflows/x")),
            "~/.mapbox/workflows/x"
        );
        assert_eq!(tilde(Path::new("/opt/x")), "/opt/x");
    }

    #[test]
    fn a_name_and_a_path_never_look_alike() {
        for local in [
            "./copy-style",
            "../x",
            "workflow/copy-style",
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
            .expect("workflow/ exists")
            .flatten()
            .filter(|entry| entry.path().is_dir())
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
        assert!(checked > 0, "no workflows found under workflow/");
    }
}
