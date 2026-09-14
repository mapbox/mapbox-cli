//! End-to-end tests for `--dry-run`.
//!
//! The flag's promise is a narrow one and worth pinning as a whole: on a
//! command that changes something, it prints what would be sent and changes
//! nothing. Both halves need a real process to check. The unit tests in
//! `src/executor.rs` cover the rendering as pure functions, and the ones in
//! `src/main.rs` pin which commands carry the flag; what only a child process
//! shows is which stream the plan lands on, what the exit code is, and that
//! the credential store on disk is the same afterwards.
//!
//! Nothing here reaches the network — that is most of the point — and nothing
//! touches the developer's own credentials: every test gets its own
//! `MAPBOX_CONFIG_DIR`, so the stores these write and read are their own.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};

/// A credential store of this test's own.
///
/// `MAPBOX_CONFIG_DIR` rather than a redirected `HOME`: it names the store
/// directly, so a test that asserts a file was left alone is pointing at the
/// same path the CLI resolved. One directory per test keeps the suite
/// parallel — `cargo test` runs these concurrently, and a shared store would
/// make "the file is still there" depend on test order.
fn config_dir(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("dry-run-{test}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create config dir");
    dir
}

/// Runs the real binary with the developer's own environment cleared.
///
/// A `MAPBOX_ACCESS_TOKEN` in the running shell would silently supply the
/// token these tests are asserting about, and `MAPBOX_OUTPUT` would change
/// the shape of every assertion.
fn command(config: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("MAPBOX_DEBUG")
        .env("MAPBOX_CONFIG_DIR", config);
    cmd
}

fn run(config: &Path, args: &[&str]) -> Output {
    command(config).args(args).output().expect("run mapbox")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("expected JSON, got {text:?} ({e})"))
}

/// A token shaped like a real one, carrying whatever claims are asked for.
///
/// `exp` is the one that matters here: a token without it is never considered
/// stale, so nothing tries to refresh it and nothing reaches the network.
fn fake_token(claims: &str) -> String {
    let payload = STANDARD_NO_PAD
        .encode(claims)
        .replace('+', "-")
        .replace('/', "_");
    format!("tk.{payload}.signature")
}

/// Puts a stored login in the config dir, as `mapbox auth login` would have.
fn write_login(config: &Path, access_token: &str) -> PathBuf {
    let path = config.join("credentials.json");
    let credentials = format!(
        r#"{{"access_token":"{access_token}","refresh_token":"a-refresh-token",
            "username":"zz-clitest","client_id":"a-client-id"}}"#
    );
    std::fs::write(&path, credentials).expect("write credentials");
    path
}

/// The whole contract in one command: the request is described, the exit code
/// is a success, and nothing about it says a call was made.
#[test]
fn a_mutating_command_describes_the_request_it_would_send() {
    let config = config_dir("describes");
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--username",
            "zz-clitest",
            "--token",
            "sk.a-test-token",
            "styles",
            "delete",
            "zz-clitest-style",
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let plan = json(&stdout(&out));
    assert_eq!(plan["dry_run"], true);
    assert_eq!(plan["command"], "styles delete");
    assert_eq!(plan["method"], "DELETE");
    assert_eq!(
        plan["url"],
        "https://api.mapbox.com/styles/v1/zz-clitest/zz-clitest-style"
    );
    // No body was given and none is invented.
    assert_eq!(plan["body"], serde_json::Value::Null);
}

/// The plan is the request, and the request carries the token in its query
/// string — so the one output whose whole purpose is to be read, pasted and
/// captured is also the one most able to leak a live credential.
#[test]
fn the_plan_never_carries_the_access_token() {
    let config = config_dir("redaction");
    let secret = "sk.zz-clitest-not-a-real-token";
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--debug",
            "--username",
            "zz-clitest",
            "--token",
            secret,
            "styles",
            "create",
            "--data",
            r#"{"name":"zz-clitest"}"#,
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let out_text = stdout(&out);
    let err_text = stderr(&out);
    assert!(!out_text.contains(secret), "token on stdout: {out_text}");
    assert!(!err_text.contains(secret), "token on stderr: {err_text}");
    assert_eq!(json(&out_text)["query"]["access_token"], "<redacted>");
}

/// A body reaches the plan as the document it parses to, not as the string it
/// was typed as — the one thing worth checking before a `create` runs for
/// real is what the API will receive.
#[test]
fn the_plan_shows_the_body_that_would_be_sent() {
    let config = config_dir("body");
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--username",
            "zz-clitest",
            "--token",
            "sk.a-test-token",
            "styles",
            "create",
            "--data",
            r#"{"name":"zz-clitest","version":8}"#,
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let body = &json(&stdout(&out))["body"];
    assert_eq!(body["source"], "--data");
    assert_eq!(body["content_type"], "application/json");
    assert_eq!(body["json"]["name"], "zz-clitest");
    assert_eq!(body["json"]["version"], 8);
}

/// What the flag is for. A dry run that accepted a body the real call would
/// reject would be worse than no dry run at all — it would be a green light
/// on the mistake it was asked to look for.
#[test]
fn a_dry_run_rejects_a_body_the_real_call_would_reject() {
    let config = config_dir("invalid-body");
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--username",
            "zz-clitest",
            "--token",
            "sk.a-test-token",
            "styles",
            "create",
            "--data",
            "{not json",
            "--dry-run",
        ],
    );

    assert!(!out.status.success());
    // A failure is not a result: stdout stays empty in every mode.
    assert_eq!(stdout(&out), "");
    assert_eq!(json(&stderr(&out))["code"], "invalid_data");
}

/// Offered only where it means something. A `--dry-run` on a `GET` would be a
/// flag that reads as a safety net and does nothing, so it is not a flag.
#[test]
fn a_read_only_command_does_not_take_dry_run() {
    let config = config_dir("read-only");
    let out = run(
        &config,
        &[
            "--username",
            "zz-clitest",
            "--token",
            "sk.a-test-token",
            "styles",
            "list",
            "--dry-run",
        ],
    );

    assert!(!out.status.success());
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("--dry-run"),
        "the error should name the argument: {}",
        stderr(&out)
    );
}

/// `auth logout` deletes a file, and which file depends on `--profile` and
/// `MAPBOX_CONFIG_DIR` — exactly the sort of thing worth being shown before
/// it happens rather than after.
#[test]
fn logout_names_the_file_it_would_delete_and_leaves_it_there() {
    let config = config_dir("logout");
    let path = write_login(&config, &fake_token(r#"{"u":"zz-clitest"}"#));

    let out = run(&config, &["-o", "json", "auth", "logout", "--dry-run"]);

    assert!(out.status.success(), "{}", stderr(&out));
    let plan = json(&stdout(&out));
    assert_eq!(plan["dry_run"], true);
    assert_eq!(plan["command"], "auth logout");
    assert_eq!(plan["would_delete"], true);
    assert_eq!(plan["credentials_path"], path.display().to_string());
    assert!(path.exists(), "the dry run deleted the credentials");
}

/// Nothing stored is not a failure — `auth logout` succeeds there too — but
/// the plan has to say so, or a reader takes "would delete" as read.
#[test]
fn logout_says_when_there_is_nothing_to_delete() {
    let config = config_dir("logout-empty");
    let out = run(&config, &["-o", "json", "auth", "logout", "--dry-run"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(json(&stdout(&out))["would_delete"], false);
}

/// A precondition the real command would fail on is a failure here too.
/// Describing a refresh that cannot run would be the same false green light
/// as accepting a body that cannot be sent.
#[test]
fn refresh_fails_the_dry_run_when_there_is_nothing_to_refresh() {
    let config = config_dir("refresh-empty");
    let out = run(&config, &["-o", "json", "auth", "refresh", "--dry-run"]);

    assert!(!out.status.success());
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("Not currently logged in"),
        "{}",
        stderr(&out)
    );
}

/// The subtlest thing a dry run could get wrong.
///
/// Resolving a token normally goes through `load_fresh_credentials`, which
/// spends the stored refresh token and rewrites the credentials file when the
/// access token has expired. That is a mutation, on the one code path that
/// promised not to make any — and it happens before the command being
/// previewed is even reached.
///
/// A stored token with an `exp` in the past is what triggers it. The
/// assertion is on `--debug`'s own line, printed before the request goes out,
/// so a regression fails here rather than depending on whether the network
/// was reachable.
#[test]
fn a_dry_run_does_not_spend_the_stored_refresh_token() {
    let config = config_dir("no-refresh");
    let expired = fake_token(r#"{"u":"zz-clitest","exp":1000000000}"#);
    let path = write_login(&config, &expired);
    let before = std::fs::read_to_string(&path).expect("read credentials");

    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--debug",
            "styles",
            "delete",
            "zz-clitest-style",
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("grant_type=refresh_token"),
        "the dry run tried to refresh: {}",
        stderr(&out)
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read credentials"),
        before,
        "the dry run rewrote the credentials file",
    );
}

/// Under `text` the plan is still the result, so it is still stdout — a
/// `--dry-run > plan.txt` has to catch the request, not an empty file.
#[test]
fn the_plan_is_a_result_and_goes_to_stdout() {
    let config = config_dir("text-mode");
    let out = run(
        &config,
        &[
            "-o",
            "text",
            "--username",
            "zz-clitest",
            "--token",
            "sk.a-test-token",
            "styles",
            "delete",
            "zz-clitest-style",
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Dry run"), "{text}");
    assert!(
        text.contains("DELETE https://api.mapbox.com/styles/v1/zz-clitest/zz-clitest-style"),
        "{text}"
    );
}
