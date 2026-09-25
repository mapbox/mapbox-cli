//! End-to-end tests for `--schema`.
//!
//! The unit tests in `src/schema.rs` compare the schema against the command
//! tree in the same process, which is where drift between the two would show
//! up. What they cannot show is the part a caller actually depends on: that
//! naming a command is enough to describe it — no token, no arguments, no
//! request — and that the answer arrives on stdout as one JSON document with
//! an exit code of zero.
//!
//! Nothing here reaches the network. That is not incidental: a schema
//! request that needed a token would be useless to the agent it exists for,
//! and these tests run with none.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A config directory of this test binary's own. See the same function in
/// `output_contract.rs` for why `--profile` alone is not isolation.
fn sandbox_home() -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("schema-contract-home");
    std::fs::create_dir_all(&home).expect("create sandbox home");
    home
}

/// Runs the real binary with the developer's own environment cleared — a
/// token in the running shell would let a mistake here reach the API.
///
/// `MAPBOX_CONFIG_DIR` is the isolation that holds everywhere; `HOME` is only
/// a backstop. See the same function in `output_contract.rs` for why the
/// variable is needed on Windows, where `HOME` is not what the home directory
/// is read from.
fn command() -> Command {
    let home = sandbox_home();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
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

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("expected JSON, got {text:?} ({e})"))
}

/// The schema of a successful `--schema` run, with the invariants every one
/// of them shares already checked.
fn schema(args: &[&str]) -> serde_json::Value {
    let out = run(args);
    assert!(out.status.success(), "{args:?} failed: {}", stderr(&out));
    assert_eq!(stderr(&out), "", "{args:?} wrote to stderr");

    let value = json(&stdout(&out));
    assert_eq!(value["schema_version"], 1);
    value
}

fn commands(value: &serde_json::Value) -> &Vec<serde_json::Value> {
    value["commands"].as_array().expect("commands is an array")
}

fn names(value: &serde_json::Value) -> Vec<String> {
    commands(value)
        .iter()
        .map(|command| {
            command["command"]
                .as_str()
                .expect("a command name")
                .to_string()
        })
        .collect()
}

#[test]
fn a_command_describes_itself_without_being_able_to_run() {
    // Neither the style id this command requires nor a token to fetch it
    // with. Running it is impossible; describing it is the whole point.
    let value = schema(&["styles", "get", "--schema"]);

    assert_eq!(value["target"], "mapbox styles get");
    assert_eq!(names(&value), ["mapbox styles get"]);

    let command = &commands(&value)[0];
    assert_eq!(command["kind"], "api");
    assert_eq!(command["request"]["method"], "GET");
    assert_eq!(
        command["request"]["url"],
        "https://api.mapbox.com/styles/v1/{username}/{style_id}"
    );

    let style_id = command["arguments"]
        .as_array()
        .expect("arguments")
        .iter()
        .find(|arg| arg["name"] == "style-id")
        .expect("the style id is described");
    assert_eq!(style_id["kind"], "positional");
    assert_eq!(style_id["required"], true);
    assert_eq!(style_id["location"], "path");
}

#[test]
fn the_account_the_url_needs_is_listed_as_an_argument() {
    // `{username}` has no parameter of its own — the executor fills it from
    // the global flag. A schema that showed the placeholder and nothing that
    // fills it would describe an unbuildable request.
    let value = schema(&["accounts", "list-tokens", "--schema"]);

    let username = commands(&value)[0]["arguments"]
        .as_array()
        .expect("arguments")
        .iter()
        .find(|arg| arg["name"] == "username")
        .expect("the account is described");

    assert_eq!(username["flag"], "--username");
    assert_eq!(username["source"], "global");
    assert_eq!(username["required"], true);
}

#[test]
fn the_root_describes_the_whole_surface() {
    let value = schema(&["--schema"]);
    let listed = names(&value);

    assert_eq!(value["target"], "mapbox");
    for expected in [
        "mapbox styles list",
        "mapbox geocoder forward",
        "mapbox auth login",
        "mapbox tilesets-cli",
    ] {
        assert!(
            listed.contains(&expected.to_string()),
            "{expected} is missing"
        );
    }

    let kinds: Vec<&str> = commands(&value)
        .iter()
        .filter_map(|command| command["kind"].as_str())
        .collect();
    for kind in ["api", "builtin", "passthrough"] {
        assert!(kinds.contains(&kind), "no command of kind {kind}");
    }
}

#[test]
fn a_service_describes_only_its_own_commands() {
    let value = schema(&["styles", "--schema"]);

    assert_eq!(value["target"], "mapbox styles");
    assert!(commands(&value)
        .iter()
        .all(|command| command["service"] == "styles"));
    assert!(names(&value).contains(&"mapbox styles list".to_string()));
}

#[test]
fn every_answer_carries_the_globals() {
    // An agent holding one command still has to be told what goes in front
    // of it.
    let value = schema(&["styles", "list", "--schema"]);
    let globals = value["global_options"].as_array().expect("global_options");

    let token = globals
        .iter()
        .find(|arg| arg["flag"] == "--token")
        .expect("--token is a global");
    assert_eq!(token["env"], "MAPBOX_ACCESS_TOKEN");

    assert!(value["authentication"]
        .as_str()
        .is_some_and(|text| text.contains("mapbox auth login")));
}

/// The other half of the same promise, for the arguments that take nothing.
///
/// `--yes` and `--debug` declare `.env()` with `FalseyValueParser`, so that
/// `MAPBOX_YES=1` means yes rather than being a usage error. clap reports that
/// parser's twelve spellings as the argument's possible values, and publishing
/// them told an agent `--yes` accepts `1`, `on` or `false` — none of which it
/// does. `mapbox --yes=1` is a usage error, which is exactly the gap between
/// promise and behavior this file exists to close.
#[test]
fn a_flag_promises_no_values_because_it_takes_none() {
    let value = schema(&["styles", "delete", "--schema"]);
    let globals = value["global_options"].as_array().expect("global_options");

    let flags: Vec<&serde_json::Value> =
        globals.iter().filter(|arg| arg["kind"] == "flag").collect();
    assert!(
        flags.len() >= 4,
        "expected the boolean globals to be described as flags, got {}",
        flags.len()
    );

    for flag in flags {
        let values = flag["values"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        assert!(
            values.is_empty(),
            "{} takes no value, so it must promise none — got {:?}",
            flag["flag"],
            values
        );
    }

    // And the promise holds the other way: the value the parser would have
    // advertised is refused on the command line.
    //
    // Asserted on the payload, not on the exit code. `--yes=1 styles
    // delete x` would fail for a second reason anyway — the sandboxed
    // environment leaves `{username}` unresolved — so a non-zero exit would
    // pass this even if `--yes` started taking a value.
    let refused = run(&["-o", "json", "--yes=1", "styles", "delete", "x"]);
    let message = json(&stderr(&refused))["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("unexpected value '1' for '--yes'"),
        "--yes=1 was not refused as a value: {message}"
    );
}

#[test]
fn the_values_the_schema_promises_are_the_values_the_cli_takes() {
    // `tilesize`'s enum is written as integers in the spec, which the reader
    // used to drop: the schema said nothing about the allowed values and the
    // CLI accepted any number. Both halves are asserted together, because
    // either one alone can regress into a description that is not true of the
    // command it describes.
    let value = schema(&["static", "get-tile", "--schema"]);
    let tilesize = commands(&value)[0]["arguments"]
        .as_array()
        .expect("arguments")
        .iter()
        .find(|arg| arg["name"] == "tilesize")
        .expect("tilesize is described");
    assert_eq!(tilesize["values"][0], "256");
    assert_eq!(tilesize["values"][1], "512");

    let refused = run(&[
        "--username",
        "u",
        "static",
        "get-tile",
        "some-style",
        "300",
        "1",
        "2",
        "3",
        "",
        ".png",
    ]);
    assert!(!refused.status.success(), "300 was accepted");
    assert_eq!(stdout(&refused), "");
    let message = json(&stderr(&refused))["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(message.contains("256, 512"), "{message}");
}

#[test]
fn a_pipe_gets_one_line() {
    let out = run(&["--schema"]);
    assert_eq!(stdout(&out).trim().lines().count(), 1);
}

#[test]
fn text_output_is_still_json() {
    // There is no prose rendering of a schema worth having, so `text` only
    // decides that a person is reading and the document should be indented.
    let out = run(&["-o", "text", "styles", "list", "--schema"]);
    assert!(out.status.success());

    let text = stdout(&out);
    assert!(text.lines().count() > 1, "text mode should indent");
    assert_eq!(json(&text)["target"], "mapbox styles list");
}

#[test]
fn an_operation_that_is_not_a_command_cannot_be_described() {
    // `--schema` describes the command surface, and a withheld operation is
    // not on it. Asking about one has to answer exactly as a typo does, or
    // the flag becomes a way to enumerate what the CLI declines to offer.
    let out = run(&["styles", "set-style-protected", "--schema"]);

    assert!(!out.status.success());
    assert_eq!(stdout(&out), "");
    assert_eq!(json(&stderr(&out))["code"], "usage");
}

#[test]
fn a_schema_written_where_a_value_goes_is_not_a_request() {
    // `-o` takes a value, so this line asks for an output format called
    // `--schema`. Clap decides that, which is the reason the flag is not
    // detected by scanning argv: a scan would see the word and answer.
    let out = run(&["-o", "--schema", "styles", "list"]);

    assert!(!out.status.success());
    assert_eq!(stdout(&out), "");
}

#[test]
fn help_and_version_still_win() {
    // Both reach the same fallback `--schema` does — a line clap refused to
    // parse, for a reason that is not a complaint. Only one kind of refusal
    // is a schema request.
    let help = run(&["--help"]);
    assert!(help.status.success());
    assert!(stdout(&help).contains("Usage: mapbox"));

    let version = run(&["--version"]);
    assert!(version.status.success());
    assert!(stdout(&version).starts_with("mapbox "));
}

#[test]
fn the_hand_written_commands_describe_themselves() {
    let value = schema(&["auth", "--schema"]);

    assert_eq!(value["target"], "mapbox auth");
    assert_eq!(
        names(&value),
        [
            "mapbox auth login",
            "mapbox auth logout",
            "mapbox auth refresh",
            "mapbox auth whoami",
            "mapbox auth profiles"
        ]
    );
    assert!(commands(&value)
        .iter()
        .all(|command| command["kind"] == "builtin"));
    assert!(commands(&value)
        .iter()
        .all(|command| command["request"].is_null()));

    let one = schema(&["auth", "login", "--schema"]);
    assert_eq!(names(&one), ["mapbox auth login"]);

    // A hand-written command that declares a flag of its own. `auth whoami
    // --verify` is the first, so until it existed `own_arguments` was only
    // ever exercised against builtins that declare nothing at all — and an
    // empty list is what a filter that drops too much also returns. The
    // globals still belong to `global_options`, so there is exactly one.
    let whoami = schema(&["auth", "whoami", "--schema"]);
    let arguments = commands(&whoami)[0]["arguments"]
        .as_array()
        .expect("a builtin's own arguments");
    assert_eq!(arguments.len(), 1, "{arguments:?}");
    assert_eq!(arguments[0]["flag"], "--verify");
    assert_eq!(arguments[0]["type"], "boolean");
}

#[test]
fn a_token_never_reaches_the_output() {
    // The one invariant with a bad failure mode. Every way a credential can
    // arrive is present at once, including the environment variables the
    // harness normally strips, and none of them may appear in the answer —
    // the schema names the variables, never their values.
    let out = command()
        .env("MAPBOX_ACCESS_TOKEN", "sk.SENTINEL_ENV_TOKEN")
        .env("MapboxAccessToken", "pk.SENTINEL_ALT_TOKEN")
        .env("MAPBOX_USERNAME", "sentinel-env-user")
        .args([
            "--token",
            "sk.SENTINEL_TYPED_TOKEN",
            "--username",
            "sentinel-typed-user",
            "styles",
            "list",
            "--schema",
        ])
        .output()
        .expect("run mapbox");

    assert!(out.status.success(), "{}", stderr(&out));
    let text = format!("{}{}", stdout(&out), stderr(&out));
    for sentinel in [
        "SENTINEL_ENV_TOKEN",
        "SENTINEL_ALT_TOKEN",
        "sentinel-env-user",
        "SENTINEL_TYPED_TOKEN",
        "sentinel-typed-user",
    ] {
        assert!(!text.contains(sentinel), "{sentinel} reached the output");
    }
    // The variable's name is what belongs there, and still does.
    assert!(stdout(&out).contains("MAPBOX_ACCESS_TOKEN"));
}

#[test]
fn nothing_about_a_credential_is_resolved() {
    // `auth::validate_profile` rejects this name, and it is the first thing
    // `run` does. A schema request that succeeds with it has demonstrably
    // not reached the code that resolves, refreshes or locks credentials.
    let out = run(&[
        "--profile",
        "../not-a-profile",
        "styles",
        "list",
        "--schema",
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(json(&stdout(&out))["target"], "mapbox styles list");

    // The same line without `--schema` is refused, which is what makes the
    // assertion above mean anything.
    let refused = run(&["--profile", "../not-a-profile", "styles", "list"]);
    assert!(!refused.status.success());
}

/// Unix-only, because this is the one case here that actually spawns the
/// child: proving the flag was forwarded means reading it back out of
/// something that echoes its argv, and `/bin/echo` has no Windows
/// counterpart — `echo` there is a `cmd` builtin, not an executable
/// `MAPBOX_TILESETS_CLI` could name. Same reason `tilesets_cli_proxy.rs` is
/// `#![cfg(unix)]` whole. The proxy is a no-op on Windows anyway: it forwards
/// to a macOS/Linux Python package.
#[cfg(unix)]
#[test]
fn the_passthrough_keeps_a_flag_written_after_its_name() {
    // Everything after `tilesets-cli` belongs to the child, `--schema`
    // included — that is what the schema's own `note` promises. The child
    // here is `echo`, so the test never depends on `tilesets` being
    // installed.
    let out = command()
        .env("MAPBOX_TILESETS_CLI", "/bin/echo")
        .args(["tilesets-cli", "--schema"])
        .output()
        .expect("run mapbox");

    assert!(
        !stdout(&out).contains("schema_version"),
        "the flag was intercepted"
    );
    assert!(
        stdout(&out).contains("--schema"),
        "the flag was not forwarded"
    );
    assert!(
        stderr(&out).contains("Put it before the subcommand"),
        "no hint about where it belongs: {}",
        stderr(&out)
    );
}

#[test]
fn the_passthrough_is_described_when_the_flag_comes_first() {
    // The spelling the note tells callers to use. The child must not run.
    let out = command()
        .env("MAPBOX_TILESETS_CLI", "/bin/echo")
        .args(["--schema", "tilesets-cli"])
        .output()
        .expect("run mapbox");

    assert!(out.status.success(), "{}", stderr(&out));
    let value = json(&stdout(&out));
    assert_eq!(value["target"], "mapbox tilesets-cli");

    let command = &commands(&value)[0];
    assert_eq!(command["kind"], "passthrough");
    assert_eq!(command["forwards_to"], "tilesets");
}

/// Commands whose deprecation has already been looked at: the spec marks
/// the endpoint deprecated, and the decision was to keep exposing it with a
/// warning.
///
/// Empty today, and it is the place the triage decision goes — the same
/// shape `WITHHELD_OPERATIONS` uses for the other answer, which is to stop
/// exposing the command at all. The reasoning belongs in a comment on the
/// entry: an entry here is the record, so a bare string is a decision nobody
/// can check later.
const TRIAGED_DEPRECATIONS: &[&str] = &[];

/// `deprecated` is absent rather than `false` on everything currently
/// shipped, which is what lets a caller read its presence as the whole
/// answer.
///
/// The assertion doubles as the tripwire for an upstream deprecation.
/// A maintainer-only drift check compares whole specs against
/// `MAPBOX_SPEC_ENTRIES` and never looks inside them, so a `deprecated: true`
/// landing on an operation that
/// is already wired is caught by nothing else. When this fails, the sync
/// deprecated something, and there are two answers: withhold the operation
/// in `src/spec.rs`, or add it to `TRIAGED_DEPRECATIONS` above to record
/// that it stays. If it stays, say so in `docs/commands.md` as well — that
/// page is what a user reads, and nothing generates it.
#[test]
fn every_deprecation_in_the_surface_has_been_triaged() {
    let value = schema(&["--schema"]);

    let untriaged: Vec<&str> = commands(&value)
        .iter()
        .filter(|command| command["deprecated"] != serde_json::Value::Null)
        .map(|command| command["command"].as_str().unwrap_or("?"))
        .filter(|command| {
            !TRIAGED_DEPRECATIONS
                .iter()
                .any(|triaged| command == &format!("mapbox {triaged}"))
        })
        .collect();

    assert!(
        untriaged.is_empty(),
        "newly deprecated, needs a triage decision — withhold it, or add it \
         to TRIAGED_DEPRECATIONS: {untriaged:?}"
    );
}
