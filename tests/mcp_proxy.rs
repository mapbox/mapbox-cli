//! End-to-end tests for `mapbox mcp`, against a stub standing in for the
//! `claude` CLI.
//!
//! `src/mcp.rs`'s own unit tests cover the pure table/parsing logic. What
//! they cannot cover is the actual subprocess handoff — the point of this
//! command is shelling out to another CLI, so these run the real `mapbox`
//! binary against a fake `claude`, the same shape
//! `tests/tilesets_cli_proxy.rs` uses for the real `tilesets` proxy.
//!
//! Unix-only: the stub is a shell script, which Windows would not run
//! anyway (`MAPBOX_CLAUDE_CLI` still works with a real `.exe` there, but
//! there is nothing here to build one from).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Stands in for `claude`: answers `--version`, `mcp get <name>` and `mcp
/// add ...`, each controlled by an environment variable so one stub script
/// covers every scenario below rather than one per test.
///
/// `STUB_GET_EXIT` (default 1, "not installed") is what `mcp get` exits
/// with. `STUB_ADD_EXIT` (default 0) is what `mcp add` exits with, and it
/// also writes `add-called` to a marker file so a test can assert `add` was
/// never reached at all (the `--dry-run` and already-installed cases).
const STUB: &str = r#"#!/bin/sh
case "$1" in
    --version)
        echo "stub-claude 0.0.0"
        exit 0
        ;;
    mcp)
        case "$2" in
            get)
                echo "get-called:$3"
                exit "${STUB_GET_EXIT:-1}"
                ;;
            add)
                echo "add-called:$*" >>"$STUB_MARKER"
                if [ "${STUB_ADD_EXIT:-0}" != "0" ]; then
                    echo "some failure detail" >&2
                fi
                exit "${STUB_ADD_EXIT:-0}"
                ;;
        esac
        ;;
esac
exit 1
"#;

/// Writes the stub and a fresh, empty marker file under this test target's
/// temp dir, named per test so parallel tests never share either.
fn stub_for(test: &str) -> (PathBuf, PathBuf) {
    let stub = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("claude-stub-{test}"));
    std::fs::write(&stub, STUB).expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");

    let marker = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("claude-marker-{test}"));
    std::fs::write(&marker, "").expect("write marker");

    (stub, marker)
}

fn marker_was_written(marker: &Path) -> bool {
    std::fs::read_to_string(marker)
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// Runs `mapbox mcp <args>` with `MAPBOX_CLAUDE_CLI` pointed at the stub.
fn run(stub: &Path, marker: &Path, get_exit: &str, add_exit: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .arg("mcp")
        .args(args)
        .env("MAPBOX_CLAUDE_CLI", stub)
        .env("STUB_MARKER", marker)
        .env("STUB_GET_EXIT", get_exit)
        .env("STUB_ADD_EXIT", add_exit)
        .output()
        .expect("run the mapbox binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_fresh_server_is_installed() {
    let (stub, marker) = stub_for("fresh");
    let output = run(
        &stub,
        &marker,
        "1", // mcp get: not installed
        "0", // mcp add: succeeds
        &[
            "install",
            "--server",
            "mapbox",
            "--client",
            "claude-code",
            "-o",
            "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(marker_was_written(&marker), "add was never called");
    let stdout = stdout(&output);
    assert!(stdout.contains("installed"), "{stdout:?}");
    assert!(!stdout.contains("already installed"), "{stdout:?}");
}

#[test]
fn an_already_installed_server_is_left_alone() {
    let (stub, marker) = stub_for("already-installed");
    let output = run(
        &stub,
        &marker,
        "0", // mcp get: already there
        "0",
        &[
            "install",
            "--server",
            "mapbox",
            "--client",
            "claude-code",
            "-o",
            "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        !marker_was_written(&marker),
        "add was called for a server that was already installed"
    );
    assert!(stdout(&output).contains("already installed"));
}

#[test]
fn dry_run_never_calls_add() {
    let (stub, marker) = stub_for("dry-run");
    let output = run(
        &stub,
        &marker,
        "1", // not installed
        "0",
        &[
            "install",
            "--server",
            "mapbox",
            "--client",
            "claude-code",
            "--dry-run",
            "-o",
            "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!marker_was_written(&marker), "--dry-run ran add anyway");
    assert!(stdout(&output).contains("would install"));
}

#[test]
fn a_failed_add_is_reported_without_failing_the_whole_run() {
    let (stub, marker) = stub_for("add-fails");
    let output = run(
        &stub,
        &marker,
        "1", // not installed
        "1", // add fails
        &[
            "install",
            "--server",
            "mapbox",
            "--client",
            "claude-code",
            "-o",
            "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(marker_was_written(&marker));
    assert!(stdout(&output).contains("failed"), "{}", stdout(&output));
}

#[test]
fn global_passes_scope_user_to_add() {
    let (stub, marker) = stub_for("global");
    let output = run(
        &stub,
        &marker,
        "1",
        "0",
        &[
            "install",
            "--server",
            "mapbox",
            "--client",
            "claude-code",
            "--global",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let logged = std::fs::read_to_string(&marker).expect("read marker");
    assert!(logged.contains("--scope user"), "{logged:?}");
}

#[test]
fn no_global_omits_scope_and_lets_claude_default_to_local() {
    let (stub, marker) = stub_for("no-global");
    let output = run(
        &stub,
        &marker,
        "1",
        "0",
        &["install", "--server", "mapbox", "--client", "claude-code"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let logged = std::fs::read_to_string(&marker).expect("read marker");
    assert!(!logged.contains("--scope"), "{logged:?}");
}

#[test]
fn a_client_not_on_path_is_skipped_and_named() {
    let (_stub, marker) = stub_for("client-not-found");
    // MAPBOX_CLAUDE_CLI points at nothing at all — every client is
    // unreachable regardless of --client, the same as a bare `claude` that
    // was never installed.
    let output = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["mcp", "install"])
        .env("MAPBOX_CLAUDE_CLI", "/nonexistent/claude")
        .env("STUB_MARKER", &marker)
        .output()
        .expect("run the mapbox binary");
    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr(&output);
    assert!(stderr.contains("mcp_client_not_found") || stderr.contains("No supported"));
}

#[test]
fn list_reports_status_for_every_known_server_and_client() {
    let (stub, marker) = stub_for("list");
    let output = run(
        &stub,
        &marker,
        "0", // installed
        "0",
        &["list", "-o", "json"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = stdout(&output);
    assert!(stdout.contains("\"server\":\"mapbox\""), "{stdout:?}");
    assert!(
        stdout.contains("\"server\":\"mapbox-devkit\""),
        "{stdout:?}"
    );
    assert!(stdout.contains("\"status\":\"installed\""), "{stdout:?}");
}
