//! The `workflow.yaml` schema, and the layout rules a workflow directory is
//! held to.
//!
//! A workflow reaches this module as files in memory — read from a local
//! directory or out of a GitHub tarball — and is checked here before any of
//! it is written or run. The rules are the same for the workflows in this
//! repository's `workflow/` and for anybody else's, so `install` is where a
//! broken one is refused rather than `run`, halfway through.
//!
//! What this cannot check is whether a `command` step names a real command
//! with real arguments: that needs the command tree, and lives in
//! [`super::runner::command_problems`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

use super::template::{self, Reference};

pub const SCHEMA_VERSION: u64 = 1;
pub const DEFINITION_FILE: &str = "workflow.yaml";
pub const SCRIPTS_DIR: &str = "scripts";
pub const README_FILE: &str = "README.md";

/// A workflow's files, by path relative to its own directory.
pub type Files = BTreeMap<PathBuf, Vec<u8>>;

/// A checked workflow. Nothing constructs one except [`parse`].
#[derive(Debug, Clone)]
pub struct Workflow {
    pub name: String,
    /// The workflow's own version, independent of the CLI's: workflows are
    /// released on their own, and may move to a repository of their own.
    /// Optional, so a workflow that has never been released still installs.
    pub version: Option<String>,
    /// The oldest CLI that can run it, from `requires: ">=X.Y.Z"`. Checked by
    /// [`super::runner::command_problems`], since it is the only check that
    /// covers what a script step asks of `$MAPBOX_CLI`.
    pub requires: Option<String>,
    pub summary: String,
    pub description: Option<String>,
    pub inputs: BTreeMap<String, Input>,
    pub steps: Vec<Step>,
    pub outputs: Option<Value>,
    /// What `run` prints in text mode instead of the outputs as fields: a
    /// template over the same inputs and steps. `-o json` still prints the
    /// outputs.
    pub result: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    #[serde(rename = "type")]
    pub kind: InputType,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputType {
    String,
    Number,
    Boolean,
}

impl InputType {
    pub fn as_str(self) -> &'static str {
        match self {
            InputType::String => "string",
            InputType::Number => "number",
            InputType::Boolean => "boolean",
        }
    }

    pub fn accepts(self, value: &Value) -> bool {
        matches!(
            (self, value),
            (InputType::String, Value::String(_))
                | (InputType::Number, Value::Number(_))
                | (InputType::Boolean, Value::Bool(_))
        )
    }
}

#[derive(Debug, Clone)]
pub struct Step {
    pub id: String,
    pub name: Option<String>,
    pub action: Action,
    pub stdin: Option<Value>,
    /// Runs under `--dry-run` too. A script is told so through
    /// [`DRY_RUN_ENV`] and promises to write nothing; a command step may be
    /// marked only if its command is on the read-only list that
    /// [`super::runner::command_problems`] checks.
    pub dry_run: bool,
    /// A command step's stdout, written as this file in the run's
    /// [`super::workdir`] rather than read as its output. How a binary
    /// response, such as a style's ZIP, reaches a later step.
    pub save: Option<String>,
}

/// Set to `1` for a `dry_run` step while `--dry-run` runs it.
pub const DRY_RUN_ENV: &str = "MAPBOX_WORKFLOW_DRY_RUN";

#[derive(Debug, Clone)]
pub enum Action {
    Command {
        /// `styles get` as `["styles", "get"]`.
        path: Vec<String>,
        args: Map<String, Value>,
    },
    Script {
        /// Relative to `scripts/`.
        script: PathBuf,
        interpreter: String,
        args: Vec<Value>,
    },
}

impl Step {
    /// How progress and plans name the step.
    pub fn label(&self) -> String {
        match &self.action {
            Action::Command { path, .. } => format!("mapbox {}", path.join(" ")),
            Action::Script {
                script,
                interpreter,
                ..
            } => format!("{interpreter} {SCRIPTS_DIR}/{}", slash_path(script)),
        }
    }
}

/// The file as written, before anything is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    schema: Option<u64>,
    /// A string `0.1.0` alongside `schema`; on its own, the integer that
    /// named the schema before `schema` existed. See [`schema_and_version`].
    #[serde(default)]
    version: Option<Value>,
    #[serde(default)]
    requires: Option<String>,
    name: String,
    summary: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    inputs: BTreeMap<String, Input>,
    steps: Vec<RawStep>,
    #[serde(default)]
    outputs: Option<Value>,
    #[serde(default)]
    result: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    interpreter: Option<String>,
    #[serde(default)]
    args: Option<Value>,
    #[serde(default)]
    stdin: Option<Value>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    save: Option<String>,
}

/// A workflow name is one directory name, lower-case and dash-separated —
/// `uninstall` turns it into a path it deletes, so nothing else may pass.
pub fn is_workflow_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Input names and step ids, which expressions refer to.
fn is_identifier(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `major.minor.patch`, with an optional pre-release. A release is meant to
/// be tagged `workflow-<name>-<version>`, so only what a ref may hold, and no
/// leading `v` or `+build`, which would give one version two spellings.
fn is_workflow_version(version: &str) -> bool {
    version.starts_with(|c: char| c.is_ascii_digit())
        && !version.ends_with('-')
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && crate::update_check::parse_version(version).is_some()
}

/// The version in `requires: ">=X.Y.Z"`. A lower bound only: which future
/// CLI breaks a workflow is not knowable when it is written.
fn minimum_cli(requires: &str) -> Option<&str> {
    let version = requires.trim().strip_prefix(">=")?.trim();
    is_workflow_version(version).then_some(version)
}

/// The workflow's own version, once `schema` is checked. Before `schema`
/// existed, `version: 1` named the schema; that spelling still reads, as an
/// unversioned workflow, so a commit from then still installs.
fn schema_and_version(
    schema: Option<u64>,
    version: Option<&Value>,
    problems: &mut Vec<String>,
) -> Option<String> {
    match (schema, version) {
        (Some(schema), version) => {
            if schema != SCHEMA_VERSION {
                problems.push(format!(
                    "`schema: {schema}` is not one this CLI reads; it reads \
                     `schema: {SCHEMA_VERSION}`"
                ));
            }
            match version {
                None => None,
                Some(Value::String(version)) if is_workflow_version(version) => {
                    Some(version.clone())
                }
                Some(other) => {
                    let shown = other
                        .as_str()
                        .map_or_else(|| other.to_string(), String::from);
                    problems.push(format!(
                        "`version: {shown}` must be the workflow's own version, like `0.1.0`"
                    ));
                    None
                }
            }
        }
        (None, Some(Value::Number(legacy))) if legacy.as_u64() == Some(SCHEMA_VERSION) => None,
        (None, _) => {
            problems.push(format!("`schema: {SCHEMA_VERSION}` is missing"));
            None
        }
    }
}

/// A path that stays inside the directory it is relative to.
pub fn is_contained(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub fn slash_path(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The interpreter a script runs under when the step names none. A short,
/// fixed list: guessing further is how a script ends up run by the wrong
/// program.
fn default_interpreter(script: &Path) -> Option<&'static str> {
    match script.extension()?.to_str()? {
        "sh" => Some("sh"),
        "py" => Some("python3"),
        "js" | "mjs" => Some("node"),
        _ => None,
    }
}

/// Checks `files` as the workflow called `name`, and returns it or every
/// problem found — all of them at once, so fixing a workflow is not one
/// install attempt per mistake.
pub fn parse(name: &str, files: &Files) -> Result<Workflow, Vec<String>> {
    let mut problems = vec![];

    for path in files.keys() {
        let allowed = path == Path::new(DEFINITION_FILE)
            || path == Path::new(README_FILE)
            || path.starts_with(SCRIPTS_DIR);
        if !allowed {
            problems.push(format!(
                "`{}` does not belong in a workflow: only {DEFINITION_FILE}, {README_FILE} \
                 and {SCRIPTS_DIR}/ do",
                slash_path(path)
            ));
        }
    }

    let Some(bytes) = files.get(Path::new(DEFINITION_FILE)) else {
        problems.push(format!("there is no {DEFINITION_FILE}"));
        return Err(problems);
    };
    let raw: Raw = match serde_yaml::from_slice(bytes) {
        Ok(raw) => raw,
        Err(e) => {
            problems.push(format!("{DEFINITION_FILE} is not valid: {e}"));
            return Err(problems);
        }
    };

    let version = schema_and_version(raw.schema, raw.version.as_ref(), &mut problems);
    let requires = raw.requires.as_deref().and_then(|requires| {
        let minimum = minimum_cli(requires);
        if minimum.is_none() {
            problems.push(format!(
                "`requires: {requires}` must be `\">=X.Y.Z\"`, quoted, naming the oldest \
                 mapbox that can run it"
            ));
        }
        minimum.map(String::from)
    });
    if raw.name != name {
        problems.push(format!(
            "`name: {}` must match the directory it is in, `{name}`",
            raw.name
        ));
    }
    if !is_workflow_name(&raw.name) {
        problems.push(format!(
            "`name: {}` must be lower-case letters, digits and dashes",
            raw.name
        ));
    }
    if raw.summary.trim().is_empty() || raw.summary.contains('\n') {
        problems.push("`summary` must be one non-empty line".to_string());
    }

    for (input, spec) in &raw.inputs {
        if !is_identifier(input) {
            problems.push(format!(
                "input `{input}` must be lower-case letters, digits and underscores"
            ));
        }
        if let Some(default) = &spec.default {
            if spec.required {
                problems.push(format!(
                    "input `{input}` is required and has a default; it can only be one"
                ));
            }
            if !spec.kind.accepts(default) {
                problems.push(format!(
                    "input `{input}`'s default is not a {}",
                    spec.kind.as_str()
                ));
            }
        }
    }

    if raw.steps.is_empty() {
        problems.push("`steps` is empty".to_string());
    }

    let mut steps = vec![];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut runs_in_dry_run: BTreeSet<String> = BTreeSet::new();
    let mut referenced_scripts: BTreeSet<PathBuf> = BTreeSet::new();
    for raw_step in raw.steps {
        let id = raw_step.id.clone();
        if !is_identifier(&id) {
            problems.push(format!(
                "step `{id}`: the id must be lower-case letters, digits and underscores"
            ));
        }
        if seen.contains(&id) {
            problems.push(format!("step `{id}` appears twice"));
        }

        for value in raw_step.args.iter().chain(raw_step.stdin.iter()) {
            check_references(
                value,
                &format!("step `{id}`"),
                &raw.inputs,
                &seen,
                &mut problems,
            );
        }

        if raw_step.dry_run {
            // A dry run skips every other step, so there is no output of
            // theirs for this one to read.
            for value in raw_step.args.iter().chain(raw_step.stdin.iter()) {
                for reference in template::references(value).unwrap_or_default() {
                    if let Reference::Step { id: read, .. } = &reference {
                        if seen.contains(read) && !runs_in_dry_run.contains(read) {
                            problems.push(format!(
                                "step `{id}` runs under --dry-run but reads `{}`, from a \
                                 step that does not",
                                template::display(&reference)
                            ));
                        }
                    }
                }
            }
        }

        let action = match (raw_step.command, raw_step.script) {
            (Some(command), None) => {
                if let Some(save) = &raw_step.save {
                    let path = Path::new(save);
                    if !is_contained(path) || path.components().count() != 1 {
                        problems.push(format!(
                            "step `{id}`: `save: {save}` must be a plain file name"
                        ));
                    }
                }
                if raw_step.interpreter.is_some() {
                    problems.push(format!(
                        "step `{id}`: `interpreter` only applies to a `script` step"
                    ));
                }
                let path: Vec<String> = command.split_whitespace().map(String::from).collect();
                if path.is_empty() {
                    problems.push(format!("step `{id}`: `command` is empty"));
                }
                let args = match raw_step.args {
                    None => Map::new(),
                    Some(Value::Object(args)) => args,
                    Some(_) => {
                        problems.push(format!(
                            "step `{id}`: a command's `args` is a mapping of argument name to value"
                        ));
                        Map::new()
                    }
                };
                Some(Action::Command { path, args })
            }
            (None, Some(script)) => {
                if raw_step.save.is_some() {
                    problems.push(format!(
                        "step `{id}`: `save` only applies to a `command` step; a script \
                         writes its own files"
                    ));
                }
                let script = PathBuf::from(script);
                let under_scripts = Path::new(SCRIPTS_DIR).join(&script);
                if !is_contained(&script) {
                    problems.push(format!(
                        "step `{id}`: `script: {}` must be a path inside {SCRIPTS_DIR}/",
                        script.display()
                    ));
                } else if !files.contains_key(&under_scripts) {
                    problems.push(format!(
                        "step `{id}`: there is no {SCRIPTS_DIR}/{}",
                        slash_path(&script)
                    ));
                }
                referenced_scripts.insert(under_scripts);

                let interpreter = match raw_step
                    .interpreter
                    .or_else(|| default_interpreter(&script).map(String::from))
                {
                    Some(interpreter)
                        if !interpreter.is_empty()
                            && !interpreter.contains(char::is_whitespace) =>
                    {
                        interpreter
                    }
                    Some(interpreter) => {
                        problems.push(format!(
                            "step `{id}`: `interpreter: {interpreter}` must be one program name"
                        ));
                        String::new()
                    }
                    None => {
                        problems.push(format!(
                            "step `{id}`: name an `interpreter` for {}; only .sh, .py and .js \
                             have a default",
                            slash_path(&script)
                        ));
                        String::new()
                    }
                };
                let args = match raw_step.args {
                    None => vec![],
                    Some(Value::Array(args)) => args,
                    Some(_) => {
                        problems.push(format!(
                            "step `{id}`: a script's `args` is a list of values"
                        ));
                        vec![]
                    }
                };
                Some(Action::Script {
                    script,
                    interpreter,
                    args,
                })
            }
            (Some(_), Some(_)) => {
                problems.push(format!(
                    "step `{id}` has both `command` and `script`; a step is one or the other"
                ));
                None
            }
            (None, None) => {
                problems.push(format!("step `{id}` needs a `command` or a `script`"));
                None
            }
        };

        seen.insert(id.clone());
        if raw_step.dry_run {
            runs_in_dry_run.insert(id.clone());
        }
        if let Some(action) = action {
            steps.push(Step {
                id,
                name: raw_step.name,
                action,
                stdin: raw_step.stdin,
                dry_run: raw_step.dry_run,
                save: raw_step.save,
            });
        }
    }

    if let Some(outputs) = &raw.outputs {
        check_references(outputs, "`outputs`", &raw.inputs, &seen, &mut problems);
    }
    if let Some(result) = &raw.result {
        let result = Value::String(result.clone());
        check_references(&result, "`result`", &raw.inputs, &seen, &mut problems);
    }

    for path in files.keys().filter(|path| path.starts_with(SCRIPTS_DIR)) {
        if !referenced_scripts.contains(path) {
            problems.push(format!(
                "`{}` is not used by any step; a workflow ships only the scripts it runs",
                slash_path(path)
            ));
        }
    }

    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Workflow {
        name: raw.name,
        version,
        requires,
        summary: raw.summary,
        description: raw.description,
        inputs: raw.inputs,
        steps,
        outputs: raw.outputs,
        result: raw.result,
    })
}

/// Every expression in `value` must name a declared input or a step that
/// has already run by the time `value` is read.
fn check_references(
    value: &Value,
    owner: &str,
    inputs: &BTreeMap<String, Input>,
    earlier_steps: &BTreeSet<String>,
    problems: &mut Vec<String>,
) {
    let references = match template::references(value) {
        Ok(references) => references,
        Err(e) => {
            problems.push(format!("{owner}: {e}"));
            return;
        }
    };
    for reference in references {
        match &reference {
            Reference::Input(name) if !inputs.contains_key(name) => problems.push(format!(
                "{owner}: `{}` names no declared input",
                template::display(&reference)
            )),
            Reference::Step { id, .. } if !earlier_steps.contains(id) => problems.push(format!(
                "{owner}: `{}` names no earlier step",
                template::display(&reference)
            )),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "\
schema: 1
name: demo
summary: A demo
inputs:
  style_id: { type: string, required: true }
steps:
  - id: fetch
    command: styles get
    args: { style-id: '${{ inputs.style_id }}' }
  - id: shape
    script: shape.py
    stdin: '${{ steps.fetch.output }}'
outputs:
  id: '${{ steps.shape.output.id }}'
";

    fn files(entries: &[(&str, &str)]) -> Files {
        entries
            .iter()
            .map(|(path, body)| (PathBuf::from(path), body.as_bytes().to_vec()))
            .collect()
    }

    fn problems(entries: &[(&str, &str)]) -> Vec<String> {
        parse("demo", &files(entries)).expect_err("should be refused")
    }

    #[test]
    fn a_minimal_workflow_parses() {
        let workflow = parse(
            "demo",
            &files(&[
                (DEFINITION_FILE, MINIMAL),
                ("scripts/shape.py", ""),
                (README_FILE, ""),
            ]),
        )
        .unwrap();
        assert_eq!(workflow.steps.len(), 2);
        assert_eq!(workflow.steps[0].label(), "mapbox styles get");
        assert_eq!(workflow.steps[1].label(), "python3 scripts/shape.py");
    }

    #[test]
    fn the_name_must_match_the_directory() {
        let found = parse(
            "other",
            &files(&[(DEFINITION_FILE, MINIMAL), ("scripts/shape.py", "")]),
        )
        .unwrap_err();
        assert!(found.iter().any(|p| p.contains("must match")), "{found:?}");
    }

    #[test]
    fn stray_and_unused_files_are_refused() {
        let found = problems(&[
            (DEFINITION_FILE, MINIMAL),
            ("scripts/shape.py", ""),
            ("scripts/old.py", ""),
            ("notes.txt", ""),
        ]);
        assert!(
            found.iter().any(|p| p.contains("scripts/old.py")),
            "{found:?}"
        );
        assert!(found.iter().any(|p| p.contains("notes.txt")), "{found:?}");
    }

    #[test]
    fn a_missing_script_is_refused() {
        let found = problems(&[(DEFINITION_FILE, MINIMAL)]);
        assert!(
            found.iter().any(|p| p.contains("no scripts/shape.py")),
            "{found:?}"
        );
    }

    #[test]
    fn a_script_path_cannot_leave_scripts() {
        let yaml = MINIMAL.replace("script: shape.py", "script: ../workflow.yaml");
        let found = problems(&[(DEFINITION_FILE, &yaml), ("scripts/shape.py", "")]);
        assert!(
            found.iter().any(|p| p.contains("inside scripts/")),
            "{found:?}"
        );
    }

    #[test]
    fn a_reference_must_point_backwards() {
        let yaml = MINIMAL.replace(
            "args: { style-id: '${{ inputs.style_id }}' }",
            "args: { style-id: '${{ steps.shape.output.id }}' }",
        );
        let found = problems(&[(DEFINITION_FILE, &yaml), ("scripts/shape.py", "")]);
        assert!(
            found.iter().any(|p| p.contains("no earlier step")),
            "{found:?}"
        );
    }

    #[test]
    fn save_is_a_plain_file_name_on_a_command_step() {
        for bad in ["../x.zip", "dir/x.zip", "/tmp/x.zip"] {
            let yaml = MINIMAL.replace(
                "    command: styles get\n",
                &format!("    command: styles get\n    save: '{bad}'\n"),
            );
            let found = problems(&[(DEFINITION_FILE, &yaml), ("scripts/shape.py", "")]);
            assert!(
                found
                    .iter()
                    .any(|p| p.contains("must be a plain file name")),
                "{bad}: {found:?}"
            );
        }

        let yaml = MINIMAL.replace(
            "    script: shape.py\n",
            "    script: shape.py\n    save: out.json\n",
        );
        let found = problems(&[(DEFINITION_FILE, &yaml), ("scripts/shape.py", "")]);
        assert!(
            found
                .iter()
                .any(|p| p.contains("only applies to a `command` step")),
            "{found:?}"
        );
    }

    /// A dry run skips the steps not marked `dry_run`, so a step that is
    /// marked cannot read their output.
    #[test]
    fn a_dry_run_step_reads_only_dry_run_steps() {
        let yaml = MINIMAL.replace(
            "    script: shape.py\n",
            "    script: shape.py\n    dry_run: true\n",
        );
        let found = problems(&[(DEFINITION_FILE, &yaml), ("scripts/shape.py", "")]);
        assert!(
            found
                .iter()
                .any(|p| p.contains("runs under --dry-run but reads `steps.fetch")),
            "{found:?}"
        );
    }

    #[test]
    fn result_may_name_only_earlier_steps_and_declared_inputs() {
        let yaml = format!("{MINIMAL}result: 'Made ${{{{ steps.nope.output.id }}}}'\n");
        let found = problems(&[(DEFINITION_FILE, &yaml), ("scripts/shape.py", "")]);
        assert!(
            found
                .iter()
                .any(|p| p.starts_with("`result`") && p.contains("no earlier step")),
            "{found:?}"
        );
    }

    #[test]
    fn an_undeclared_input_is_refused() {
        let yaml = MINIMAL.replace("inputs.style_id }}'", "inputs.nope }}'");
        let found = problems(&[(DEFINITION_FILE, &yaml), ("scripts/shape.py", "")]);
        assert!(
            found.iter().any(|p| p.contains("no declared input")),
            "{found:?}"
        );
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let yaml = MINIMAL.replace("summary: A demo", "summary: A demo\nretries: 3");
        let found = problems(&[(DEFINITION_FILE, &yaml), ("scripts/shape.py", "")]);
        assert!(found.iter().any(|p| p.contains("retries")), "{found:?}");
    }

    #[test]
    fn an_unknown_extension_needs_an_interpreter() {
        let yaml = MINIMAL.replace("shape.py", "shape.rb");
        let found = problems(&[(DEFINITION_FILE, &yaml), ("scripts/shape.rb", "")]);
        assert!(found.iter().any(|p| p.contains("interpreter")), "{found:?}");
    }

    #[test]
    fn workflow_names_are_one_plain_directory_name() {
        for good in ["copy-style", "a", "v2-sync"] {
            assert!(is_workflow_name(good), "{good}");
        }
        for bad in ["", "..", "a/b", "/tmp", "Copy", "-x", "a_b", "a.b"] {
            assert!(!is_workflow_name(bad), "{bad}");
        }
    }

    fn parse_yaml(yaml: &str) -> Result<Workflow, Vec<String>> {
        parse(
            "demo",
            &files(&[(DEFINITION_FILE, yaml), ("scripts/shape.py", "")]),
        )
    }

    #[test]
    fn version_and_requires_are_optional_and_read_when_given() {
        let bare = parse_yaml(MINIMAL).unwrap();
        assert_eq!((bare.version, bare.requires), (None, None));

        let yaml = MINIMAL.replace(
            "name: demo\n",
            "name: demo\nversion: 0.2.0-beta.1\nrequires: \">= 0.4.0\"\n",
        );
        let given = parse_yaml(&yaml).unwrap();
        assert_eq!(given.version.as_deref(), Some("0.2.0-beta.1"));
        assert_eq!(given.requires.as_deref(), Some("0.4.0"));
    }

    /// Before `schema`, the schema was `version: 1`. A commit from then
    /// still installs, with no version of its own.
    #[test]
    fn the_old_schema_spelling_still_reads() {
        let yaml = MINIMAL.replace("schema: 1\n", "version: 1\n");
        assert_eq!(parse_yaml(&yaml).unwrap().version, None);
    }

    #[test]
    fn a_schema_is_required_and_must_be_known() {
        for (yaml, expected) in [
            (MINIMAL.replace("schema: 1\n", ""), "`schema: 1` is missing"),
            (
                MINIMAL.replace("schema: 1\n", "version: 0.1.0\n"),
                "`schema: 1` is missing",
            ),
            (MINIMAL.replace("schema: 1\n", "schema: 2\n"), "`schema: 2`"),
        ] {
            let found = parse_yaml(&yaml).unwrap_err();
            assert!(found.iter().any(|p| p.contains(expected)), "{found:?}");
        }
    }

    #[test]
    fn a_version_is_a_plain_semver() {
        for bad in [
            "1",
            "1.0",
            "v0.1.0",
            "0.1.0+build",
            "0.1",
            "0.1.0-",
            "0.1.0/x",
        ] {
            let yaml = MINIMAL.replace("name: demo\n", &format!("name: demo\nversion: '{bad}'\n"));
            let found = parse_yaml(&yaml).unwrap_err();
            assert!(
                found
                    .iter()
                    .any(|p| p.contains("the workflow's own version")),
                "{bad}: {found:?}"
            );
        }
        // Unquoted, YAML reads `1` as a number, which is the old schema
        // spelling's shape; beside `schema` it is refused all the same.
        let yaml = MINIMAL.replace("name: demo\n", "name: demo\nversion: 1\n");
        assert!(parse_yaml(&yaml).is_err());
    }

    #[test]
    fn requires_is_a_quoted_lower_bound() {
        for bad in ["0.4.0", "<=0.4.0", ">=0.4", ">=v0.4.0"] {
            let yaml = MINIMAL.replace("name: demo\n", &format!("name: demo\nrequires: '{bad}'\n"));
            let found = parse_yaml(&yaml).unwrap_err();
            assert!(
                found.iter().any(|p| p.contains("`requires:")),
                "{bad}: {found:?}"
            );
        }
    }
}
