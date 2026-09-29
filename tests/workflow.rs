//! End-to-end tests for `mapbox workflow`.
//!
//! The unit tests in `src/workflow/` cover the schema, the expressions and
//! how a step's arguments become a command line. What they cannot show is
//! what a real run does: that a command step is a real child `mapbox` whose
//! JSON reaches the next step, that a script's stdin and stdout carry values
//! between steps, that only the workflow's result lands on stdout, and what
//! `install` and `uninstall` leave on disk.
//!
//! Nothing here reaches the network. The command steps run `config list` and
//! `auth whoami`, which make no request, and every install is from a local
//! directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

/// A token-shaped fake for account `example-user`.
const TOKEN: &str = "pk.eyJ1IjoiZXhhbXBsZS11c2VyIiwiYSI6IngifQ.SIGNATURE";

fn scratch(name: &str) -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("workflow-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create the scratch home");
    home
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env("MAPBOX_NO_UPDATE_CHECK", "1")
        .env("MAPBOX_QUIET", "1")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("MAPBOX_CONFIG_DIR", home.join(".mapbox"))
        .args(args)
        .output()
        .expect("run mapbox")
}

fn stdout_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON document ({e}):\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// Writes a workflow at `<home>/src/<name>/` and returns its path.
fn write_workflow(home: &Path, name: &str, yaml: &str, scripts: &[(&str, &str)]) -> PathBuf {
    let dir = home.join("src").join(name);
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("workflow.yaml"), yaml).unwrap();
    for (file, body) in scripts {
        std::fs::write(dir.join("scripts").join(file), body).unwrap();
    }
    dir
}

const DEMO: &str = r#"version: 1
name: demo
summary: Pass values from a command to a script and back
inputs:
  greeting: { type: string, default: hello }
steps:
  - id: settings
    command: config list
  - id: whoami
    command: auth whoami
  - id: shout
    script: shout.sh
    args: ["${{ inputs.greeting }}"]
    stdin: ${{ steps.settings.output[0] }}
outputs:
  account: ${{ steps.whoami.output.account }}
  first_key: ${{ steps.settings.output[0].key }}
  shouted: ${{ steps.shout.output.shouted }}
  seen: ${{ steps.shout.output.seen }}
"#;

/// Echoes its argument upper-cased beside what it read on stdin, and says
/// something on stderr, which must not reach the result.
const SHOUT: &str = r#"read -r line
echo "shouting" >&2
printf '{"shouted":"%s","seen":%s}\n' "$(printf %s "$1" | tr a-z A-Z)" "$line"
"#;

fn install_demo(home: &Path) {
    let dir = write_workflow(home, "demo", DEMO, &[("shout.sh", SHOUT)]);
    let out = run(
        home,
        &["workflow", "install", dir.to_str().unwrap(), "-o", "json"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_run_carries_values_between_steps_and_prints_only_the_result() {
    let home = scratch("run");
    install_demo(&home);

    let out = run(
        &home,
        &[
            "--token",
            TOKEN,
            "workflow",
            "run",
            "demo",
            "--greeting",
            "hi",
            "-o",
            "json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let first_key = json!("update-check");
    assert_eq!(
        stdout_json(&out),
        json!({
            "account": "example-user",
            "first_key": first_key,
            "shouted": "HI",
            "seen": { "key": first_key, "value": true },
        })
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[1/3] settings (mapbox config list)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("[3/3] shout (sh scripts/shout.sh)"),
        "{stderr}"
    );
    assert!(stderr.contains("shouting"), "{stderr}");
    assert!(stderr.contains("not recommended for use"), "{stderr}");
}

#[test]
fn a_failing_step_stops_the_run() {
    let home = scratch("fail");
    let yaml = "version: 1\nname: broken\nsummary: Fails in the middle\nsteps:\n\
                \x20 - id: first\n    script: fail.sh\n\
                \x20 - id: second\n    command: config list\n";
    let dir = write_workflow(&home, "broken", yaml, &[("fail.sh", "exit 3\n")]);
    assert!(run(&home, &["workflow", "install", dir.to_str().unwrap()])
        .status
        .success());

    let out = run(&home, &["workflow", "run", "broken", "-o", "json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("exited with 3"), "{stderr}");
    assert!(!stderr.contains("[2/2]"), "the second step ran: {stderr}");
}

#[test]
fn dry_run_runs_nothing() {
    let home = scratch("dry-run");
    let marker = home.join("ran");
    let yaml = "version: 1\nname: touch\nsummary: Leaves a file behind\n\
                inputs:\n  path: { type: string, required: true }\nsteps:\n\
                \x20 - id: touch\n    script: touch.sh\n    args: ['${{ inputs.path }}']\n";
    let dir = write_workflow(&home, "touch", yaml, &[("touch.sh", "touch \"$1\"\n")]);
    assert!(run(&home, &["workflow", "install", dir.to_str().unwrap()])
        .status
        .success());

    let path = marker.display().to_string();
    let out = run(
        &home,
        &[
            "workflow",
            "run",
            "touch",
            "--path",
            &path,
            "--dry-run",
            "-o",
            "json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_json(&out)["steps"][0]["run"], "sh scripts/touch.sh");
    assert!(!marker.exists(), "--dry-run ran the step");

    let out = run(&home, &["workflow", "run", "touch", "--path", &path]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(marker.exists(), "the real run did not run the step");
}

#[test]
fn install_refuses_an_invalid_workflow_and_writes_nothing() {
    let home = scratch("invalid");
    let yaml = "version: 1\nname: bad\nsummary: Names a command that is not one\nsteps:\n\
                \x20 - id: a\n    command: styles nope\n";
    let dir = write_workflow(&home, "bad", yaml, &[("unused.sh", "")]);

    let out = run(
        &home,
        &["workflow", "install", dir.to_str().unwrap(), "-o", "json"],
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("invalid_workflow"), "{stderr}");
    assert!(stderr.contains("scripts/unused.sh"), "{stderr}");
    assert!(!home.join(".mapbox/workflows/bad").exists());
}

#[test]
fn install_replaces_only_with_force_and_uninstall_removes() {
    let home = scratch("lifecycle");
    install_demo(&home);
    let installed = home.join(".mapbox/workflows/demo");
    assert!(installed.join("workflow.yaml").is_file());
    assert!(installed.join(".install.json").is_file());

    let source = home.join("src/demo");
    let again = run(&home, &["workflow", "install", source.to_str().unwrap()]);
    assert_eq!(again.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&again.stderr).contains("already installed"));

    let forced = run(
        &home,
        &["workflow", "install", source.to_str().unwrap(), "--force"],
    );
    assert!(
        forced.status.success(),
        "{}",
        String::from_utf8_lossy(&forced.stderr)
    );

    let listed = stdout_json(&run(&home, &["workflow", "list", "-o", "json"]));
    assert_eq!(listed[0]["name"], "demo");

    let removed = run(&home, &["workflow", "uninstall", "demo", "-o", "json"]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!installed.exists());

    let missing = run(&home, &["workflow", "run", "demo"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("mapbox workflow install demo"));
}

#[test]
fn uninstall_removes_nothing_outside_the_installed_workflows() {
    let home = scratch("escape");
    std::fs::create_dir_all(home.join("keep")).unwrap();
    for name in ["../keep", "/tmp", "..", "../.."] {
        let out = run(&home, &["workflow", "uninstall", name]);
        assert_eq!(out.status.code(), Some(1), "{name}");
    }
    assert!(home.join("keep").exists());
}

#[test]
fn uninstall_takes_the_directory_install_was_given() {
    let home = scratch("uninstall-path");
    install_demo(&home);
    let source = home.join("src/demo");

    // Read from the JSON error rather than matched in its text: JSON escapes a
    // Windows path's backslashes, so the raw text never holds the path as typed.
    let again = run(
        &home,
        &[
            "workflow",
            "install",
            source.to_str().unwrap(),
            "-o",
            "json",
        ],
    );
    let stderr = String::from_utf8_lossy(&again.stderr);
    let error: Value = stderr
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("no JSON error on stderr:\n{stderr}"));
    assert_eq!(
        error["next_actions"][0],
        json!(format!(
            "mapbox workflow install {} --force",
            source.display()
        )),
        "{stderr}"
    );

    let out = run(&home, &["workflow", "uninstall", source.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!home.join(".mapbox/workflows/demo").exists());
    assert!(
        source.join("workflow.yaml").is_file(),
        "the source directory was touched"
    );
}

#[test]
fn inputs_are_flags_like_any_other_command() {
    let home = scratch("flags");
    install_demo(&home);

    let help = run(&home, &["workflow", "run", "demo", "--help"]);
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(text.contains("--greeting <GREETING>"), "{text}");
    assert!(text.contains("[default: hello]"), "{text}");

    // Clap's own usage error, exit 2, as for a typo on any command.
    let typo = run(&home, &["workflow", "run", "demo", "--greting", "hi"]);
    assert_eq!(typo.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&typo.stderr).contains("--greeting"));

    // Globals still work after the workflow's name.
    let out = run(
        &home,
        &[
            "workflow",
            "run",
            "demo",
            "--greeting=yo",
            "--token",
            TOKEN,
            "-o",
            "json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_json(&out)["shouted"], "YO");
}

#[test]
fn a_missing_required_input_names_its_flag() {
    let home = scratch("required");
    let yaml = "version: 1\nname: needs\nsummary: Needs an input\n\
                inputs:\n  style_id: { type: string, required: true }\nsteps:\n\
                \x20 - id: a\n    command: config list\n";
    let dir = write_workflow(&home, "needs", yaml, &[]);
    std::fs::remove_dir(dir.join("scripts")).unwrap();
    assert!(run(&home, &["workflow", "install", dir.to_str().unwrap()])
        .status
        .success());

    let out = run(&home, &["workflow", "run", "needs"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--style-id <STYLE_ID>"));

    // The underscore spelling is accepted as well.
    let out = run(
        &home,
        &["workflow", "run", "needs", "--style_id", "x", "-o", "json"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_workflow_that_is_not_installed_says_how_to_install_it() {
    let home = scratch("not-installed");
    let out = run(&home, &["workflow", "run", "copy-style", "--style-id", "x"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("mapbox workflow install copy-style"));
}

#[test]
fn history_records_the_command_and_not_the_workflow_name() {
    let home = scratch("history");
    install_demo(&home);
    assert!(run(&home, &["workflow", "run", "demo", "--token", TOKEN])
        .status
        .success());

    let listed = stdout_json(&run(&home, &["history", "list", "-o", "json"]));
    assert_eq!(listed[0]["command"], json!(["workflow", "run"]), "{listed}");
    assert!(!listed.to_string().contains("demo"), "{listed}");
}
