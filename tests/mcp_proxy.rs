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

/// Every client's binary-override variable, each defaulted to a path that
/// doesn't exist. Applied first in every test's `Command`, so a real
/// `code`/`cursor`/`codex`/`claude` installed on the machine running this
/// suite never leaks into a scenario that isn't testing it — a test then
/// overrides only the one it cares about.
fn isolate_every_client(cmd: &mut Command) -> &mut Command {
    cmd.env("MAPBOX_CLAUDE_CLI", "/nonexistent/claude")
        .env("MAPBOX_CODEX_CLI", "/nonexistent/codex")
        .env("MAPBOX_CODE_CLI", "/nonexistent/code")
        .env("MAPBOX_CURSOR_CLI", "/nonexistent/cursor")
}

/// Runs `mapbox mcp <args>` with `MAPBOX_CLAUDE_CLI` pointed at the stub.
fn run(stub: &Path, marker: &Path, get_exit: &str, add_exit: &str, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    isolate_every_client(&mut cmd);
    cmd.arg("mcp")
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
    // Every client's binary override points at nothing at all, so
    // auto-detection finds none of them reachable.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    isolate_every_client(&mut cmd);
    let output = cmd
        .args(["mcp", "install"])
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

// --- Codex: same `mcp get`/`mcp add` shape as Claude Code, but `add` takes
// its name and URL differently, doesn't refuse a duplicate, and (for a
// server that advertises OAuth support) starts a login flow whose own exit
// code answers a different question than "was this written" — verified
// live against the real Mapbox hosted endpoint, see `src/mcp.rs`'s module
// docs. The stub below models exactly that: `add` always writes to
// `STUB_STATE` (unless told not to, for the one scenario that means nothing
// was written at all) regardless of what `STUB_CODEX_ADD_EXIT` says, and
// `get` answers from that state rather than a fixed exit code — because the
// real bug is precisely that those two are decoupled.

const CODEX_STUB: &str = r#"#!/bin/sh
case "$1" in
    --version)
        echo "stub-codex 0.0.0"
        exit 0
        ;;
    mcp)
        case "$2" in
            get)
                if [ -s "$STUB_STATE" ]; then exit 0; else exit 1; fi
                ;;
            add)
                echo "add-called:$*" >>"$STUB_MARKER"
                if [ "${STUB_CODEX_WRITES:-1}" = "1" ]; then
                    echo installed >"$STUB_STATE"
                fi
                exit "${STUB_CODEX_ADD_EXIT:-0}"
                ;;
        esac
        ;;
esac
exit 1
"#;

/// The Codex stub, its marker and its state file, each fresh and named per
/// test.
fn codex_stub_for(test: &str) -> (PathBuf, PathBuf, PathBuf) {
    let stub = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("codex-stub-{test}"));
    std::fs::write(&stub, CODEX_STUB).expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");

    let marker = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("codex-marker-{test}"));
    std::fs::write(&marker, "").expect("write marker");

    let state = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("codex-state-{test}"));
    let _ = std::fs::remove_file(&state);

    (stub, marker, state)
}

fn run_codex(
    stub: &Path,
    marker: &Path,
    state: &Path,
    add_exit: &str,
    writes: &str,
    args: &[&str],
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    isolate_every_client(&mut cmd);
    cmd.arg("mcp")
        .args(args)
        .env("MAPBOX_CODEX_CLI", stub)
        .env("STUB_MARKER", marker)
        .env("STUB_STATE", state)
        .env("STUB_CODEX_ADD_EXIT", add_exit)
        .env("STUB_CODEX_WRITES", writes)
        .output()
        .expect("run the mapbox binary")
}

#[test]
fn codex_fresh_install_succeeds_cleanly() {
    let (stub, marker, state) = codex_stub_for("fresh");
    let output = run_codex(
        &stub,
        &marker,
        &state,
        "0",
        "1",
        &[
            "install", "--server", "mapbox", "--client", "codex", "-o", "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(marker_was_written(&marker));
    let stdout = stdout(&output);
    assert!(stdout.contains("installed"), "{stdout:?}");
    assert!(!stdout.contains("login incomplete"), "{stdout:?}");
}

/// The real bug this whole design works around: `add` exits non-zero
/// because its own OAuth step failed, but the config entry is written
/// regardless. This must be reported as installed-with-a-caveat, not as a
/// flat failure.
#[test]
fn codex_add_that_fails_oauth_but_writes_the_entry_is_not_reported_as_failed() {
    let (stub, marker, state) = codex_stub_for("oauth-fails");
    let output = run_codex(
        &stub,
        &marker,
        &state,
        "1", // add's own exit code: OAuth step failed
        "1", // ...but it still wrote the entry
        &[
            "install", "--server", "mapbox", "--client", "codex", "-o", "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(marker_was_written(&marker));
    let stdout = stdout(&output);
    assert!(stdout.contains("login incomplete"), "{stdout:?}");
    assert!(!stdout.contains(": failed"), "{stdout:?}");
}

/// The other half of that bug: when `add` really did fail and nothing was
/// written, this must still report a genuine failure rather than assuming
/// success just because the exit code alone can't be trusted here.
#[test]
fn codex_add_that_writes_nothing_is_reported_as_failed() {
    let (stub, marker, state) = codex_stub_for("writes-nothing");
    let output = run_codex(
        &stub,
        &marker,
        &state,
        "1",
        "0", // nothing was actually written this time
        &[
            "install", "--server", "mapbox", "--client", "codex", "-o", "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(marker_was_written(&marker));
    assert!(stdout(&output).contains("failed"), "{}", stdout(&output));
}

#[test]
fn codex_already_installed_is_left_alone() {
    let (stub, marker, state) = codex_stub_for("already-installed");
    std::fs::write(&state, "installed").expect("seed state");
    let output = run_codex(
        &stub,
        &marker,
        &state,
        "0",
        "1",
        &[
            "install", "--server", "mapbox", "--client", "codex", "-o", "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!marker_was_written(&marker), "add was called anyway");
    assert!(stdout(&output).contains("already installed"));
}

// --- VS Code / Cursor: no `mcp` subcommand at all, just a top-level
// `--add-mcp '<json>'` flag that neither refuses a duplicate name nor
// offers a `get`/`list` to check one — so this command reads the config
// file directly instead of ever writing it, and each editor keeps that
// file somewhere genuinely different (`src/mcp.rs`'s `AddMcpConfig`,
// confirmed live for each). The stub only needs to answer `--version` and
// `--add-mcp`; the "already installed" case is set up by writing the real
// config file shape directly, the same file `file_get` reads, rather than
// teaching the stub to simulate a merge.

const ADD_MCP_FLAG_STUB: &str = r#"#!/bin/sh
case "$1" in
    --version)
        echo "stub-editor 0.0.0"
        exit 0
        ;;
    --add-mcp)
        echo "add-mcp-called:$2" >>"$STUB_MARKER"
        exit "${STUB_ADD_EXIT:-0}"
        ;;
esac
exit 1
"#;

fn add_mcp_flag_stub_for(test: &str) -> (PathBuf, PathBuf) {
    let stub = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("editor-stub-{test}"));
    std::fs::write(&stub, ADD_MCP_FLAG_STUB).expect("write stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");

    let marker = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("editor-marker-{test}"));
    std::fs::write(&marker, "").expect("write marker");

    (stub, marker)
}

fn run_vscode(
    stub: &Path,
    marker: &Path,
    config_dir: &Path,
    add_exit: &str,
    args: &[&str],
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    isolate_every_client(&mut cmd);
    cmd.arg("mcp")
        .args(args)
        .env("MAPBOX_CODE_CLI", stub)
        .env("MAPBOX_CODE_CONFIG_DIR", config_dir)
        .env("STUB_MARKER", marker)
        .env("STUB_ADD_EXIT", add_exit)
        .output()
        .expect("run the mapbox binary")
}

#[test]
fn vscode_fresh_install_calls_add_mcp() {
    let (stub, marker) = add_mcp_flag_stub_for("vscode-fresh");
    let config_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("vscode-config-fresh");
    let output = run_vscode(
        &stub,
        &marker,
        &config_dir,
        "0",
        &[
            "install", "--server", "mapbox", "--client", "vscode", "-o", "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(marker_was_written(&marker), "--add-mcp was never called");
    assert!(stdout(&output).contains("installed"), "{}", stdout(&output));
}

/// The whole reason this reads the config file first: `--add-mcp` itself
/// would silently overwrite a same-named entry, so this command must never
/// call it for a server that's already there.
#[test]
fn vscode_already_installed_is_never_passed_to_add_mcp() {
    let (stub, marker) = add_mcp_flag_stub_for("vscode-already-installed");
    let config_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("vscode-config-already");
    let user_dir = config_dir.join("User");
    std::fs::create_dir_all(&user_dir).expect("create config dir");
    std::fs::write(
        user_dir.join("mcp.json"),
        r#"{"servers":{"mapbox":{"type":"http","url":"https://mcp.mapbox.com/mcp"}},"inputs":[]}"#,
    )
    .expect("seed mcp.json");

    let output = run_vscode(
        &stub,
        &marker,
        &config_dir,
        "0",
        &[
            "install", "--server", "mapbox", "--client", "vscode", "-o", "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        !marker_was_written(&marker),
        "--add-mcp was called for a server already in mcp.json"
    );
    assert!(stdout(&output).contains("already installed"));
}

/// An `mcp.json` that exists but doesn't parse must never be guessed past —
/// see `GetOutcome::Unreadable` in `src/mcp.rs`.
#[test]
fn vscode_unreadable_config_is_reported_rather_than_guessed_past() {
    let (stub, marker) = add_mcp_flag_stub_for("vscode-unreadable");
    let config_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("vscode-config-unreadable");
    let user_dir = config_dir.join("User");
    std::fs::create_dir_all(&user_dir).expect("create config dir");
    std::fs::write(user_dir.join("mcp.json"), "{not valid json").expect("seed mcp.json");

    let output = run_vscode(
        &stub,
        &marker,
        &config_dir,
        "0",
        &[
            "install", "--server", "mapbox", "--client", "vscode", "-o", "text",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        !marker_was_written(&marker),
        "--add-mcp was called despite an unreadable config"
    );
    assert!(
        stdout(&output).contains("could not be read"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn vscode_has_no_per_project_scope_and_says_so() {
    let (stub, marker) = add_mcp_flag_stub_for("vscode-no-global");
    let config_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("vscode-config-no-global");
    let output = run_vscode(
        &stub,
        &marker,
        &config_dir,
        "0",
        &["install", "--server", "mapbox", "--client", "vscode"],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("no per-project scope"),
        "{}",
        stderr(&output)
    );
}

/// Cursor speaks the identical `--add-mcp` flag but keeps the result
/// somewhere different (`settings.json`'s `"mcp"` key, not a dedicated
/// `mcp.json`) — confirmed live, see `src/mcp.rs`'s module docs. This
/// exercises that nested path rather than retesting `--add-mcp` itself.
#[test]
fn cursor_reads_its_own_settings_json_shape() {
    let (stub, marker) = add_mcp_flag_stub_for("cursor-already-installed");
    let config_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cursor-config-already");
    let user_dir = config_dir.join("User");
    std::fs::create_dir_all(&user_dir).expect("create config dir");
    std::fs::write(
        user_dir.join("settings.json"),
        r#"{"editor.fontSize":14,"mcp":{"servers":{"mapbox":{"type":"http","url":"https://mcp.mapbox.com/mcp"}}}}"#,
    )
    .expect("seed settings.json");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    isolate_every_client(&mut cmd);
    let output = cmd
        .arg("mcp")
        .args([
            "install", "--server", "mapbox", "--client", "cursor", "-o", "text",
        ])
        .env("MAPBOX_CURSOR_CLI", &stub)
        .env("MAPBOX_CURSOR_CONFIG_DIR", &config_dir)
        .env("STUB_MARKER", &marker)
        .env("STUB_ADD_EXIT", "0")
        .output()
        .expect("run the mapbox binary");

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        !marker_was_written(&marker),
        "--add-mcp was called for a server already in settings.json"
    );
    assert!(stdout(&output).contains("already installed"));
}
