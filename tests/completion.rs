//! End-to-end tests for `mapbox completion <shell>`.
//!
//! The property worth testing is not that `clap_complete` works — that is
//! upstream's suite — but that what this CLI hands it is the tree it actually
//! ships. So every check here is about the *relationship* between the printed
//! script and the binary that printed it: a service the specs added is in it,
//! an operation `Operation::is_exposed` withholds is not, and a hand-written
//! command is treated like any other.
//!
//! What no `cargo test` can reach is whether a shell accepts the script it
//! gets. `scripts/test-completion.sh` does that half — it parses and sources
//! each script in the real shell, and drives an actual completion in the two
//! that can be driven with no terminal (bash and fish). The two halves are
//! deliberately split rather than merged behind a "shell is installed" guard:
//! a check that skips itself on a machine without fish is not the check that
//! should be deciding whether `cargo test` passes.
//!
//! Nothing here reaches the network, and nothing needs a token — a shell
//! asking what this CLI can complete has not logged in.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Every shell the command claims. The list is duplicated from
/// `src/completion.rs` on purpose: a test that read `SHELLS` would pass by
/// following whatever that list became, and
/// `the_claimed_shells_are_the_ones_that_generate` is the check that the two
/// agree.
const SHELLS: &[&str] = &["bash", "zsh", "fish", "powershell"];

/// A config directory of this test binary's own. See the same function in
/// `output_contract.rs` for why `--profile` alone is not isolation.
fn sandbox_home() -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("completion-home");
    std::fs::create_dir_all(&home).expect("create sandbox home");
    home
}

/// The real binary, with the developer's own environment cleared for the
/// reason `schema_contract.rs` clears it.
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

/// One shell's script, with the invariants every one of them shares already
/// checked.
fn script(shell: &str) -> String {
    let out = run(&["completion", shell]);
    assert!(
        out.status.success(),
        "`mapbox completion {shell}` failed: {}",
        stderr(&out)
    );
    assert_eq!(
        stderr(&out),
        "",
        "`mapbox completion {shell}` wrote to stderr"
    );
    let script = stdout(&out);
    assert!(
        !script.trim().is_empty(),
        "`mapbox completion {shell}` printed nothing"
    );
    script
}

/// The whole surface, as the binary reports it.
fn schema() -> serde_json::Value {
    let out = run(&["--schema"]);
    assert!(out.status.success(), "--schema failed: {}", stderr(&out));
    serde_json::from_slice(&out.stdout).expect("`mapbox --schema` prints one JSON document")
}

/// Every command the CLI ships, as `["styles", "get"]` — the words a
/// completion script has to know about.
fn command_paths() -> Vec<Vec<String>> {
    schema()["commands"]
        .as_array()
        .expect("commands is an array")
        .iter()
        .map(|command| {
            command["command"]
                .as_str()
                .expect("a command name")
                .split_whitespace()
                .skip(1) // "mapbox"
                .map(str::to_string)
                .collect()
        })
        .collect()
}

#[test]
fn every_claimed_shell_gets_a_script_on_stdout() {
    for shell in SHELLS {
        let script = script(shell);
        assert!(
            script.contains("mapbox"),
            "the {shell} script never names the binary"
        );
    }
}

/// The list clap accepts and the list this file checks have to be the same
/// list — a shell added to `SHELLS` in `src/completion.rs` and not here would
/// ship unverified, which is the whole reason that list is hand-written.
#[test]
fn the_claimed_shells_are_the_ones_that_generate() {
    let out = run(&["completion", "--help"]);
    let help = stdout(&out);
    let expected = format!("[possible values: {}]", SHELLS.join(", "));
    assert!(
        help.contains(&expected),
        "`completion --help` does not offer exactly {SHELLS:?}:\n{help}"
    );

    // And nothing else is accepted, so the two lists cannot differ in the
    // other direction either.
    let out = run(&["completion", "elvish"]);
    assert!(
        !out.status.success(),
        "`completion elvish` succeeded, so a shell nothing here checks is being claimed"
    );
}

/// The point of generating rather than maintaining: every service and every
/// operation the binary has is in the script, including the ones a spec sync
/// added after this test was written.
#[test]
fn every_command_the_cli_ships_is_completable_in_every_shell() {
    let paths = command_paths();
    // A floor, well under the real number. The specs vendored in `openapi/`
    // are a deliberate subset of what upstream describes — all of `sources`
    // and several `styles`/`fonts`/`maps` operations never reach them — so
    // the true count is 47 today. 40 leaves that room to move while still
    // catching the surface collapsing for real.
    assert!(
        paths.len() > 40,
        "only {} commands came back from --schema; the surface cannot be that small",
        paths.len()
    );

    for shell in SHELLS {
        let script = script(shell);
        let missing: Vec<String> = paths
            .iter()
            .flatten()
            // `<args…>` is the tilesets proxy's forwarded argv, not a word
            // anything completes.
            .filter(|word| !word.starts_with('<'))
            .filter(|word| !script.contains(word.as_str()))
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "the {shell} script does not mention these commands: {missing:?}"
        );
    }
}

/// The other direction: a build's script describes that build. `setStyleProtected` is withheld by
/// `WITHHELD_OPERATIONS` — it unlocks a style for deletion — so `mapbox
/// styles set-style-protected` answers exactly as a typo does, and a
/// completion script offering it would send someone to a command that does
/// not exist.
#[test]
fn a_withheld_operation_is_absent_from_every_shell() {
    let withheld = [
        "set-style-protected",
        "admin-get-style",
        "admin-update-style",
    ];

    // First: these really are withheld, so the test cannot pass by checking
    // for words the CLI never had. If one is exposed later, this fails here
    // rather than silently checking nothing.
    let out = run(&["styles", "set-style-protected", "--schema"]);
    assert!(
        !out.status.success(),
        "`styles set-style-protected` is a command now — pick another withheld operation"
    );

    for shell in SHELLS {
        let script = script(shell);
        for name in withheld {
            assert!(
                !script.contains(name),
                "the {shell} script offers `{name}`, which this build withholds"
            );
        }
    }
}

/// How one long flag is written in one shell's script.
///
/// bash, zsh and PowerShell all put the flag as typed; fish's `complete`
/// takes it as `-l token`, with the dashes supplied by fish itself. Searching
/// for the bare word instead would match any description mentioning it, which
/// on this CLI is most of them.
fn flag_in(shell: &str, flag: &str) -> String {
    match shell {
        "fish" => format!("-l {}", flag.trim_start_matches('-')),
        _ => flag.to_string(),
    }
}

/// `completion` is a command like any other, so it completes itself — and in
/// the two shells whose generator emits a positional's possible values, the
/// shells it accepts come with it.
///
/// fish and PowerShell get the command but not those values: `clap_complete`
/// 4.6 renders `PossibleValuesParser` for options in every shell and for
/// positionals only in bash and zsh. That is upstream's gap, not a choice
/// here, and it is written down so a later version filling it reads as an
/// improvement rather than a surprise.
#[test]
fn the_script_completes_the_command_that_printed_it() {
    for shell in SHELLS {
        let script = script(shell);
        assert!(
            script.contains("completion"),
            "the {shell} script does not complete `mapbox completion`"
        );
    }

    for shell in ["bash", "zsh"] {
        let script = script(shell);
        for name in SHELLS {
            assert!(
                script.contains(name),
                "the {shell} script does not offer `{name}` as a value of `completion`"
            );
        }
    }
}

/// A global is completed on every command, which is what `.global(true)`
/// means and what a caller typing `mapbox styles list --to<tab>`
/// expects.
#[test]
fn the_globals_are_completable_too() {
    for shell in SHELLS {
        let script = script(shell);
        for flag in ["--token", "--profile", "--output", "--schema", "--dry-run"] {
            let spelling = flag_in(shell, flag);
            assert!(
                script.contains(&spelling),
                "the {shell} script never offers `{flag}` (looked for `{spelling}`)"
            );
        }
    }
}

/// The script is the result, so it leaves by itself on stdout — no envelope,
/// no progress line, nothing that would have to be stripped before the shell
/// could read it.
#[test]
fn json_mode_does_not_wrap_the_script() {
    let out = command()
        .args(["-o", "json", "completion", "bash"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());

    let script = stdout(&out);
    assert!(
        script.starts_with("_mapbox()"),
        "`-o json` changed the script itself: {}",
        &script[..script.len().min(80)]
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&script).is_err(),
        "`-o json` wrapped the script in an envelope, which no shell can source"
    );

    // Warned about, once, on stderr — where a warning cannot corrupt the
    // thing being redirected.
    let warning = stderr(&out);
    assert!(
        warning.contains("--output") && warning.contains("completion"),
        "`-o json` was ignored silently: {warning:?}"
    );
}

/// The ordinary invocation is a redirect, which `auto` resolves to `json`.
/// That must change nothing, and must not warn: `mapbox completion bash >
/// mapbox.bash` is the line in every install instruction there is.
#[test]
fn a_redirect_is_neither_wrapped_nor_warned_about() {
    let out = run(&["completion", "bash"]);
    assert!(out.status.success());
    assert!(stdout(&out).starts_with("_mapbox()"));
    assert_eq!(
        stderr(&out),
        "",
        "the ordinary redirect case warns, so every install instruction prints a warning"
    );
}

/// A shell this CLI does not generate for is refused here, locally, with the
/// four it does named — never by printing a script for something else.
///
/// The two cases word themselves differently and both are checked: an unknown
/// value can list the alternatives, a missing argument names the placeholder,
/// and clap writes each. Nothing reaches stdout either way, so a redirect
/// that failed leaves an empty file rather than half a script.
#[test]
fn a_missing_or_unknown_shell_is_a_usage_error() {
    let out = run(&["completion", "nushell"]);
    assert!(!out.status.success(), "`completion nushell` succeeded");
    assert_eq!(stdout(&out), "", "`completion nushell` wrote to stdout");
    let message = stderr(&out);
    for shell in SHELLS {
        assert!(
            message.contains(shell),
            "an unknown shell did not name `{shell}` as an alternative: {message}"
        );
    }

    let out = run(&["completion"]);
    assert!(!out.status.success(), "`completion` alone succeeded");
    assert_eq!(stdout(&out), "", "`completion` alone wrote to stdout");
    assert!(
        stderr(&out).contains("SHELL"),
        "a missing shell did not name the argument: {}",
        stderr(&out)
    );
}

/// No credential store, no network, no token — and nothing created on disk
/// where credentials would live. A completion script is what a shell's
/// startup asks for, and a startup that triggered a login prompt or a
/// refresh round-trip would be unusable.
#[test]
fn it_needs_no_token_and_touches_no_credentials() {
    let home = sandbox_home();
    let config = home.join(".mapbox");
    let _ = std::fs::remove_dir_all(&config);

    let out = run(&["completion", "zsh"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !config.exists(),
        "`completion` created the credential store at {}",
        config.display()
    );
}

/// `--schema` describes every command in the tree, and `completion` is in the
/// tree. The unit test in `schema.rs` fails the build if it is not described
/// at all; this is the caller-visible half of the same fact.
#[test]
fn the_schema_describes_it_as_a_builtin() {
    let entry = schema()["commands"]
        .as_array()
        .expect("commands")
        .iter()
        .find(|command| command["command"] == "mapbox completion")
        .cloned()
        .expect("`mapbox completion` is described");

    assert_eq!(entry["kind"], "builtin");
    assert_eq!(entry["request"], serde_json::Value::Null);

    let shell = entry["arguments"]
        .as_array()
        .expect("arguments")
        .iter()
        .find(|argument| argument["name"] == "shell")
        .cloned()
        .expect("the shell positional is described");
    assert_eq!(shell["kind"], "positional");
    assert_eq!(shell["required"], true);

    let values: Vec<String> = shell["values"]
        .as_array()
        .expect("the shells are published as values")
        .iter()
        .map(|value| value.as_str().expect("a shell name").to_string())
        .collect();
    assert_eq!(values, SHELLS);
}
