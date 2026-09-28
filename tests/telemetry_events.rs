//! End-to-end tests for the run's `cli.command` event.
//!
//! The unit tests in `src/telemetry_event.rs` cover the pure parts — how an argument
//! is classified, what a timestamp looks like, which files pruning may
//! touch. What they cannot show is what a real run leaves behind: that the
//! event lands where it should, carries what the command did and nothing the
//! user typed, disappears when telemetry is off, and never changes stdout.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// A token whose payload claims the account `example-user`. The signature
/// is the part that must never appear in an event.
const TOKEN: &str = "pk.eyJ1IjoiZXhhbXBsZS11c2VyIiwiYSI6IngifQ.SIGNATURE-NOT-FOR-EVENTS";
const ADDRESS: &str = "1600 Pennsylvania Ave";

fn scratch(name: &str) -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("events-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    // With `.mapbox` already there, as on any machine that has logged in:
    // the file sink never creates it.
    std::fs::create_dir_all(config_dir(&home)).expect("create the scratch config dir");
    home
}

fn config_dir(home: &Path) -> PathBuf {
    home.join(".mapbox")
}

fn command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("MAPBOX_CLI_NO_TELEMETRY")
        .env("MAPBOX_NO_UPDATE_CHECK", "1")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("MAPBOX_CONFIG_DIR", config_dir(home));
    cmd
}

fn run(home: &Path, args: &[&str]) -> Output {
    command(home).args(args).output().expect("run mapbox")
}

/// Every event written under `home`, oldest first.
fn events(home: &Path) -> Vec<Value> {
    let dir = config_dir(home).join(".telemetry");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = entries
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|f| {
            std::fs::read_to_string(f)
                .expect("read an event file")
                .lines()
                .map(|l| serde_json::from_str(l).expect("an event line is JSON"))
                .collect::<Vec<Value>>()
        })
        .collect()
}

#[test]
fn a_run_writes_one_event_with_what_it_did() {
    let home = scratch("one");
    let data = r#"{"name":"secret-style-name","layers":[]}"#;
    let out = run(
        &home,
        &[
            "styles",
            "create",
            "--username",
            "someone",
            "--data",
            data,
            "--dry-run",
            "-t",
            TOKEN,
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let events = events(&home);
    assert_eq!(events.len(), 1, "{events:?}");
    let event = &events[0];
    assert_eq!(event["event"], "cli.command");
    assert_eq!(event["sdkIdentifier"], "mapbox-cli");
    assert_eq!(event["command"], serde_json::json!(["styles", "create"]));
    assert_eq!(event["invocation"], "execute");
    assert_eq!(event["exitCode"], 0);
    assert_eq!(event["dryRun"], true);
    assert_eq!(event["auth"]["source"], "flag");
    assert_eq!(event["auth"]["type"], "pk");
    assert_eq!(event["auth"]["account"], "example-user");
    assert_eq!(
        event["stdoutBytes"].as_u64(),
        Some(out.stdout.len() as u64),
        "stdoutBytes should be what was written"
    );
    let params = event["params"].as_array().expect("params");
    assert!(params.contains(&serde_json::json!({
        "name": "data", "bytes": data.len(), "keys": ["layers", "name"]
    })));
    assert!(params.contains(&serde_json::json!({ "name": "username", "length": 7 })));
}

#[test]
fn nothing_the_user_typed_reaches_the_event() {
    let home = scratch("private");
    // No network needed: a usage error still records, and `--dry-run` sends
    // nothing.
    let _ = run(
        &home,
        &[
            "styles",
            "create",
            "--username",
            "someone",
            "--data",
            "{\"x\":1}",
            "--dry-run",
            "-t",
            TOKEN,
        ],
    );
    let _ = run(
        &home,
        &[
            "geocoder",
            "forward",
            "--q",
            ADDRESS,
            "--no-such-flag",
            "-t",
            TOKEN,
        ],
    );
    let _ = run(&home, &["/Users/someone/secret/path"]);

    let written = std::fs::read_dir(config_dir(&home).join(".telemetry"))
        .expect("the telemetry directory")
        .map(|e| std::fs::read_to_string(e.expect("an entry").path()).unwrap_or_default())
        .collect::<String>();
    for secret in [
        "SIGNATURE-NOT-FOR-EVENTS",
        ADDRESS,
        "someone",
        "secret/path",
    ] {
        assert!(
            !written.contains(secret),
            "`{secret}` reached the event files:\n{written}"
        );
    }
    assert_eq!(events(&home).len(), 3);
}

#[test]
fn help_version_and_usage_errors_record_their_invocation() {
    let home = scratch("invocation");
    let _ = run(&home, &["--version"]);
    let _ = run(&home, &["styles", "--help"]);
    let _ = run(&home, &["styles", "list", "--schema"]);
    let _ = run(&home, &["nosuchcommand"]);

    let events = events(&home);
    let seen: Vec<(&str, &Value)> = events
        .iter()
        .map(|e| (e["invocation"].as_str().unwrap_or(""), &e["command"]))
        .collect();
    assert_eq!(
        seen,
        [
            ("version", &Value::Null),
            ("help", &serde_json::json!(["styles"])),
            ("schema", &serde_json::json!(["styles", "list"])),
            ("execute", &Value::Null),
        ]
    );
    assert_eq!(events[3]["usageError"], "InvalidSubcommand");
    assert_eq!(events[3]["errorCode"], "usage");
    assert_eq!(events[3]["exitCode"], 2);
}

#[test]
fn either_opt_out_records_nothing() {
    let home = scratch("opt-out-env");
    let out = command(&home)
        .env("MAPBOX_CLI_NO_TELEMETRY", "1")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert!(
        !config_dir(&home).join(".telemetry").exists(),
        "MAPBOX_CLI_NO_TELEMETRY=1 still wrote telemetry"
    );

    let home = scratch("opt-out-config");
    assert!(run(&home, &["config", "set", "telemetry", "off"])
        .status
        .success());
    // That run records nothing either: the setting it wrote is read at exit.
    let _ = run(&home, &["config", "list"]);
    assert_eq!(events(&home), Vec::<Value>::new());
}

#[test]
fn stdout_is_identical_with_telemetry_on_and_off() {
    let on = run(&scratch("stdout-on"), &["-o", "json", "config", "list"]);
    let off = command(&scratch("stdout-off"))
        .env("MAPBOX_CLI_NO_TELEMETRY", "1")
        .args(["-o", "json", "config", "list"])
        .output()
        .expect("run mapbox");
    assert_eq!(on.stdout, off.stdout);
    assert_eq!(on.stderr, off.stderr);
}

#[test]
fn completion_records_nothing() {
    let home = scratch("completion");
    assert!(run(&home, &["completion", "zsh"]).status.success());
    assert!(
        !config_dir(&home).join(".telemetry").exists(),
        "`completion` wrote telemetry"
    );
}

/// Recording never creates the config directory: a machine that has never
/// logged in or set a config keeps no `~/.mapbox` at all.
#[test]
fn without_a_config_directory_nothing_is_created() {
    let home = scratch("no-config-dir");
    std::fs::remove_dir(config_dir(&home)).expect("remove the scratch config dir");
    assert!(run(&home, &["styles", "--help"]).status.success());
    assert!(
        !config_dir(&home).exists(),
        "recording created {}",
        config_dir(&home).display()
    );
}

#[test]
fn a_run_started_by_a_workflow_step_records_its_parent() {
    let parent = "5f0c1e9a-7b2d-4c1e-9f3a-2d8e6b1a0c47";
    let home = scratch("parent");
    let out = command(&home)
        .env("MAPBOX_CLI_PARENT_EVENT", parent)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    // Anything that isn't an event id is ignored rather than recorded.
    let _ = command(&home)
        .env("MAPBOX_CLI_PARENT_EVENT", "/Users/someone/secret")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");

    let events = events(&home);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["parentEventId"], parent);
    assert_ne!(events[0]["eventId"], parent);
    assert!(events[1].get("parentEventId").is_none(), "{:?}", events[1]);
}
