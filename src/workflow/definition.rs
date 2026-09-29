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
    pub summary: String,
    pub description: Option<String>,
    pub inputs: BTreeMap<String, Input>,
    pub steps: Vec<Step>,
    pub outputs: Option<Value>,
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
}

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
    version: u64,
    name: String,
    summary: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    inputs: BTreeMap<String, Input>,
    steps: Vec<RawStep>,
    #[serde(default)]
    outputs: Option<Value>,
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

    if raw.version != SCHEMA_VERSION {
        problems.push(format!(
            "`version: {}` is not one this CLI reads; it reads `version: {SCHEMA_VERSION}`",
            raw.version
        ));
    }
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

        let action = match (raw_step.command, raw_step.script) {
            (Some(command), None) => {
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
        if let Some(action) = action {
            steps.push(Step {
                id,
                name: raw_step.name,
                action,
                stdin: raw_step.stdin,
            });
        }
    }

    if let Some(outputs) = &raw.outputs {
        check_references(outputs, "`outputs`", &raw.inputs, &seen, &mut problems);
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
        summary: raw.summary,
        description: raw.description,
        inputs: raw.inputs,
        steps,
        outputs: raw.outputs,
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
version: 1
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
}
