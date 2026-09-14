//! End-to-end tests for the `mapbox tilesets-cli` proxy.
//!
//! The unit tests in `src/tilesets_cli.rs` cover argv parsing and token
//! selection in-process. They cannot cover the handoff itself: on Unix the
//! proxy `exec`s, so `run` never returns to a caller and there is nothing to
//! assert against. These tests run the real binary against a stub `tilesets`
//! that reports exactly what it received.
//!
//! Unix-only, matching the `exec` path being exercised. The stub is a shell
//! script, which Windows would not run anyway.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};

/// Stands in for `tilesets`: reports its argv and the token it was handed, one
/// field per line, then exits with a code distinctive enough that finding it on
/// the proxy proves the child's status was propagated rather than invented.
const STUB: &str = r#"#!/bin/sh
for a in "$@"; do printf 'arg=%s\n' "$a"; done
printf 'token=%s\n' "${MAPBOX_ACCESS_TOKEN-<unset>}"
exit 7
"#;

const STUB_EXIT: i32 = 7;

/// Writes the stub under this test target's temp dir. Named per test so
/// parallel tests never write the same path.
fn stub_for(test: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("tilesets-stub-{test}"));
    std::fs::write(&path, STUB).expect("write stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    path
}

/// A home directory of this test binary's own, holding no credentials.
///
/// Clearing `MAPBOX_ACCESS_TOKEN` is not enough on its own: with no token in
/// the environment the proxy falls through to *stored* credentials, so every
/// test here resolved the developer's real `~/.mapbox` and handed the token to
/// the stub — which reports it on stdout. Cargo hides a passing test's output,
/// so it only ever surfaced when some other assertion failed, printing a live
/// token into the terminal. The tests that want stored credentials point
/// `HOME` at a fixture of their own; this is the floor for everyone else.
fn sandbox_home() -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("proxy-empty-home");
    std::fs::create_dir_all(&home).expect("create sandbox home");
    home
}

/// Runs the real `mapbox` binary with the stub standing in for `tilesets`.
///
/// The developer's own `MAPBOX_ACCESS_TOKEN` is cleared so a token in the
/// shell can't make an assertion pass (or fail) that has nothing to do with
/// it, and `HOME` is redirected so a stored login cannot either.
fn run(stub: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(args)
        .env("MAPBOX_TILESETS_CLI", stub)
        .env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_YES")
        .env("HOME", sandbox_home())
        .output()
        .expect("run the mapbox binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Every `arg=` line the stub reported, in order.
fn args_seen(output: &Output) -> Vec<String> {
    stdout(output)
        .lines()
        .filter_map(|line| line.strip_prefix("arg=").map(str::to_owned))
        .collect()
}

/// The `token=` line the stub reported.
fn token_seen(output: &Output) -> String {
    stdout(output)
        .lines()
        .find_map(|line| line.strip_prefix("token=").map(str::to_owned))
        .expect("stub always reports a token line")
}

/// Every test below passes `--token` so the run is deterministic: without one,
/// the proxy would consult whatever credentials the machine happens to have
/// stored, and could make a refresh request over the network.
const TOKEN: [&str; 2] = ["--token", "sk.injected"];

/// A scratch `HOME`, optionally holding a stored login.
///
/// Pointing `HOME` at a scratch directory is what makes the stored-credential
/// paths testable at all: otherwise they read the developer's real login and
/// the result depends on who is running the suite. The store sits at
/// `$HOME/.mapbox` on every platform, so there is nothing here to vary by OS.
fn home_with_login(test: &str, account: Option<&str>) -> PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("home-{test}"));
    let _ = std::fs::remove_dir_all(&home);
    let config = home.join(".mapbox");
    std::fs::create_dir_all(&config).expect("create config dir");

    if let Some(account) = account {
        write_login(&config, account);
    }
    home
}

/// Puts a stored login in `dir`, as `mapbox auth login` would have.
///
/// The fake token carries a `u` claim but no `exp`, so nothing tries to
/// refresh it — these tests must not touch the network.
fn write_login(dir: &Path, account: &str) {
    let credentials = format!(
        r#"{{"access_token":"{}","username":"{account}"}}"#,
        fake_token("tk", account)
    );
    std::fs::write(dir.join("credentials.json"), credentials).expect("write credentials");
}

/// A token shaped like a real one — `<prefix>.<base64url payload>.<signature>`
/// — carrying an account in its `u` claim and no `exp`, so nothing tries to
/// refresh it. The claim has to be readable: the shadowed-login warning is
/// driven by it, and an unparseable token correctly produces no warning at all.
fn fake_token(prefix: &str, account: &str) -> String {
    let payload = STANDARD_NO_PAD
        .encode(format!(r#"{{"u":"{account}"}}"#))
        .replace('+', "-")
        .replace('/', "_");
    format!("{prefix}.{payload}.signature")
}

#[test]
fn argv_reaches_the_child_untouched() {
    let stub = stub_for("argv");
    let output = run(
        &stub,
        &[
            TOKEN[0],
            TOKEN[1],
            "tilesets-cli",
            "upload-source",
            "someone",
            "a-source",
            "--replace",
            "-q",
            "two words",
        ],
    );

    assert_eq!(
        args_seen(&output),
        [
            "upload-source",
            "someone",
            "a-source",
            "--replace",
            "-q",
            "two words"
        ],
        "argv must survive verbatim, quoting included"
    );
}

#[test]
fn the_childs_exit_code_becomes_the_proxys() {
    let stub = stub_for("exit-code");
    let output = run(&stub, &[TOKEN[0], TOKEN[1], "tilesets-cli", "anything"]);

    assert_eq!(output.status.code(), Some(STUB_EXIT));
}

/// The security property from Diagram 3: a token on a command line is readable
/// by any other local user through `ps`, so it must travel in the environment.
#[test]
fn the_token_travels_in_the_environment_never_in_argv() {
    let stub = stub_for("token-env");
    let output = run(&stub, &["--token", "sk.secret", "tilesets-cli", "whoami"]);

    assert_eq!(token_seen(&output), "sk.secret");
    assert_eq!(
        args_seen(&output),
        ["whoami"],
        "the token must not appear anywhere in the child's argv"
    );
}

/// A `--token` written *after* the subcommand belongs to `tilesets`, which
/// prefers its own flag over the environment — so it stays the per-call
/// override even though we also set the environment.
#[test]
fn a_token_after_the_subcommand_is_forwarded_instead() {
    let stub = stub_for("token-forwarded");
    let output = run(
        &stub,
        &[
            TOKEN[0],
            TOKEN[1],
            "tilesets-cli",
            "--token",
            "sk.forwarded",
            "list",
        ],
    );

    assert_eq!(args_seen(&output), ["--token", "sk.forwarded", "list"]);
    assert_eq!(token_seen(&output), "sk.injected");
}

/// Regression at the binary level for the clap bug this proxy had to solve:
/// `mapbox`'s globals are `.global(true)`, so clap parses them inside every
/// subcommand. After the subcommand name they are the child's.
#[test]
fn globals_written_after_the_subcommand_are_forwarded() {
    let stub = stub_for("globals");
    let output = run(
        &stub,
        &[
            TOKEN[0],
            TOKEN[1],
            "tilesets-cli",
            "--profile",
            "not-ours",
            "--debug",
            "list",
        ],
    );

    assert_eq!(
        args_seen(&output),
        ["--profile", "not-ours", "--debug", "list"]
    );
}

/// `tilesets` was reserved for a command of our own, and is now one: the
/// generated group holding `get-rastertile` and `get-vectortile`. Nothing
/// about it is the proxy, so a line written against the Python CLI's own
/// subcommands has to fail here rather than being forwarded.
#[test]
fn the_tilesets_name_does_not_reach_the_proxy() {
    let stub = stub_for("reserved-name");
    let output = run(&stub, &[TOKEN[0], TOKEN[1], "tilesets", "list", "someone"]);

    assert_ne!(
        output.status.code(),
        Some(STUB_EXIT),
        "the stub was reached"
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("list"),
        "the error should name the subcommand `mapbox tilesets` does not have"
    );
}

/// The environment outranks stored credentials, so the proxy must pass an
/// existing token straight through rather than resolving one of its own. Set
/// here explicitly rather than inherited, so the result doesn't depend on the
/// machine running the tests.
#[test]
fn an_environment_token_is_passed_through_untouched() {
    let stub = stub_for("env-token");
    let output = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["tilesets-cli", "list", "someone"])
        .env("MAPBOX_TILESETS_CLI", &stub)
        .env("MAPBOX_ACCESS_TOKEN", "pk.from-the-environment")
        .env_remove("MapboxAccessToken")
        .output()
        .expect("run the mapbox binary");

    assert_eq!(token_seen(&output), "pk.from-the-environment");
    assert_eq!(args_seen(&output), ["list", "someone"]);
}

/// The answer to "how do I force my login?" — `--use-login` skips the
/// environment even when it holds a perfectly valid token for someone else.
#[test]
fn use_login_prefers_the_stored_token_over_the_environment() {
    let stub = stub_for("use-login");
    let home = home_with_login("use-login", Some("stored-account"));

    let output = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["--use-login", "tilesets-cli", "list", "stored-account"])
        .env("MAPBOX_TILESETS_CLI", &stub)
        .env("MAPBOX_ACCESS_TOKEN", "pk.from-the-environment")
        .env_remove("MapboxAccessToken")
        .env("HOME", &home)
        .output()
        .expect("run the mapbox binary");

    assert!(
        token_seen(&output).starts_with("tk."),
        "expected the stored token, got {}",
        token_seen(&output)
    );
}

/// Without `--use-login` the same setup inherits the environment instead —
/// the pair of tests pins the precedence from both sides.
#[test]
fn without_use_login_the_environment_still_wins() {
    let stub = stub_for("env-over-stored");
    let home = home_with_login("env-over-stored", Some("stored-account"));

    let env_token = fake_token("pk", "someone-else");
    let output = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["tilesets-cli", "list", "stored-account"])
        .env("MAPBOX_TILESETS_CLI", &stub)
        .env("MAPBOX_ACCESS_TOKEN", &env_token)
        .env_remove("MapboxAccessToken")
        .env("HOME", &home)
        .output()
        .expect("run the mapbox binary");

    assert_eq!(token_seen(&output), env_token);

    let stderr = stderr(&output);
    assert!(stderr.contains("someone-else"), "{stderr}");
    assert!(stderr.contains("stored-account"), "{stderr}");
    assert!(
        stderr.contains("--use-login"),
        "the warning must say what to do: {stderr}"
    );
}

/// Falling back to the environment here would hand over the very token the
/// flag asked to ignore, so it has to be an error.
#[test]
fn use_login_without_a_login_is_an_error() {
    let stub = stub_for("use-login-empty");
    let home = home_with_login("use-login-empty", None);

    let output = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["--use-login", "tilesets-cli", "list", "someone"])
        .env("MAPBOX_TILESETS_CLI", &stub)
        .env("MAPBOX_ACCESS_TOKEN", "pk.from-the-environment")
        .env("HOME", &home)
        .output()
        .expect("run the mapbox binary");

    let stderr = stderr(&output);
    assert!(stderr.contains("mapbox auth login"), "{stderr}");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "the child must not have run at all"
    );
}

/// `MAPBOX_CONFIG_DIR` replaces the whole store rather than nudging it, which
/// is what lets a container or CI job keep a real login instead of falling
/// back to a raw `MAPBOX_ACCESS_TOKEN`. `HOME` here holds no login at all, so
/// an ignored override would fail this the same way `--use-login` fails with
/// nothing stored.
#[test]
fn the_config_dir_override_is_where_a_login_is_read_from() {
    let stub = stub_for("config-dir-override");
    let home = home_with_login("config-dir-override", None);
    let store = Path::new(env!("CARGO_TARGET_TMPDIR")).join("config-dir-override-store");
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(&store).expect("create the override directory");
    write_login(&store, "stored-account");

    let output = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["--use-login", "tilesets-cli", "list", "stored-account"])
        .env("MAPBOX_TILESETS_CLI", &stub)
        .env("MAPBOX_ACCESS_TOKEN", "pk.from-the-environment")
        .env_remove("MapboxAccessToken")
        .env("HOME", &home)
        .env("MAPBOX_CONFIG_DIR", &store)
        .output()
        .expect("run the mapbox binary");

    assert!(
        token_seen(&output).starts_with("tk."),
        "expected the login from {}, got {}",
        store.display(),
        token_seen(&output)
    );
}

/// `shadowed_login_warning` is unit-tested as a pure function; this is the
/// wrapper that reads the environment and the credential store and decides to
/// print it, which nothing else exercises. It matters because the precedence
/// it warns about is deliberate: the environment wins, so on the day someone
/// exports a token for the wrong account, this warning is all that stands
/// between them and a `Not found` from an API that means "not yours".
#[test]
fn an_environment_token_for_another_account_is_called_out() {
    let stub = stub_for("shadowed-login");
    let home = home_with_login("shadowed-login", Some("me"));

    let output = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["tilesets-cli", "list", "me"])
        .env("MAPBOX_TILESETS_CLI", &stub)
        // Parseable, unlike the `pk.from-the-environment` the precedence
        // tests use: an account it cannot read is correctly not warned about.
        .env("MAPBOX_ACCESS_TOKEN", fake_token("pk", "someone-else"))
        .env_remove("MapboxAccessToken")
        .env("HOME", &home)
        .output()
        .expect("run the mapbox binary");

    let warning = stderr(&output);
    assert!(warning.contains("someone-else"), "{warning}");
    assert!(warning.contains("`me`"), "{warning}");
    assert!(
        warning.contains("--use-login"),
        "the warning has to carry the way out: {warning}"
    );
}

#[test]
fn a_missing_binary_prints_install_guidance() {
    let output = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args([TOKEN[0], TOKEN[1], "tilesets-cli", "list", "someone"])
        .env_remove("MAPBOX_TILESETS_CLI")
        .env("PATH", "")
        .output()
        .expect("run the mapbox binary");

    let stderr = stderr(&output);
    assert!(stderr.contains("pipx install mapbox-tilesets"), "{stderr}");
    assert!(stderr.contains("MAPBOX_TILESETS_CLI"), "{stderr}");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn a_broken_override_blames_the_env_var() {
    let output = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args([TOKEN[0], TOKEN[1], "tilesets-cli", "list", "someone"])
        .env("MAPBOX_TILESETS_CLI", "/nonexistent/tilesets")
        .output()
        .expect("run the mapbox binary");

    let stderr = stderr(&output);
    assert!(stderr.contains("MAPBOX_TILESETS_CLI"), "{stderr}");
    assert!(stderr.contains("/nonexistent/tilesets"), "{stderr}");
    assert!(
        !stderr.contains("pipx"),
        "a bad override is not an install problem: {stderr}"
    );
    assert_eq!(output.status.code(), Some(1));
}

/// `mapbox --yes tilesets-cli delete <id>` reads as a non-interactive delete
/// and is not one: the child asks its own question and has never heard of
/// `--yes`. Silence there would break the flag's promise in the one place it
/// matters most — a CI job blocking on a prompt nobody can answer.
#[test]
fn yes_is_reported_as_not_reaching_the_child() {
    let stub = stub_for("yes-warned");
    let out = run(&stub, &["--yes", "tilesets-cli", "list", "someone"]);

    let warning = stderr(&out);
    assert!(warning.contains("--yes"), "{warning}");
    assert!(
        warning.contains("--force"),
        "the warning has to name the child's own flag: {warning}"
    );
    // Still forwarded and still run: this is advice, not interception.
    assert_eq!(args_seen(&out), ["list", "someone"]);
    assert_eq!(out.status.code(), Some(STUB_EXIT));
}

/// Exported once, it applies to everything, so warning on it would nag on
/// every tileset command forever — the reasoning `MAPBOX_OUTPUT` already
/// settled.
#[test]
fn the_environment_variable_is_not_warned_about() {
    let stub = stub_for("yes-env-quiet");
    let out = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["tilesets-cli", "list", "someone"])
        .env("MAPBOX_TILESETS_CLI", &stub)
        .env("MAPBOX_YES", "1")
        .env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .output()
        .expect("run the mapbox binary");

    assert!(
        !stderr(&out).contains("--yes"),
        "MAPBOX_YES was warned about: {}",
        stderr(&out)
    );
}

/// Written after the subcommand it is forwarded, and `tilesets` rejects an
/// option it has never heard of — the misplacement every global shares.
#[test]
fn yes_after_the_subcommand_is_called_out_as_misplaced() {
    let stub = stub_for("yes-misplaced");
    let out = run(&stub, &["tilesets-cli", "--yes", "list", "someone"]);

    let warning = stderr(&out);
    assert!(
        warning.contains("written after"),
        "expected the misplaced-global warning: {warning}"
    );
    // Forwarded verbatim, as `globals_written_after_the_subcommand_are_forwarded`
    // pins for every other global: the warning is advice, not interception.
    assert_eq!(args_seen(&out), ["--yes", "list", "someone"]);
}

/// `--debug` must not turn a forwarded token into a durable secret.
///
/// The unit tests cover every spelling; this covers the path, because the
/// redaction is only worth anything if the line the real binary prints goes
/// through it. Run with `MAPBOX_DEBUG=1` specifically: that spelling was a
/// usage error before this branch, so making it work is what widened the
/// exposure.
#[test]
fn a_forwarded_token_is_not_printed_by_debug() {
    const FAKE_TOKEN: &str = "sk.eyJ1IjoiZmFrZSIsImEiOiJmYWtlIn0.AAAAAAAAAAAAAAAAAAAAAA";

    let stub = stub_for("debug-redaction");
    let out = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["tilesets-cli", "--token", FAKE_TOKEN, "list", "someone"])
        .env("MAPBOX_TILESETS_CLI", &stub)
        .env("MAPBOX_DEBUG", "1")
        .env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_YES")
        .env("HOME", sandbox_home())
        .output()
        .expect("run the mapbox binary");

    // stderr only: the stub reports its own argv on stdout, token included,
    // which is the stub's whole job and not what is being asserted here.
    let printed = stderr(&out);
    assert!(
        printed.contains("[debug] exec"),
        "MAPBOX_DEBUG=1 did not turn debug output on: {printed}"
    );
    assert!(
        !printed.contains(FAKE_TOKEN),
        "the token reached the debug line: {printed}"
    );
    assert!(
        printed.contains("--token <redacted>"),
        "the flag should stay readable with only its value hidden: {printed}"
    );

    // Still forwarded to the child untouched — only the printing is redacted.
    assert_eq!(args_seen(&out), ["--token", FAKE_TOKEN, "list", "someone"]);
}
