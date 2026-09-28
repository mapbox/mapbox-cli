//! End-to-end tests for command history and `mapbox history`.
//!
//! The unit tests in `src/run_history.rs` and `src/history.rs` cover the
//! pure parts — which runs are recorded, what a line keeps, how an id prefix
//! resolves. What they cannot show is what a real run leaves on disk: a line
//! by default, none of what was typed after the command path, nothing at all
//! with history off, and `history` reading back what the runs before it
//! recorded.
//!
//! Nothing here reaches the network. The one request a test makes goes to a
//! proxy on a loopback port nobody is listening on, so it fails at once and
//! is still a request `http::send` saw.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// A token-shaped fake. Nothing of it may reach history.
const TOKEN: &str = "pk.eyJ1IjoiZXhhbXBsZS11c2VyIiwiYSI6IngifQ.SIGNATURE-NOT-FOR-HISTORY";
const SEARCH: &str = "1600 Pennsylvania Ave";

fn scratch(name: &str) -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("history-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create the scratch home");
    home
}

fn config_dir(home: &Path) -> PathBuf {
    home.join(".mapbox")
}

fn history_dir(home: &Path) -> PathBuf {
    config_dir(home).join("history")
}

fn command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("MAPBOX_HISTORY")
        .env_remove("SUDO_USER")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .env("MAPBOX_NO_UPDATE_CHECK", "1")
        // Telemetry keeps its own directory beside `history/`.
        .env("MAPBOX_CLI_NO_TELEMETRY", "1")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("MAPBOX_CONFIG_DIR", config_dir(home));
    cmd
}

fn run(home: &Path, args: &[&str]) -> Output {
    command(home).args(args).output().expect("run mapbox")
}

/// Every recorded line, oldest first, and the raw bytes they came from.
fn lines(home: &Path) -> (Vec<Value>, String) {
    let Ok(entries) = std::fs::read_dir(history_dir(home)) else {
        return (vec![], String::new());
    };
    let mut files: Vec<PathBuf> = entries.map(|e| e.expect("an entry").path()).collect();
    files.sort();
    let raw: String = files
        .iter()
        .map(|f| std::fs::read_to_string(f).expect("read a history file"))
        .collect();
    let parsed = raw
        .lines()
        .map(|l| serde_json::from_str(l).expect("a history line is JSON"))
        .collect();
    (parsed, raw)
}

/// A request that fails before it leaves the machine: through a proxy on a
/// loopback port with nothing listening.
fn a_refused_search(home: &Path) -> Output {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        listener.local_addr().expect("the bound address").port()
    };
    command(home)
        .env("HTTPS_PROXY", format!("http://127.0.0.1:{port}"))
        .args(["search", "forward", "--q", SEARCH, "--token", TOKEN])
        .output()
        .expect("run mapbox")
}

#[test]
fn a_run_is_recorded_by_default_with_its_command_path_only() {
    let home = scratch("default");
    let out = a_refused_search(&home);
    assert!(!out.status.success(), "the request cannot have succeeded");

    let (lines, raw) = lines(&home);
    assert_eq!(lines.len(), 1, "{raw}");
    let line = &lines[0];
    assert_eq!(line["command"], serde_json::json!(["search", "forward"]));
    assert_eq!(line["exitCode"], 1);
    assert_eq!(line["requestCount"], 1);
    assert!(line["errorCode"].is_string(), "{line}");
    assert!(
        line["id"].as_str().is_some_and(|id| id.len() == 36),
        "{line}"
    );

    for typed in [SEARCH, "Pennsylvania", TOKEN, "SIGNATURE", "example-user"] {
        assert!(!raw.contains(typed), "history kept {typed:?}: {raw}");
    }
}

#[test]
fn a_run_leaves_only_history_and_with_it_off_nothing() {
    let home = scratch("on");
    run(&home, &["styles", "lsit"]);
    let created: Vec<_> = std::fs::read_dir(config_dir(&home))
        .expect("the config directory")
        .map(|e| e.expect("an entry").file_name())
        .collect();
    assert_eq!(created, ["history"]);

    let home = scratch("off");
    let out = command(&home)
        .env("MAPBOX_HISTORY", "0")
        .args(["styles", "lsit"])
        .output()
        .expect("run mapbox");
    assert!(!out.status.success());
    assert!(
        !config_dir(&home).exists(),
        "a run with history off created {}",
        config_dir(&home).display()
    );
}

#[test]
fn the_setting_turns_it_off_and_the_variable_overrides_the_setting() {
    let home = scratch("switches");
    assert!(run(&home, &["config", "set", "history", "off"])
        .status
        .success());
    run(&home, &["styles", "lsit"]);
    assert_eq!(lines(&home).0.len(), 0, "off by the setting");

    command(&home)
        .env("MAPBOX_HISTORY", "1")
        .args(["styles", "lsit"])
        .output()
        .expect("run mapbox");
    assert_eq!(
        lines(&home).0.len(),
        1,
        "`MAPBOX_HISTORY=1` wins over the setting"
    );
}

#[test]
fn help_version_completion_history_and_sudo_are_not_recorded() {
    let home = scratch("skipped");
    for args in [
        &["--help"][..],
        &["--version"],
        &["styles", "--help"],
        &["completion", "zsh"],
        &["history", "list"],
    ] {
        assert!(run(&home, args).status.success(), "{args:?}");
    }
    command(&home)
        .env("SUDO_USER", "someone")
        .args(["styles", "lsit"])
        .output()
        .expect("run mapbox");
    assert!(
        !config_dir(&home).exists(),
        "a run history skips created {}",
        config_dir(&home).display()
    );
}

#[test]
fn history_reads_back_the_runs() {
    let home = scratch("read-back");
    a_refused_search(&home);
    run(&home, &["styles", "lsit"]);

    let list = run(&home, &["-o", "json", "history", "list"]);
    assert!(list.status.success());
    let listed: Value = serde_json::from_slice(&list.stdout).expect("a JSON list");
    let listed = listed.as_array().expect("an array");
    assert_eq!(listed.len(), 2);
    assert_eq!(
        listed[0]["command"],
        serde_json::json!(["styles"]),
        "newest first"
    );
    let older = listed[1]["id"].as_str().expect("an id");

    let show = run(&home, &["-o", "json", "history", "show", &older[..8]]);
    assert!(show.status.success());
    let shown: Value = serde_json::from_slice(&show.stdout).expect("a JSON run");
    assert_eq!(shown["id"], older);
    assert_eq!(shown["command"], serde_json::json!(["search", "forward"]));

    let text = run(&home, &["-o", "text", "history", "list"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.starts_with("ID "), "a header row first: {text}");
    assert!(text.contains("mapbox search forward"), "{text}");
    assert!(!text.contains(SEARCH), "{text}");

    let missing = run(&home, &["-o", "json", "history", "show", "zzzz"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains(r#""code":"history_not_found""#),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

#[test]
fn history_with_history_off_says_so_on_stderr() {
    let home = scratch("read-off");
    let out = command(&home)
        .env("MAPBOX_HISTORY", "0")
        .args(["-o", "json", "history", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "[]");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("mapbox config set history on"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn days_past_the_thirty_day_window_are_removed() {
    let home = scratch("retention");
    std::fs::create_dir_all(history_dir(&home)).expect("the history directory");
    let old = history_dir(&home).join("2000-01-01.jsonl");
    std::fs::write(&old, "{}\n").expect("an old file");
    let not_history = history_dir(&home).join("notes.txt");
    std::fs::write(&not_history, "mine").expect("an unrelated file");

    run(&home, &["styles", "lsit"]);
    assert!(!old.exists(), "a file from 2000 outlived a 30-day window");
    assert!(not_history.exists(), "only dated history files are removed");
}
