//! End-to-end tests for the `--output` contract.
//!
//! The unit tests in `src/output.rs` cover mode resolution as a pure
//! function. What they cannot cover is the thing the contract is actually
//! about: which stream each kind of output lands on, in a real process, with
//! a real exit code. Cargo runs a test's child with pipes for stdout and
//! stderr, so every invocation here is a non-terminal one — which is exactly
//! the case `auto` is supposed to resolve to `json`, and the case an agent
//! calling this CLI will be in.
//!
//! Nothing here reaches the network, and nothing touches the developer's own
//! credentials: `HOME` is redirected into this target's temp dir, so the
//! `auth` commands and the credential lock files they create land there.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A home directory of this test binary's own.
///
/// The credential store is `$HOME/.mapbox`, and `--profile` alone is not
/// isolation: resolving credentials for *any* profile creates a lock file next
/// to the real ones, and an `auth` command run against a real path can delete
/// them. Redirecting `HOME` moves the whole store out of harm's way.
///
/// `HOME` alone does not, though — see [`command`].
fn sandbox_home() -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("output-contract-home");
    std::fs::create_dir_all(&home).expect("create sandbox home");
    home
}

/// Runs the real binary with the developer's own environment cleared.
///
/// `MAPBOX_ACCESS_TOKEN`, `MAPBOX_USERNAME` and `MAPBOX_OUTPUT` in the
/// running shell would each change what these assertions see — and a token
/// would turn a local failure into a live API call.
///
/// `MAPBOX_CONFIG_DIR` is what actually does the isolating, and `HOME` backs
/// it up. `config_dir()` reads the variable first and falls back to
/// `dirs::home_dir().join(".mapbox")` — and on Windows `dirs::home_dir()` asks
/// the OS for `FOLDERID_Profile` rather than reading `HOME`, so a `HOME` set
/// here would be ignored and these tests would resolve to the *real*
/// `%USERPROFILE%\.mapbox`. Naming the directory outright is the one form of
/// redirection that holds on every platform.
fn command() -> Command {
    let home = sandbox_home();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("MAPBOX_YES")
        .env("HOME", &home)
        .env("MAPBOX_CONFIG_DIR", home.join(".mapbox"));
    cmd
}

fn run(args: &[&str]) -> Output {
    command().args(args).output().expect("run mapbox")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// Parses one line of JSON, failing with the text itself when it is not JSON
/// — an assertion on a field is useless if the reason it failed is hidden.
fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("expected JSON, got {text:?} ({e})"))
}

/// A profile with no credentials file, so nothing reads or writes the
/// developer's real ones.
const UNUSED_PROFILE: [&str; 2] = ["--profile", "mapbox-cli-output-contract-test"];

/// A plain file sitting where the credential directory goes — the shape older
/// Mapbox tooling left behind at `~/.mapbox`.
///
/// `login` has to refuse *before* the browser, and this pins which refusal
/// comes first.
///
/// Originally the subject was the credential store: a plain file where
/// `~/.mapbox` goes, reported with the `mv` that fixes it, before an OAuth
/// round-trip that would have been wasted. The terminal check now runs ahead of
/// even that — `config_dir` *creates* the store, so a run that was never going
/// to finish must not get that far — which is what a test process can see, and
/// what this now asserts. The store's own message is covered by
/// `auth::tests::a_file_where_the_config_directory_belongs_says_so`, and the
/// ordering between the two by `yes_does_not_buy_a_login_a_terminal` in
/// `non_interactive.rs`.
#[test]
fn a_file_where_the_credential_directory_goes_fails_before_the_browser() {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("collision-home");
    std::fs::create_dir_all(&home).expect("create sandbox home");
    std::fs::write(home.join(".mapbox"), "pk.a-legacy-token").expect("write the legacy file");

    let out = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["-o", "text", "auth", "login"])
        .env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_CONFIG_DIR")
        .env("HOME", &home)
        .output()
        .expect("run mapbox");

    let text = stderr(&out);
    assert!(
        text.contains("no terminal"),
        "the terminal check has to be the first refusal: {text}"
    );
    assert!(
        !text.contains("is a file"),
        "the credential store was inspected by a run that could not finish: {text}"
    );
    // The setup wrote it as a file; a run that reached `config_dir` would have
    // failed on it, and a run that somehow got past would have replaced it.
    assert!(
        home.join(".mapbox").is_file(),
        "the credential store was touched by a run that could not finish"
    );
    assert!(!out.status.success());
    assert!(
        !stdout(&out).contains("Registering OAuth client"),
        "login reached the network before noticing it had nowhere to save: {}",
        stdout(&out)
    );
}

/// The same obstruction, seen from a command that is not about credentials.
///
/// It stays a warning rather than becoming an error: a job that authenticates
/// through `MAPBOX_ACCESS_TOKEN` works perfectly well despite the file, and
/// killing it over a directory it never needed would be wrong. But it is one
/// line — the full repair instructions on every invocation would bury the
/// output the user actually ran the command for. No network is reached here:
/// the missing path parameter is caught after credentials resolve.
///
/// Unix only, and it is the redirection that does not port rather than the
/// behaviour: this case has to leave `MAPBOX_CONFIG_DIR` unset so the default
/// `~/.mapbox` is what gets probed, and on Windows that default comes from
/// `FOLDERID_Profile` — `HOME` is not read, so there is no way to point the
/// default at a directory where a test may plant a file. Unlike the case
/// above, this command does resolve the store, so the redirection has to work
/// for the assertion to mean anything.
#[cfg(unix)]
#[test]
fn a_command_that_is_not_about_credentials_warns_in_one_line() {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("collision-home-warning");
    std::fs::create_dir_all(&home).expect("create sandbox home");
    std::fs::write(home.join(".mapbox"), "pk.a-legacy-token").expect("write the legacy file");

    let out = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .args(["-o", "text", "styles", "list"])
        .env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_CONFIG_DIR")
        .env("HOME", &home)
        .output()
        .expect("run mapbox");

    let warning = stderr(&out)
        .lines()
        .find(|line| line.starts_with("Warning: "))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("expected a warning, got {:?}", stderr(&out)));

    assert!(
        warning.contains("not a directory"),
        "the diagnosis has to be the directory, not the lock: {warning}"
    );
    assert!(
        !warning.contains("could not lock"),
        "locking is not what failed: {warning}"
    );
    assert!(
        warning.contains("mv "),
        "one line still has room for the fix itself: {warning}"
    );
}

#[test]
fn a_pipe_gets_json_without_being_asked() {
    let out = run(&["--profile", "nope", "styles", "list"]);

    let error = &json(&stderr(&out));
    assert_eq!(error["code"], "missing_path_parameters");
    assert!(!out.status.success());
}

#[test]
fn an_explicit_text_beats_the_pipe() {
    let out = run(&["-o", "text", "--profile", "nope", "styles", "list"]);

    let text = stderr(&out);
    assert!(
        text.starts_with("Error: Missing required path parameters"),
        "expected prose, got {text:?}"
    );
}

/// `MAPBOX_OUTPUT` covers a whole shell or CI job. No `-o` here on purpose:
/// a value typed on the command line outranks the environment, so passing
/// even `-o auto` would be testing clap's precedence rather than the
/// environment being read at all.
#[test]
fn the_environment_sets_the_mode_too() {
    let out = command()
        .args(["--profile", "nope", "styles", "list"])
        .env("MAPBOX_OUTPUT", "text")
        .output()
        .expect("run mapbox");

    assert!(
        stderr(&out).starts_with("Error: "),
        "MAPBOX_OUTPUT=text should have produced prose, got {:?}",
        stderr(&out)
    );
}

/// The other half of that precedence: a typed value beats the environment.
#[test]
fn the_command_line_outranks_the_environment() {
    let out = command()
        .args(["-o", "json", "--profile", "nope", "styles", "list"])
        .env("MAPBOX_OUTPUT", "text")
        .output()
        .expect("run mapbox");

    assert_eq!(json(&stderr(&out))["code"], "missing_path_parameters");
}

/// The whole point of keeping failures off stdout: a caller redirecting it
/// gets an empty file, not half a result it has to sniff.
#[test]
fn a_failure_writes_nothing_to_stdout() {
    for args in [
        &["--profile", "nope", "styles", "list"][..],
        &["accounts", "create-token"][..],
        &["--nonsense"][..],
    ] {
        let out = run(args);
        assert!(!out.status.success(), "{args:?} should have failed");
        assert_eq!(stdout(&out), "", "{args:?} wrote to stdout");
    }
}

#[test]
fn a_usage_error_is_json_on_a_pipe_and_keeps_claps_exit_code() {
    let out = run(&["geocoder", "forward-geocode", "--nonsense"]);

    let error = &json(&stderr(&out));
    assert_eq!(error["code"], "usage");
    assert!(
        error["message"]
            .as_str()
            .expect("a message")
            .contains("--nonsense"),
        "the message should name the offending argument: {error}"
    );
    // Clap distinguishes a usage error (2) from a runtime failure (1);
    // flattening them would cost a caller that distinction.
    assert_eq!(out.status.code(), Some(2));
}

/// `-o` written on a line clap rejects still has to be honoured — that is
/// the case `requested_in_argv` exists for.
#[test]
fn an_explicit_mode_survives_a_line_clap_could_not_parse() {
    let out = run(&["-o", "text", "geocoder", "forward-geocode", "--nonsense"]);

    let text = stderr(&out);
    assert!(
        text.starts_with("error:") && !text.starts_with('{'),
        "expected clap's own rendering, got {text:?}"
    );
}

/// Help is an error to clap but not a failure, and nobody wants their help
/// text as a JSON string.
#[test]
fn help_is_never_wrapped() {
    let out = run(&["--help"]);

    assert!(out.status.success());
    let text = stdout(&out);
    assert!(!text.starts_with('{'), "help was wrapped: {text:?}");
    assert!(text.contains("--output"), "--output should be documented");
}

/// An operation that can never succeed is not in the command surface, so it
/// answers exactly as a mistyped name does — same code, same shape, same
/// exit. Anything else would tell a caller which scopes exist while still
/// refusing to do the work.
#[test]
fn a_disabled_operation_is_indistinguishable_from_a_typo() {
    let disabled = run(&["accounts", "create-token"]);
    let typo = run(&["accounts", "create-tokn"]);

    assert_eq!(disabled.status.code(), typo.status.code());

    let d = json(&stderr(&disabled)).clone();
    let t = json(&stderr(&typo)).clone();
    assert_eq!(d["code"], "usage");
    assert_eq!(d["code"], t["code"]);
    // The names differ; nothing else may.
    assert_eq!(
        d.as_object().map(|o| o.len()),
        t.as_object().map(|o| o.len())
    );
    assert!(
        !stderr(&disabled).to_lowercase().contains("scope"),
        "the reply named a scope: {}",
        stderr(&disabled)
    );
}

#[test]
fn a_result_goes_to_stdout_in_both_shapes() {
    let piped = run(&[&UNUSED_PROFILE[..], &["auth", "logout"][..]].concat());
    assert!(piped.status.success());
    assert_eq!(json(&stdout(&piped))["logged_out"], false);
    assert_eq!(stderr(&piped), "", "a result should not touch stderr");

    let prose = run(&[&UNUSED_PROFILE[..], &["-o", "text", "auth", "logout"][..]].concat());
    assert!(prose.status.success());
    assert_eq!(stdout(&prose).trim(), "Not currently logged in.");
}

/// One JSON document per invocation, so a consumer can read a line and parse
/// it without accumulating.
#[test]
fn json_mode_emits_exactly_one_line() {
    let out = run(&[&UNUSED_PROFILE[..], &["-o", "json", "auth", "logout"][..]].concat());

    assert_eq!(stdout(&out).lines().count(), 1, "{:?}", stdout(&out));
}

/// The same promise, across the whole surface rather than one command.
///
/// `-o json` promises that everything on stdout is JSON. It does not promise
/// how many documents: a command that streams will emit one per line, and
/// that is a property of the command rather than of the flag. What must not
/// move is the other side of that: **a command that does not stream emits
/// exactly one document, before the first streaming command lands and after
/// it.**
///
/// Piped — which every test process is — `json` is compact, so "one document"
/// and "one line" are the same assertion, and it is exactly the property that
/// separates one document from JSON Lines. Checking the line count is
/// therefore stronger than parsing: `serde_json` would accept a pretty
/// document spread over forty lines, and a caller reading a line at a time
/// would not.
///
/// Every command here is one the suite can run with no network and no token.
/// That is most of what can be checked without a live account, and the
/// commands that are missing — the API operations — all leave through the
/// same `output::emit`.
#[test]
fn every_json_result_is_one_document() {
    let skills = sandbox_home().join("skills-out");
    let dir = skills.display().to_string();

    // (what it is, the arguments after `-o json`)
    let cases: [(&str, Vec<&str>); 7] = [
        ("a result", vec!["auth", "logout"]),
        (
            "a report",
            vec!["--token", TOKEN_FOR_SOMEONE, "auth", "whoami"],
        ),
        ("the whole schema", vec!["--schema"]),
        ("one command's schema", vec!["styles", "get", "--schema"]),
        (
            "a dry-run plan",
            vec![
                "--token",
                TOKEN_FOR_SOMEONE,
                "--username",
                "someone",
                "styles",
                "delete",
                "an-id",
                "--dry-run",
            ],
        ),
        (
            "a file listing",
            vec![
                "generate-skills",
                "--dry-run",
                "--service",
                "geocoder",
                "--dir",
                &dir,
            ],
        ),
        ("a local plan", vec!["uninstall", "--dry-run"]),
    ];

    for (what, args) in cases {
        let out = run(&[&UNUSED_PROFILE[..], &["-o", "json"][..], &args[..]].concat());
        let printed = stdout(&out);
        assert!(
            out.status.success(),
            "{what} ({args:?}) failed: {}",
            stderr(&out)
        );
        assert_eq!(
            printed.lines().count(),
            1,
            "{what} ({args:?}) put {} lines on stdout, and a caller reading one \
             line and parsing it would get a fragment:\n{printed}",
            printed.lines().count()
        );
        // And it really is JSON, not one long line of something else.
        let _ = json(&printed);
    }
}

/// A failure is one document too, on stderr, for the same reason: a caller
/// branching on `code` reads a line and parses it.
#[test]
fn a_json_failure_is_one_document_too() {
    let out = run(&["-o", "json", "styles"]);

    assert!(!out.status.success());
    assert_eq!(stdout(&out), "", "a failure wrote to stdout");
    let printed = stderr(&out);
    assert_eq!(
        printed.lines().count(),
        1,
        "the error envelope spans several lines:\n{printed}"
    );
    assert_eq!(json(&printed)["code"], "missing_subcommand");
}

/// A missing subcommand used to parse cleanly once any global was present,
/// so `mapbox -o json` — the exact line the contract tells an agent to use —
/// exited 0 with nothing on either stream.
#[test]
fn a_command_with_no_subcommand_fails_loudly() {
    for args in [&["-o", "json"][..], &["styles", "-o", "json"][..]] {
        let out = run(args);
        assert!(!out.status.success(), "{args:?} exited 0");
        assert_eq!(stdout(&out), "", "{args:?} wrote to stdout");
        assert_eq!(
            json(&stderr(&out))["code"],
            "missing_subcommand",
            "{args:?}"
        );
    }
}

/// Text mode used to fall through to clap's raw rendering for a missing
/// subcommand — the error paragraph, then a repeated usage line and a
/// `--help` hint that says nothing the message above it didn't. It should get
/// the same one-line message and `--help` suggestion `json` already does.
#[test]
fn a_missing_subcommand_gets_a_short_message_in_text_mode_too() {
    for args in [&["-o", "text"][..], &["styles", "-o", "text"][..]] {
        let out = run(args);
        assert!(!out.status.success(), "{args:?} exited 0");
        assert_eq!(stdout(&out), "", "{args:?} wrote to stdout");

        let text = stderr(&out);
        assert!(
            text.starts_with("Error: "),
            "{args:?} should get the short message, not clap's dump: {text:?}"
        );
        assert!(
            !text.contains("Usage:"),
            "{args:?} should not repeat clap's usage line: {text:?}"
        );
        assert!(
            text.contains("Next: mapbox"),
            "{args:?} should suggest --help: {text:?}"
        );
    }
}

/// A bare `mapbox styles` used to render clap's *whole help text* as the
/// error, whose first line is the service's own about — so the error
/// "message" once read as "Mapbox Styles API". It also used to depend on
/// whether `MAPBOX_TOKEN`/`MAPBOX_ACCESS_TOKEN` happened to be set: clap
/// counts an env-populated global arg as "present", so the exact same
/// command line showed full help with no token in the environment and this
/// error with one set. Neither variant is a result to report as one: it
/// should always be the same `missing_subcommand` error, regardless of the
/// environment.
#[test]
fn a_bare_service_always_errors_regardless_of_the_token_environment() {
    for token_env in [None, Some("x")] {
        let mut cmd = command();
        cmd.args(["styles", "-o", "json"]);
        if let Some(token) = token_env {
            // The var `--token` actually binds to (`command()` clears it by
            // default, so it must be set back here to exercise this case).
            cmd.env("MAPBOX_ACCESS_TOKEN", token);
        }
        let out = cmd.output().expect("run mapbox");

        assert!(!out.status.success(), "{token_env:?} exited 0");
        assert_eq!(stdout(&out), "", "{token_env:?} wrote to stdout");
        assert_eq!(
            json(&stderr(&out))["code"],
            "missing_subcommand",
            "{token_env:?}"
        );
    }
}

/// `export MAPBOX_OUTPUT=` is a common way to clear a variable. Under clap's
/// `.env()` it failed every command, including the ones needed to recover.
#[test]
fn an_unusable_environment_value_does_not_brick_the_cli() {
    for value in ["", "  ", "bogus"] {
        let out = command()
            .args(["--profile", "nope", "styles", "list"])
            .env("MAPBOX_OUTPUT", value)
            .output()
            .expect("run mapbox");

        // Still the command's own failure, not a usage error about --output.
        assert_eq!(
            json(stderr(&out).lines().last().unwrap_or_default())["code"],
            "missing_path_parameters",
            "MAPBOX_OUTPUT={value:?}"
        );
    }
}

/// Clap puts "the following required arguments were not provided:" on one
/// line and the arguments themselves on the lines after it. A message taken
/// from the first line alone names none of them.
#[test]
fn a_missing_argument_error_names_the_arguments() {
    let out = run(&["static-images", "get-static-image", "mapbox", "streets-v12"]);

    let message = json(&stderr(&out))["message"]
        .as_str()
        .expect("a message")
        .to_string();

    assert!(message.contains("<lon>"), "{message}");
    assert!(message.contains("<format>"), "{message}");
}

/// A live 401, carrying the advice a caller acts on: what to do, a command
/// to run, and both pages that bear on it — as fields, not buried in the
/// message, so nothing has to parse prose to use them.
///
/// The token here is typed with `--token`, which is the case the advice used
/// to get wrong: it read the environment rather than the resolved source,
/// blamed `MAPBOX_ACCESS_TOKEN` for a failure that flag caused, and answered
/// with `--use-login` — which re-sends the same typed token and produces
/// this identical error again. So what this pins is not just that a fix
/// exists but that it names the thing that actually failed.
///
/// This is the one test here that reaches the network: only the API can
/// produce a real 401, and `remedy`'s own tests cover the table it comes
/// from.
#[test]
fn an_unauthorized_response_carries_advice_a_caller_can_act_on() {
    let out = run(&[
        "--profile",
        "nope",
        "--token",
        "pk.bogus",
        "geocoder",
        "forward-geocode",
        "--q",
        "Helsinki",
    ]);

    let error = &json(&stderr(&out));
    assert_eq!(error["code"], "http_401");

    let fix = error["fix"].as_str().expect("a 401 should carry a fix");
    assert!(fix.contains("--token"), "{fix}");
    assert!(
        !fix.contains("--use-login"),
        "`--use-login` re-sends a typed token: following this would loop: {fix}"
    );

    // `whoami`, not `login`: a login cannot outrank the flag that failed, so
    // the only useful command is the one that says which token wins.
    assert_eq!(
        error["next_actions"]
            .as_array()
            .expect("a 401 should carry a command to run")
            .iter()
            .filter_map(|action| action.as_str())
            .collect::<Vec<_>>(),
        ["mapbox auth whoami"]
    );

    // The service's page and the tokens page: the failure is about the
    // credential, but the caller is still trying to geocode.
    let docs = error["docs"].as_array().expect("a 401 should carry pages");
    assert_eq!(
        docs.iter()
            .filter_map(|url| url.as_str())
            .collect::<Vec<_>>(),
        [
            "https://docs.mapbox.com/api/search/geocoding-v6/",
            "https://docs.mapbox.com/api/accounts/tokens/"
        ]
    );
}

/// The error contract end to end, in both renderings, without a network:
/// what went wrong, what to do about it, a command to run, and where it is
/// documented.
#[test]
fn a_failure_carries_a_fix_a_command_and_a_page() {
    let out = command()
        .args(UNUSED_PROFILE)
        .args(["auth", "whoami"])
        .output()
        .expect("run mapbox");

    let error = &json(&stderr(&out));
    assert_eq!(error["code"], "not_authenticated");
    assert!(error["fix"]
        .as_str()
        .expect("a fix")
        .contains("mapbox auth login"));
    assert_eq!(
        error["next_actions"]
            .as_array()
            .expect("a command to run")
            .iter()
            .filter_map(|action| action.as_str())
            .collect::<Vec<_>>(),
        ["mapbox auth login"]
    );
    assert_eq!(
        error["docs"]
            .as_array()
            .expect("a page")
            .iter()
            .filter_map(|url| url.as_str())
            .collect::<Vec<_>>(),
        ["https://docs.mapbox.com/api/accounts/tokens/"]
    );

    // The same three, labelled, for the person at a terminal.
    let text = command()
        .args(UNUSED_PROFILE)
        .args(["-o", "text", "auth", "whoami"])
        .output()
        .expect("run mapbox");
    let rendered = stderr(&text);
    assert!(rendered.contains("Fix: "), "{rendered}");
    assert!(rendered.contains("Next: mapbox auth login"), "{rendered}");
    assert!(
        rendered.contains("Docs: https://docs.mapbox.com/api/accounts/tokens/"),
        "{rendered}"
    );
}

/// A command that stopped short offers the help for the level that was
/// typed, not for the CLI as a whole — that is the level with something left
/// to show. The two argument shapes are the ones `missing_subcommand`
/// actually arrives from; see `a_command_with_no_subcommand_fails_loudly`.
#[test]
fn a_command_with_no_operation_offers_its_own_help() {
    // Clap names the program as it was invoked, and the suggestion follows
    // it: `mapbox.exe` on Windows. Writing `mapbox` here is what made this
    // test the one that caught that.
    let program = PathBuf::from(env!("CARGO_BIN_EXE_mapbox"))
        .file_name()
        .expect("the test binary has a name")
        .to_string_lossy()
        .into_owned();

    for (args, expected) in [
        (&["-o", "json"][..], format!("{program} --help")),
        (
            &["styles", "-o", "json"][..],
            format!("{program} styles --help"),
        ),
    ] {
        let out = run(args);
        let error = &json(&stderr(&out));

        assert_eq!(error["code"], "missing_subcommand", "{args:?}");
        assert_eq!(
            error["next_actions"]
                .as_array()
                .unwrap_or_else(|| panic!("{args:?} offered no command to run: {error}"))
                .iter()
                .filter_map(|action| action.as_str())
                .collect::<Vec<_>>(),
            [expected.as_str()],
            "{args:?}"
        );
    }
}

/// Nothing invented where there is nothing to say. A misspelled flag has no
/// command that answers it, and the fields are absent rather than empty: an
/// empty list invites a consumer to wonder whether it was computed.
#[test]
fn a_failure_with_no_advice_carries_no_empty_fields() {
    let out = run(&["styles", "list", "--nonsense"]);
    let error = &json(&stderr(&out));

    assert_eq!(error["code"], "usage");
    assert!(error.get("fix").is_none(), "{error}");
    assert!(error.get("next_actions").is_none(), "{error}");
    assert!(error.get("docs").is_none(), "{error}");
}

/// A Mapbox token whose payload decodes to an account and nothing else. The
/// signature is not one — nothing here checks it, and `--verify` is the only
/// thing that would, which is why no test in this file passes that flag.
const TOKEN_FOR_SOMEONE: &str = "pk.eyJ1Ijoic29tZW9uZSJ9.signature";

/// The wiring `auth`'s own unit tests cannot reach: the subcommand, its
/// alias, and the exit code.
///
/// The exit code is the part that only a real process can prove.
/// `mapbox auth whoami && ...` is the contract, and a status command that
/// exits 0 with nothing to report breaks it silently.
#[test]
fn whoami_reports_its_source_and_fails_when_there_is_nothing_to_report() {
    let bare = run(&[UNUSED_PROFILE[0], UNUSED_PROFILE[1], "auth", "whoami"]);
    assert!(!bare.status.success(), "nothing to report is a failure");
    assert_eq!(stdout(&bare), "", "a failure writes nothing to stdout");
    assert_eq!(json(&stderr(&bare))["code"], "not_authenticated");

    // The alias has to be the same command, not a second one that drifts.
    let aliased = run(&[UNUSED_PROFILE[0], UNUSED_PROFILE[1], "auth", "status"]);
    assert_eq!(json(&stderr(&aliased))["code"], "not_authenticated");

    // A token in the environment and no login is what a CI job looks like,
    // and naming that source is the whole reason this command exists.
    let out = command()
        .args(UNUSED_PROFILE)
        .args(["auth", "whoami"])
        .env("MAPBOX_ACCESS_TOKEN", TOKEN_FOR_SOMEONE)
        .output()
        .expect("run mapbox");
    assert!(out.status.success(), "{}", stderr(&out));

    let reported = json(&stdout(&out));
    assert_eq!(reported["account"], "someone");
    assert_eq!(reported["source"], "environment");
    assert_eq!(reported["env_var"], "MAPBOX_ACCESS_TOKEN");
    assert_eq!(reported["usage"], "pk");
    assert_eq!(
        reported["verified"],
        serde_json::Value::Null,
        "null is the shape of a check nobody asked for — and nothing here \
         reaches the network"
    );

    // `--token` outranks that environment, and the report has to say so
    // rather than reporting whichever it happened to read.
    let typed = command()
        .args(UNUSED_PROFILE)
        .args(["--token", TOKEN_FOR_SOMEONE, "auth", "whoami"])
        .env("MAPBOX_ACCESS_TOKEN", "pk.something-else.signature")
        .output()
        .expect("run mapbox");
    assert_eq!(json(&stdout(&typed))["source"], "flag");
}
