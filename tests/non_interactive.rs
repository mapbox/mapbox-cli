//! End-to-end tests for the non-interactive contract.
//!
//! The unit tests in `src/confirm.rs` cover the decision as a pure function,
//! which is the only way to cover it: a test process has pipes for stdin and
//! stderr, so it can never *be* the terminal case. What those tests cannot
//! reach is the half of the contract that lives in a real process — that
//! `mapbox auth login` refuses instead of waiting, that it refuses before
//! touching the filesystem, and that `--yes` and `MAPBOX_YES` are the two ways
//! to say otherwise.
//!
//! Nothing here reaches the network. `login` is stopped either by the terminal
//! check or, once past it, by a `MAPBOX_CONFIG_DIR` deliberately pointed at a
//! plain file — a failure that happens before the first HTTP call. That file is
//! also what makes "did `--yes` get past the gate?" answerable: the two
//! failures name different codes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch directory of this test binary's own, named per test so parallel
/// tests never collide.
fn scratch(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("non-interactive-{test}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Runs the real binary with the developer's own environment cleared.
///
/// `MAPBOX_YES` is on the list for the same reason `MAPBOX_ACCESS_TOKEN` is:
/// exported in the running shell, it is exactly the variable that would make
/// these assertions pass for the wrong reason.
fn command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("MAPBOX_YES")
        .env_remove("MAPBOX_CONFIG_DIR")
        .env("HOME", home);
    cmd
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The `code` field of a JSON-shaped failure, with the text itself in the
/// panic when it is not one — an assertion on a field is useless if the reason
/// it failed is hidden.
fn error_code(output: &Output) -> String {
    let text = stderr(output);
    let value: serde_json::Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stderr, got {text:?} ({e})"));
    value["code"]
        .as_str()
        .unwrap_or_else(|| panic!("no code in {text:?}"))
        .to_owned()
}

/// A `login` whose credential store is also unusable: `MAPBOX_CONFIG_DIR`
/// names a plain file, which `config_dir` rejects.
///
/// Used to show which of the two failures comes first. `config_dir` also
/// *creates* the store, so "the terminal check won" and "nothing was created"
/// are the same claim seen from two directions.
fn login_with_a_blocked_store(test: &str, extra: &[&str], env: &[(&str, &str)]) -> Output {
    let home = scratch(test);
    let blocked = home.join("not-a-directory");
    std::fs::write(&blocked, "").expect("write the blocking file");

    let mut cmd = command(&home);
    cmd.args(["-o", "json"])
        .args(extra)
        .args(["auth", "login"])
        .env("MAPBOX_CONFIG_DIR", &blocked);
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("run mapbox")
}

/// The whole point. Before this, `login` opened a browser that was not there
/// and then blocked in `accept()` until the CI runner killed the job.
#[test]
fn login_without_a_terminal_refuses_instead_of_waiting() {
    let home = scratch("refuses");
    let out = command(&home)
        .args(["-o", "text", "auth", "login"])
        .output()
        .expect("run mapbox");

    let text = stderr(&out);
    assert!(!out.status.success(), "the refusal has to be a failure");
    assert!(text.contains("no terminal"), "{text}");
    assert!(
        text.contains("MAPBOX_ACCESS_TOKEN"),
        "the refusal has to name what a CI job should use instead: {text}"
    );
    assert!(
        !text.contains("--yes"),
        "the tip must not offer a flag that cannot help — see \
         `yes_does_not_buy_a_login_a_terminal`: {text}"
    );
    assert!(
        stdout(&out).is_empty(),
        "a failure writes nothing to stdout: {:?}",
        stdout(&out)
    );
}

/// A code, not prose: this is the failure a script is most likely to branch
/// on, since it is the one that says "this environment cannot log in".
#[test]
fn the_refusal_carries_a_code_and_a_fix() {
    let home = scratch("code");
    let out = command(&home)
        .args(["-o", "json", "auth", "login"])
        .output()
        .expect("run mapbox");

    assert_eq!(error_code(&out), "interactive_required");
    let value: serde_json::Value =
        serde_json::from_str(stderr(&out).trim()).expect("JSON on stderr");
    assert!(
        value["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("MAPBOX_ACCESS_TOKEN")),
        "the fix is the actionable half, and the only action that helps here \
         is a token: {}",
        stderr(&out)
    );
    // Both ways out are changes to how the command is invoked, so there is no
    // command to offer — but the page that explains the token is still worth
    // naming. See `login_needs_a_terminal`.
    assert!(
        value.get("next_actions").is_none(),
        "neither way out of this is a command line: {}",
        stderr(&out)
    );
    assert!(
        value["docs"]
            .as_array()
            .is_some_and(|docs| !docs.is_empty()),
        "{}",
        stderr(&out)
    );
}

/// The check runs before `config_dir`, which creates the store as a side
/// effect. A CI job that tried to log in should leave nothing behind.
#[test]
fn the_refusal_creates_no_credential_directory() {
    let home = scratch("no-dir");
    let out = command(&home)
        .args(["auth", "login"])
        .output()
        .expect("run mapbox");

    assert!(!out.status.success());
    assert!(
        !home.join(".mapbox").exists(),
        "the refusal created the credential store anyway"
    );
}

/// One way of saying yes, as a case: a scratch-directory name, the arguments
/// to pass, and the environment to set.
type YesSpelling<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, &'a str)]);

/// `--yes` must NOT start a login that cannot finish.
///
/// It used to. The flag's documented purpose is to give a caller "the CI
/// behavior on purpose", so a job exports `MAPBOX_YES=1` to stop its deletes
/// blocking — and that silently opted `auth login` back into the browser flow:
/// past `config_dir`, into a real dynamic client registration, then five
/// minutes of `CALLBACK_TIMEOUT` waiting for a callback nobody would send. One
/// orphaned OAuth client per run, for a login that could never complete.
///
/// A flag about confirmations does not get to assert that a human is present,
/// so it no longer does. All three spellings are pinned, because the
/// environment one is what a CI config actually sets.
#[test]
fn yes_does_not_buy_a_login_a_terminal() {
    let cases: [YesSpelling; 3] = [
        ("yes-flag", &["--yes"], &[]),
        ("short-flag", &["-y"], &[]),
        ("yes-env", &[], &[("MAPBOX_YES", "1")]),
    ];

    for (test, extra, env) in cases {
        let out = login_with_a_blocked_store(test, extra, env);
        assert_eq!(
            error_code(&out),
            "interactive_required",
            "{extra:?} {env:?} let the login start"
        );
        // The blocked store is a second line of defense that should never be
        // reached: its message means `config_dir` already ran, and `config_dir`
        // creates the directory it checks.
        assert!(
            !stderr(&out).contains("is a file"),
            "{extra:?} {env:?} got as far as the credential store: {}",
            stderr(&out)
        );
    }
}

/// No value of `MAPBOX_YES` may turn a command into a usage error.
///
/// This is the whole reason the argument carries a `FalseyValueParser`.
/// `ArgAction::SetTrue`'s own parser accepts only `true` and `false`, so
/// `MAPBOX_YES=1` — the spelling a person reaches for and a CI config
/// generator emits — was `invalid value '1' for '--yes'` on *every* command,
/// including the ones needed to recover. `MAPBOX_OUTPUT` is read by hand for
/// the same reason, and `an_unusable_environment_value_does_not_brick_the_cli`
/// is this assertion for that variable.
///
/// The yes/no meaning of each value is not observable from here: with no
/// terminal nothing is asked either way. `confirm::decide` covers the decision
/// and the prompt was exercised by hand under a pty.
#[test]
fn no_value_of_the_environment_variable_can_brick_the_cli() {
    let home = scratch("env-values");

    for value in ["1", "0", "true", "false", "yes", "no", "", "maybe", "  "] {
        let out = command(&home)
            .args(["-o", "json", "styles", "delete"])
            .env("MAPBOX_YES", value)
            .output()
            .expect("run mapbox");

        // It still fails — the required argument is missing — but on the
        // argument, never on the variable.
        let code = error_code(&out);
        let text = stderr(&out);
        assert!(
            text.contains("style-id"),
            "MAPBOX_YES={value:?} failed on something other than the missing \
             argument: {text}"
        );
        assert!(
            !text.contains("invalid value"),
            "MAPBOX_YES={value:?} was rejected: {text}"
        );
        assert_eq!(code, "usage", "MAPBOX_YES={value:?}: {text}");
    }
}

/// It is global, so it parses in either position on a generated command — the
/// usage error names the missing argument rather than the flag.
#[test]
fn yes_parses_on_a_generated_command_in_either_position() {
    let home = scratch("parses");
    for args in [
        ["-o", "json", "--yes", "styles", "delete"],
        ["-o", "json", "styles", "delete", "--yes"],
    ] {
        let out = command(&home).args(args).output().expect("run mapbox");
        let text = stderr(&out);
        assert!(
            text.contains("style-id"),
            "{args:?} should fail on the missing argument, not the flag: {text}"
        );
        assert!(
            !text.contains("unexpected argument"),
            "{args:?} did not accept the flag: {text}"
        );
    }
}

/// The one test that actually reaches the confirmation.
///
/// Everything else here runs with pipes, which is the *non*-interactive half
/// of the contract. Without this, deleting the `confirm::destructive_request`
/// line from `executor::execute`, hardcoding its `assume_yes` to `true`, or
/// moving it below the query-string build all leave the whole suite green —
/// and the last of those is the token leak the call site's comment exists to
/// prevent. Checked: the suite passed with the gate removed entirely.
///
/// A pseudo-terminal is the only way in, since a test process has pipes on
/// every stream. `script` provides one without a pty crate, and the two
/// implementations disagree about argument order, so both are tried. The
/// child's stdout is redirected to a file inside the pty session: that keeps
/// the result stream separable from the prompt, and makes `--output auto`
/// resolve to `json` exactly as it would for a piped caller.
///
/// stdin is `/dev/null`, so the answer at the prompt is EOF — a no, and the
/// one answer that needs no timing. Feeding a `y` means holding the pipe open
/// past the read, which is a sleep in a test and a flake waiting to happen.
/// Runs one `mapbox` command under a pseudo-terminal and returns what the
/// session printed *and* what the command wrote to stdout, joined.
///
/// stdout is diverted to `out_path` so the two streams stay separable — that
/// is what makes `--output auto` resolve to `json`, as it would for a piped
/// caller — and both are searched, because the prompt is on stderr while a
/// dry run's plan is a result and goes to stdout.
///
/// `script` is the way to a pty without taking a crate on. The two
/// implementations disagree about argument order — BSD wants the command after
/// the typescript file, util-linux wants it as `-c` — so both are tried and
/// the first that actually ran wins, identified by `expect` appearing in the
/// session. stdin is `/dev/null`, so an unanswered question ends in EOF rather
/// than a hang.
#[cfg(unix)]
fn under_a_pty(home: &Path, out_path: &Path, args: &str, expect: &str) -> Option<String> {
    let exe = env!("CARGO_BIN_EXE_mapbox");
    let inner = format!("{exe} {args} > {}", out_path.display());
    let forms: [Vec<String>; 2] = [
        vec![
            "-q".into(),
            "/dev/null".into(),
            "sh".into(),
            "-c".into(),
            inner.clone(),
        ],
        vec!["-q".into(), "-c".into(), inner.clone(), "/dev/null".into()],
    ];

    forms.iter().find_map(|argv| {
        let _ = std::fs::remove_file(out_path);
        let output = Command::new("script")
            .args(argv)
            .env_remove("MAPBOX_ACCESS_TOKEN")
            .env_remove("MapboxAccessToken")
            .env_remove("MAPBOX_USERNAME")
            .env_remove("MAPBOX_OUTPUT")
            .env_remove("MAPBOX_YES")
            .env("HOME", home)
            .env("MAPBOX_CONFIG_DIR", home.join("config"))
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        // `script` relays the whole session on its own stdout; the command's
        // own stdout is in the file.
        let written = std::fs::read_to_string(out_path).unwrap_or_default();
        let seen = format!("{}{}{written}", stdout(&output), stderr(&output));
        seen.contains(expect).then_some(seen)
    })
}

/// A dry run must not ask. It sends nothing, so the question would be about
/// something that is not going to happen — and it would make the flag that
/// exists to be safe the one that blocks a script.
///
/// Pinned because the two features meet in one line of ordering:
/// `confirm::destructive_request` sits below the dry-run early return in
/// `executor::execute`, and nothing but this test notices if it moves.
#[cfg(unix)]
#[test]
fn a_dry_run_at_a_terminal_does_not_ask() {
    let home = scratch("pty-dry-run");
    let out_path = home.join("stdout");

    let seen = under_a_pty(
        &home,
        &out_path,
        "--token pk.not-a-real-token --username someone \
         styles delete a-style-id --dry-run",
        // The plan is a result, so it lands on the diverted stdout — and with
        // stdout a file, `--output auto` resolves to `json`, exactly as it
        // would for a piped caller. So this is the JSON key, not the prose.
        "\"dry_run\":true",
    )
    .expect("neither `script` form produced the dry-run plan");

    assert!(
        !seen.contains("Continue?"),
        "a dry run asked for confirmation: {seen}"
    );
    assert!(
        !seen.contains("not-a-real-token"),
        "the plan printed the token: {seen}"
    );
    assert!(
        seen.contains("\"access_token\":\"<redacted>\""),
        "the plan should show the token redacted rather than absent: {seen}"
    );
}

#[cfg(unix)]
#[test]
fn a_delete_at_a_terminal_asks_before_it_sends() {
    let home = scratch("pty-prompt");
    let out_path = home.join("stdout");

    // A token is required to get as far as the request, and never sent: the
    // prompt is answered no. Deliberately not a real token's shape.
    let args = "--token pk.not-a-real-token --username someone \
                styles delete a-style-id";

    let seen = under_a_pty(&home, &out_path, args, "About to DELETE")
        .expect("neither `script` form produced the prompt");

    // The question names the request, and the access token is a query
    // parameter, so this string is the reason `destructive_request` is called
    // before the query string is built.
    assert!(
        seen.contains("About to DELETE https://api.mapbox.com/styles/v1/someone/a-style-id"),
        "the prompt must name the resource: {seen}"
    );
    assert!(
        !seen.contains("access_token"),
        "the prompt leaked the token query parameter: {seen}"
    );
    assert!(
        !seen.contains("not-a-real-token"),
        "the prompt leaked the token value: {seen}"
    );

    // EOF is a no, and a no is a failure with a code a script can branch on.
    assert!(
        seen.contains("\"code\":\"cancelled\""),
        "EOF at the prompt must cancel, in the shape the caller asked for: {seen}"
    );

    // And nothing was sent: a canceled delete writes no result at all. Read
    // from the file rather than `seen`, which has the two streams joined.
    let written = std::fs::read_to_string(&out_path).unwrap_or_default();
    assert!(
        written.trim().is_empty(),
        "a canceled delete wrote to stdout: {written:?}"
    );
}
