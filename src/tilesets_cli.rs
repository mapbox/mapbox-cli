//! Proxy for the standalone Mapbox Tilesets CLI.
//!
//! `mapbox tilesets-cli <args...>` forwards `<args...>` verbatim to the
//! `tilesets` binary from the PyPI package [`mapbox-tilesets`]. That tool is
//! distributed and installed separately — this CLI neither bundles nor
//! installs it, it only locates it and hands over.
//!
//! Authentication is the one thing the proxy does supply. `tilesets` resolves
//! its token as `--token` flag, then `MAPBOX_ACCESS_TOKEN`, then
//! `MapboxAccessToken`; we resolve one the way every other `mapbox` command
//! does and set `MAPBOX_ACCESS_TOKEN` in the child's environment, so a
//! `mapbox auth login` carries over instead of the user having to keep a
//! second token in their shell. Precedence, highest first:
//!
//! 1. `mapbox tilesets-cli --token X …` — forwarded untouched; `tilesets`
//!    prefers its own flag over any environment, so this always wins. It is
//!    the escape hatch when one call needs a different token.
//! 2. `mapbox --token X tilesets-cli …` — typed on our side of the subcommand.
//! 3. `MAPBOX_ACCESS_TOKEN` / `MapboxAccessToken` already in the environment —
//!    inherited untouched, nothing injected. Skipped entirely by `--use-login`.
//! 4. Credentials stored by `mapbox auth login` (profile-aware, refreshed if
//!    stale).
//!
//! 3 above 4 matches every other `mapbox` command, and the convention of every
//! comparable CLI: the shorter-lived and more explicit the source, the higher
//! it ranks. It also keeps the environment working as an override for scripts,
//! containers and CI, where it is the *only* one available. The cost is that a
//! token left in a shell profile shadows a later `mapbox auth login`
//! indefinitely, so `auth::warn_if_environment_token_shadows_login` says so out
//! loud when the two disagree about the account.
//!
//! The token goes into the environment, never into argv: a token on a command
//! line is readable by any other local user via `ps`, an environment is not.
//!
//! [`mapbox-tilesets`]: https://github.com/mapbox/tilesets-cli

use anyhow::{anyhow, Error, Result};
use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::PathBuf;

/// Subcommand users type: `mapbox tilesets-cli ...`.
pub const COMMAND: &str = "tilesets-cli";

/// Console script name installed by the `mapbox-tilesets` package.
pub const DEFAULT_BINARY: &str = "tilesets";

/// Escape hatch for an install that isn't on `PATH` (pyenv shims, a venv that
/// isn't activated, a vendored copy).
const BINARY_ENV: &str = "MAPBOX_TILESETS_CLI";

/// Oldest Python `mapbox-tilesets` supports. `scripts/install.sh`'s
/// `python3_at_least_310` tests the same requirement; change one and change
/// the other.
///
/// Not compiled on Windows, where the package is unsupported and no arm
/// quotes a Python version — same gate as [`installed_python3_version`].
#[cfg(not(windows))]
const PYTHON_MIN_VERSION: &str = "3.10";

/// Name of the positional that soaks up everything meant for the child.
const ARGS: &str = "args";

/// The variable `tilesets` reads its token from, ahead of `MapboxAccessToken`.
const TOKEN_ENV: &str = "MAPBOX_ACCESS_TOKEN";

/// A token to hand the child, and where it came from. `--debug` reports the
/// source, because "which token did that use?" is the first question when a
/// call comes back 401 or 404.
pub enum ChildToken {
    /// `mapbox --token …`, typed ahead of the subcommand name.
    Flag(String),
    /// Credentials stored by `mapbox auth login`.
    Stored(String),
}

impl ChildToken {
    fn value(&self) -> &str {
        match self {
            Self::Flag(t) | Self::Stored(t) => t,
        }
    }

    fn source(&self) -> &'static str {
        match self {
            Self::Flag(_) => "--token flag",
            Self::Stored(_) => "stored credentials",
        }
    }
}

/// Picks the token to inject, or `None` to leave the child's environment alone.
///
/// `stored` is a closure so it is only consulted when actually needed: a
/// `--token` or an environment token skips the credential load, and with it
/// the lock and the possible refresh round-trip.
///
/// The `token` arg declares `.env(MAPBOX_ACCESS_TOKEN)`, so `get_one` alone
/// cannot tell a typed flag from the environment fallback — hence
/// `value_source`. The distinction matters because the two want opposite
/// treatment: a typed flag has to be injected, while an environment token is
/// already where the child will look for it, and re-setting it would only risk
/// overwriting the `MapboxAccessToken` spelling with the other one.
pub fn token_for_child<F>(matches: &ArgMatches, use_login: bool, stored: F) -> Option<ChildToken>
where
    F: FnOnce() -> Option<String>,
{
    if let Some(flag) = crate::auth::typed_token(matches) {
        return Some(ChildToken::Flag(flag));
    }

    if !use_login && crate::auth::environment_token().is_some() {
        return None;
    }

    stored().map(ChildToken::Stored)
}

/// Warns that `--output` stops at this process.
///
/// The proxy hands argv to a third-party binary that implements none of this
/// CLI's output contract. Honoring the flag would mean either mirroring the
/// child's command surface to translate it per subcommand — the coupling this
/// proxy exists to avoid — or capturing its stdio, which turns the child's
/// stderr into a pipe and silently costs `upload-source` the progress bar
/// click gates on `isatty`. Neither is worth it, so the flag is documented as
/// unsupported here and pursued upstream instead.
///
/// Only a value typed on this command line is worth a warning. Under `auto`
/// nothing was asked for, and the child does its own terminal detection
/// anyway — which is exactly the behavior we would want. `MAPBOX_OUTPUT` is
/// excluded for the same reason: it is exported once and applies to
/// everything, so warning on it would nag on every tileset upload forever.
pub fn warn_output_ignored(matches: &ArgMatches) {
    let explicit = matches.value_source(crate::output::ARG) == Some(ValueSource::CommandLine);
    if explicit {
        eprintln!(
            "Warning: `--{}` is not honored by `{COMMAND}` — its output comes from \
             `{DEFAULT_BINARY}`, which has no equivalent option.",
            crate::output::ARG
        );
    }
}

/// Warns that `--yes` stops at this process.
///
/// `mapbox --yes tilesets-cli delete <id>` reads as a non-interactive delete
/// and is not one: `tilesets delete` asks its own question, on its own
/// terminal, and `--yes` never reaches it. Left silent, the flag's whole
/// promise fails exactly where it matters most — a CI job that blocks on a
/// prompt nobody can answer.
///
/// Only warns when the flag was typed on this command line. `MAPBOX_YES` is
/// excluded for the same reason `MAPBOX_OUTPUT` is: it is exported once and
/// applies to everything, so warning on it would nag on every tileset command
/// forever.
///
/// The gap that leaves is known and judged acceptable: the environment
/// spelling is the one a CI job sets, and a CI job is where an unanswered
/// prompt hurts most. What makes it bearable is that the child's prompt is
/// `click.confirm`, which reads stdin — so with no terminal it raises `Abort`
/// rather than blocking, and the job fails fast with the child's message
/// instead of stalling. Warning on the variable would trade a confusing
/// failure on the rare destructive command for a warning on every upload.
/// README says to put `--force` in the command instead.
pub fn warn_yes_ignored(matches: &ArgMatches) {
    if matches.value_source(crate::confirm::ARG) != Some(ValueSource::CommandLine) {
        return;
    }
    eprintln!(
        "Warning: `--{}` is not honored by `{COMMAND}` — `{DEFAULT_BINARY}` asks its \
         own questions and has its own `-f`/`--force` on the commands that do.",
        crate::confirm::ARG
    );
}

pub fn command() -> Command {
    Command::new(COMMAND)
        .about("Proxy commands to the Mapbox Tilesets CLI (`tilesets`, installed separately)")
        .long_about(format!(
            "Forward arguments to the Mapbox Tilesets CLI.\n\n\
             Everything after `{COMMAND}` is passed through to the `tilesets` binary \
             untouched — including flags, so `{COMMAND} --help` shows the Tilesets \
             CLI's own help rather than this one. `tilesets` ships separately as the \
             Python package `mapbox-tilesets`; if it isn't installed, this command \
             says how to install it instead of running.\n\n\
             The token is resolved the way every other `mapbox` command does — \
             flag, then MAPBOX_ACCESS_TOKEN (or MapboxAccessToken), then \
             credentials stored by `mapbox auth login` — and handed to \
             `tilesets` in its environment, so a stored login carries over \
             without exporting a token by hand. --use-login skips the \
             environment step so a stored login outranks it.\n\n\
             Set {BINARY_ENV} to run a `tilesets` that isn't on your PATH."
        ))
        // Everything past this subcommand belongs to the child, so clap must
        // not claim any of it — not `--help`, and not `mapbox`'s own global
        // flags, which would otherwise be parsed here because they're global.
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .allow_hyphen_values(true)
        .arg(
            Arg::new(ARGS)
                .value_name("ARGS")
                .num_args(0..)
                .trailing_var_arg(true)
                .allow_hyphen_values(true)
                .value_parser(clap::value_parser!(OsString))
                .help("Arguments passed through to `tilesets` unchanged"),
        )
}

/// Rewrites argv so that nothing after the `tilesets-cli` subcommand is parsed
/// as a `mapbox` flag.
///
/// `mapbox`'s `--token` / `--username` / `--profile` / `--debug` are global,
/// which means clap also parses them *inside* every subcommand. For a
/// pass-through proxy that is wrong: `mapbox tilesets-cli --token X list` must
/// hand `--token X` to `tilesets`, and clap would otherwise silently swallow it
/// as our own flag — leaving the child to fail with a confusing "no token".
/// `allow_hyphen_values` does not help, because it only takes effect once the
/// first positional value has been seen.
///
/// Inserting `--` directly after the subcommand name stops clap's option
/// parsing at exactly the right point, so the rest reaches the positional
/// verbatim. Argv is returned unchanged when this invocation is not the proxy.
pub fn escape_passthrough_args(app: &Command, mut argv: Vec<OsString>) -> Vec<OsString> {
    let Some(pos) = subcommand_position(app, &argv) else {
        return argv;
    };

    // Already explicitly terminated by the caller — a second `--` would be
    // forwarded to the child as a literal argument.
    if argv.get(pos + 1).is_some_and(|next| next == "--") {
        return argv;
    }

    argv.insert(pos + 1, OsString::from("--"));
    argv
}

/// Index of the subcommand token in argv, if this invocation is the proxy.
///
/// Walks past leading global flags using the parent command's own argument
/// definitions rather than a hardcoded list, so it stays correct if `mapbox`
/// grows or renames a global flag.
fn subcommand_position(app: &Command, argv: &[OsString]) -> Option<usize> {
    let mut i = 1;
    while i < argv.len() {
        // A non-UTF-8 token before the subcommand can't be a flag we know or
        // the subcommand name; leave argv alone and let clap report it.
        let arg = argv[i].to_str()?;

        if arg == "--" {
            return None;
        } else if let Some(long) = arg.strip_prefix("--") {
            // `--name=value` carries its value inline.
            if !long.contains('=') && long_takes_value(app, long) {
                i += 1;
            }
        } else if arg.len() > 1 && arg.starts_with('-') {
            if short_cluster_takes_next(app, &arg[1..]) {
                i += 1;
            }
        } else {
            return (arg == COMMAND).then_some(i);
        }

        i += 1;
    }
    None
}

fn takes_value(arg: &Arg) -> bool {
    arg.get_num_args().map_or_else(
        || matches!(arg.get_action(), ArgAction::Set | ArgAction::Append),
        |range| range.takes_values(),
    )
}

fn long_takes_value(app: &Command, long: &str) -> bool {
    app.get_arguments()
        .any(|arg| arg.get_long() == Some(long) && takes_value(arg))
}

/// Whether a short-flag cluster (`-t`, `-tu`, `-tVALUE`) leaves its value to be
/// picked up from the *next* argv element.
fn short_cluster_takes_next(app: &Command, cluster: &str) -> bool {
    let mut chars = cluster.chars();
    while let Some(c) = chars.next() {
        let value_flag = app
            .get_arguments()
            .any(|arg| arg.get_short() == Some(c) && takes_value(arg));
        if value_flag {
            // `-tVALUE` is self-contained; only a bare `-t` reaches forward.
            return chars.as_str().is_empty();
        }
    }
    false
}

/// `mapbox` globals that `tilesets` has no counterpart for, so finding one in
/// the forwarded arguments means it was written in the wrong place rather than
/// meant for the child.
///
/// `--token` is deliberately absent: `tilesets` has its own, and forwarding it
/// is the documented per-call override. Kept honest by
/// `the_misplaced_globals_list_matches_the_app`.
/// Each mapbox-only global, with whether it expects a value.
///
/// Both halves are pinned against the real app by
/// `the_misplaced_globals_list_matches_the_app`: the spellings so a new
/// global forces a decision about forwarding it, and the value flag so the
/// hint below stays a command line that actually works.
const MAPBOX_ONLY_GLOBALS: [(&str, bool); 12] = [
    ("--use-login", false),
    ("--schema", false),
    ("--profile", true),
    ("--username", true),
    ("-u", true),
    ("--debug", false),
    ("--output", true),
    ("-o", true),
    ("--id", true),
    // `--timeout` bounds the requests *this* CLI makes, and this command
    // makes none: `tilesets` opens its own connections, and there is no
    // header, budget or setting of ours on them. Forwarding it would hand
    // the child an option it has never heard of.
    ("--timeout", true),
    // `--yes` is mapbox-only despite the child having the same idea: the
    // Tilesets CLI spells it `-f`/`--force`, per subcommand, and only on the
    // ones that prompt. Translating one into the other would mean mirroring
    // the child's command surface — the coupling `warn_output_ignored`
    // explains this proxy exists to avoid — so it is a warning here and a
    // warning in `warn_yes_ignored` for the other half of the mistake.
    ("--yes", false),
    ("-y", false),
];

/// Warns when a `mapbox` global was written after the subcommand name.
///
/// Globals work in any position on every other command, so putting one after
/// `tilesets-cli` is an easy mistake — and the resulting failure is `tilesets`
/// rejecting an option it has never heard of, which points nowhere near the
/// cause. Warn rather than intercept: everything after the subcommand name
/// belongs to the child, and quietly stealing an argument back would break
/// that the moment `tilesets` grows a flag of the same name.
pub fn warn_about_misplaced_globals(args: &[OsString]) {
    for arg in args {
        let Some(text) = arg.to_str() else { continue };
        // Stop at the child's own `--`: past it nothing is a flag at all.
        if text == "--" {
            return;
        }
        let name = text.split('=').next().unwrap_or(text);
        let Some((_, takes_a_value)) = MAPBOX_ONLY_GLOBALS.iter().find(|(g, _)| *g == name) else {
            continue;
        };
        // `--profile`, `--username` and `--output` all take a value, and the
        // naive hint `mapbox --output tilesets-cli ...` swallows the
        // subcommand as that value — advice that fails when followed.
        let placeholder = if *takes_a_value { " <value>" } else { "" };
        eprintln!(
            "Warning: `{name}` is a mapbox option, but written after `{COMMAND}` it is \
             forwarded to `{DEFAULT_BINARY}`, which does not know it. Put it before the \
             subcommand: `mapbox {name}{placeholder} {COMMAND} ...`"
        );
    }
}

/// What `<redacted>` replaces, so the tests and the code agree on the text.
const REDACTED: &str = "<redacted>";

/// Whether the argument is a Mapbox token rather than something a reader wants
/// to see.
///
/// The prefix is the discriminator — no username, tileset id or file path
/// starts with one — and the length keeps a user literally named `pk` from
/// having their account redacted out of a debug line. Real tokens run to
/// eighty characters and more.
fn looks_like_a_token(text: &str) -> bool {
    const PREFIXES: [&str; 3] = ["pk.", "sk.", "tk."];
    text.len() >= 40 && PREFIXES.iter().any(|prefix| text.starts_with(prefix))
}

/// The forwarded argv with any token value replaced, for `--debug` to print.
///
/// `--token` written *after* `tilesets-cli` belongs to the child and is
/// forwarded verbatim — the invocation `escape_passthrough_args` exists
/// because people write it, and the one place a token still travels in argv.
/// Printing it unredacted turns a secret that `ps` exposes for the length of
/// one process into one that persists: a CI log for its retention window,
/// terminal scrollback, a command line pasted into an issue. `executor`'s
/// `access_token=<redacted>` holds the same line for our own requests.
///
/// Both halves are needed. The flag forms catch a value the child was told to
/// use; the shape catches one written anywhere else, including after a `--`
/// where nothing is a flag any more.
fn redacted_argv(args: &[OsString]) -> Vec<String> {
    let mut rendered: Vec<String> = Vec::with_capacity(args.len());
    let mut value_is_a_token = false;

    for arg in args {
        let text = arg.to_string_lossy().into_owned();

        if value_is_a_token {
            value_is_a_token = false;
            // A value never starts with `-`, so a flag here means the token
            // flag was written with nothing after it. Redacting the next flag
            // would hide the shape of the line and protect nothing.
            if !text.starts_with('-') {
                rendered.push(REDACTED.to_string());
                continue;
            }
        }

        let redacted = match text.split_once('=') {
            // `--token=sk...`, keeping the flag readable.
            Some(("--token" | "-t", _)) => Some(format!(
                "{}={REDACTED}",
                text.split('=').next().unwrap_or("--token")
            )),
            _ if text == "--token" || text == "-t" => {
                value_is_a_token = true;
                None
            }
            // `-tsk...` — click accepts a short flag joined to its value.
            _ if text.starts_with("-t") && looks_like_a_token(&text[2..]) => {
                Some(format!("-t{REDACTED}"))
            }
            _ if looks_like_a_token(&text) => Some(REDACTED.to_string()),
            _ => None,
        };

        rendered.push(redacted.unwrap_or(text));
    }

    rendered
}

/// Pulls the pass-through arguments back out of a parsed `tilesets-cli` match.
pub fn forwarded_args(matches: &ArgMatches) -> Vec<OsString> {
    matches
        .get_many::<OsString>(ARGS)
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default()
}

/// The `tilesets` binary to run: the `MAPBOX_TILESETS_CLI` override if set to
/// something non-empty, otherwise a bare name resolved against `PATH`.
fn binary() -> (PathBuf, bool) {
    match std::env::var_os(BINARY_ENV) {
        Some(path) if !path.is_empty() => (PathBuf::from(path), true),
        _ => (PathBuf::from(DEFAULT_BINARY), false),
    }
}

/// Runs `tilesets` with `args`, injecting `token` into its environment.
///
/// Never returns `Ok`: on success this process either *becomes* the child
/// (Unix) or exits with the child's status code. A returned `Err` means the
/// child could not be started at all.
pub fn run(args: &[OsString], token: Option<ChildToken>, debug: bool) -> Result<()> {
    let (path, overridden) = binary();

    let mut cmd = std::process::Command::new(&path);
    cmd.args(args);
    if let Some(token) = &token {
        cmd.env(TOKEN_ENV, token.value());
    }

    if debug {
        let source = token
            .as_ref()
            .map_or("inherited environment", ChildToken::source);
        eprintln!(
            "[debug] exec {} {} (token from {source})",
            path.display(),
            redacted_argv(args).join(" ")
        );
    }

    Err(launch_failed(&path, overridden, handoff(cmd)))
}

/// Hands the terminal to the child. Only returns if that failed.
///
/// Unix `exec`s rather than spawning so the child inherits this process
/// outright — Ctrl-C during a long `upload-source`, interactive prompts and
/// progress bars all behave exactly as they do when running `tilesets`
/// directly, with no parent left to forward signals to. Elsewhere, spawn and
/// forward the exit code.
#[cfg(unix)]
fn handoff(mut cmd: std::process::Command) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    // Now or never: after `exec` this process is the child, and `main` never
    // gets to send the event, so it goes without an exit code. Only when the
    // binary resolves — a missing `tilesets` is a failure `main` reports,
    // and the event should carry it.
    if resolves(std::path::Path::new(cmd.get_program())) {
        crate::events::finish(None);
    }
    cmd.exec()
}

/// Whether `exec` would find `program`: as given if it has a directory in
/// it, otherwise on `PATH`.
#[cfg(unix)]
fn resolves(program: &std::path::Path) -> bool {
    if program.components().count() > 1 {
        return program.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[cfg(not(unix))]
fn handoff(mut cmd: std::process::Command) -> std::io::Error {
    match cmd.status() {
        // 130 is the conventional "killed by SIGINT" code; on Windows a
        // `None` code means the child was terminated rather than exiting.
        Ok(status) => {
            let code = status.code().unwrap_or(130);
            // `exit` skips `main`'s way out, where the event is sent.
            crate::events::finish(u32::try_from(code).ok());
            std::process::exit(code)
        }
        Err(err) => err,
    }
}

/// Reports what `python3 --version` says, or `None` if there is no `python3`
/// on `PATH`.
///
/// Only called from this error path, once `tilesets` is already known to be
/// missing, so the cost of spawning a process to ask doesn't matter.
#[cfg(not(windows))]
fn installed_python3_version() -> Option<String> {
    let output = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .ok()?;
    // Python before 3.4 printed "Python X.Y.Z" on stderr rather than stdout;
    // anything this old to install for is still worth naming, so check both.
    let text = [&output.stdout, &output.stderr]
        .into_iter()
        .map(|s| String::from_utf8_lossy(s).trim().to_string())
        .find(|s| !s.is_empty())?;
    text.strip_prefix("Python ").map(str::to_owned)
}

/// Turns a spawn failure into an actionable message — for the common case
/// (not installed) that means install instructions, not an errno.
///
/// `scripts/install.sh` offers to run one of these commands at the end of an
/// install, and prints these same instructions when it cannot or the user
/// declines. The two wordings are the same explanation in two languages;
/// change one and change the other.
fn launch_failed(path: &std::path::Path, overridden: bool, err: std::io::Error) -> Error {
    if err.kind() != ErrorKind::NotFound {
        return anyhow!("Could not run `{}`: {err}", path.display());
    }

    if overridden {
        return anyhow!(
            "{BINARY_ENV} points at `{}`, but there is no executable there.\n\n\
             Unset {BINARY_ENV} to fall back to a `{DEFAULT_BINARY}` on your PATH, or \
             correct it to the full path of the Tilesets CLI executable.",
            path.display()
        );
    }

    // The scope doc limits the Python package to macOS and Linux; on Windows
    // there is no pip or pipx command that will work, so say that instead of
    // handing over one that can't.
    #[cfg(windows)]
    {
        anyhow!(
            "The Mapbox Tilesets CLI (`{DEFAULT_BINARY}`) is not installed, or is not on your PATH.\n\n\
             `mapbox {COMMAND}` does not bundle it — it forwards your arguments to the separately \
             distributed Python package `mapbox-tilesets`, which is not supported on Windows. \
             Run it from WSL instead.\n\n\
             If you have a `{DEFAULT_BINARY}` this CLI should use anyway — a wrapper that \
             shells into WSL, say — point {BINARY_ENV} at it:\n\n    \
             $env:{BINARY_ENV} = 'C:\\path\\to\\{DEFAULT_BINARY}.cmd'\n\n\
             Docs: https://github.com/mapbox/tilesets-cli"
        )
    }

    #[cfg(not(windows))]
    {
        let python_line = match installed_python3_version() {
            Some(version) => format!("Yours is {version}."),
            None => "There is no python3 on your PATH.".to_string(),
        };

        anyhow!(
            "The Mapbox Tilesets CLI (`{DEFAULT_BINARY}`) is not installed, or is not on your PATH.\n\n\
             `mapbox {COMMAND}` does not bundle it — it forwards your arguments to the separately \
             distributed Python package `mapbox-tilesets`, which needs Python {PYTHON_MIN_VERSION} \
             or newer. {python_line}\n\n\
             Install it:\n\n    \
             pipx install mapbox-tilesets\n\n    \
             # recommended: keeps it isolated, and works on systems where pip\n    \
             # refuses to install into the OS interpreter\n\n\
             If you have no pipx and your Python is not managed by your OS:\n\n    \
             python3 -m pip install --user mapbox-tilesets\n\n\
             Then confirm it is reachable:\n\n    \
             {DEFAULT_BINARY} --version\n\n\
             If it is installed somewhere not on your PATH, point this CLI at it directly:\n\n    \
             export {BINARY_ENV}=/path/to/{DEFAULT_BINARY}\n\n\
             Docs: https://github.com/mapbox/tilesets-cli"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes the tests that have to mutate the process environment.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Names the proxy treats as carrying a token.
    const TOKEN_VARS: [&str; 2] = ["MAPBOX_ACCESS_TOKEN", "MapboxAccessToken"];

    /// Runs `body` with the token environment forced to a known state, then
    /// restores the real one.
    ///
    /// Token resolution reads the environment, so without this these tests
    /// would pass or fail depending on whether the developer happens to export
    /// a token — the exact ambient dependency the tests exist to pin down.
    /// Serialized because the environment is process-global. Assertions belong
    /// *outside* the closure so a failure still restores it.
    fn with_token_env<T>(vars: &[(&str, &str)], body: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<OsString>)> = TOKEN_VARS
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();

        for name in TOKEN_VARS {
            std::env::remove_var(name);
        }
        for (name, value) in vars {
            std::env::set_var(name, value);
        }

        let result = body();

        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        result
    }

    /// Parses argv through the real `mapbox` command (global flags and all),
    /// with no services wired in — the proxy is independent of them.
    fn parse(args: &[&str]) -> clap::ArgMatches {
        let app = crate::build_app(&[]);
        let argv: Vec<OsString> = std::iter::once("mapbox")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let escaped = escape_passthrough_args(&app, argv);
        app.try_get_matches_from(escaped)
            .unwrap_or_else(|e| panic!("failed to parse {args:?}: {e}"))
    }

    /// Args the proxy would hand to `tilesets` for this command line.
    fn forwarded(args: &[&str]) -> Vec<String> {
        let matches = parse(args);
        let (name, sub) = matches.subcommand().expect("a subcommand was matched");
        assert_eq!(name, COMMAND);
        forwarded_args(sub)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn forwards_a_plain_command() {
        assert_eq!(
            forwarded(&["tilesets-cli", "list", "mofei"]),
            ["list", "mofei"]
        );
    }

    #[test]
    fn forwards_nothing_when_given_nothing() {
        assert!(forwarded(&["tilesets-cli"]).is_empty());
    }

    #[test]
    fn does_not_intercept_help_or_version() {
        assert_eq!(forwarded(&["tilesets-cli", "--help"]), ["--help"]);
        assert_eq!(forwarded(&["tilesets-cli", "--version"]), ["--version"]);
    }

    /// `tilesets` is reserved for a future command of our own, so it must not
    /// resolve to this proxy — nor be quietly accepted as anything else.
    #[test]
    fn the_tilesets_name_is_not_a_command() {
        let app = crate::build_app(&[]);
        let names: Vec<&str> = app.get_subcommands().map(|c| c.get_name()).collect();
        assert!(!names.contains(&"tilesets"), "found in {names:?}");

        let all_aliases: Vec<&str> = app
            .get_subcommands()
            .flat_map(|c| c.get_all_aliases())
            .collect();
        assert!(
            !all_aliases.contains(&"tilesets"),
            "aliased in {all_aliases:?}"
        );
    }

    /// Regression: `--token`/`--username`/`--profile`/`--debug` are global, so
    /// clap parses them inside subcommands too. After the proxy's name they
    /// belong to `tilesets`, and swallowing them here would strip the user's
    /// token without a word.
    #[test]
    fn globals_after_the_subcommand_belong_to_the_child() {
        assert_eq!(
            forwarded(&["tilesets-cli", "--token", "abc", "list", "mofei"]),
            ["--token", "abc", "list", "mofei"]
        );
        assert_eq!(
            forwarded(&["tilesets-cli", "-t", "abc", "-u", "bob", "list"]),
            ["-t", "abc", "-u", "bob", "list"]
        );
        assert_eq!(forwarded(&["tilesets-cli", "-tabc"]), ["-tabc"]);
        assert_eq!(
            forwarded(&["tilesets-cli", "--profile", "foo", "--debug", "list"]),
            ["--profile", "foo", "--debug", "list"]
        );
    }

    /// The mirror image: before the subcommand name they are still ours.
    #[test]
    fn globals_before_the_subcommand_stay_with_mapbox() {
        let matches = parse(&["--debug", "--profile", "work", "tilesets-cli", "list"]);
        assert!(matches.get_flag("debug"));
        assert_eq!(
            matches.get_one::<String>("profile").map(String::as_str),
            Some("work")
        );
        assert_eq!(forwarded(&["--debug", "tilesets-cli", "list"]), ["list"]);
    }

    /// A value that happens to spell the subcommand name is a value, not the
    /// subcommand.
    #[test]
    fn a_flag_value_is_not_mistaken_for_the_subcommand() {
        let matches = parse(&["--profile", "tilesets-cli", "auth", "logout"]);
        let (name, _) = matches.subcommand().expect("a subcommand was matched");
        assert_eq!(name, "auth");
    }

    #[test]
    fn an_explicit_separator_is_not_doubled() {
        assert_eq!(
            forwarded(&["tilesets-cli", "--", "--token", "abc"]),
            ["--token", "abc"]
        );
    }

    #[test]
    fn other_subcommands_are_left_alone() {
        let app = crate::build_app(&[]);
        let argv: Vec<OsString> = ["mapbox", "auth", "login", "--profile", "work"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(escape_passthrough_args(&app, argv.clone()), argv);
    }

    #[test]
    fn an_explicit_token_flag_wins_and_skips_the_credential_load() {
        let mut consulted = false;
        let token = with_token_env(&[("MAPBOX_ACCESS_TOKEN", "pk.environment")], || {
            let matches = parse(&["--token", "sk.explicit", "tilesets-cli", "list"]);
            token_for_child(&matches, false, || {
                consulted = true;
                Some("tk.stored".to_string())
            })
        });

        assert!(
            !consulted,
            "an explicit --token must not trigger a credential load, \
             which would take the lock and possibly a refresh round-trip"
        );
        assert!(matches!(token, Some(ChildToken::Flag(t)) if t == "sk.explicit"));
    }

    /// The precedence this CLI uses everywhere, and the one every comparable
    /// tool uses: an environment token outranks the stored login. It is also
    /// the only override available to a script, container or CI job, so
    /// swallowing it would be worse than the confusion it can cause.
    #[test]
    fn an_environment_token_outranks_stored_credentials() {
        let mut consulted = false;
        let token = with_token_env(&[("MAPBOX_ACCESS_TOKEN", "pk.environment")], || {
            let matches = parse(&["tilesets-cli", "list"]);
            token_for_child(&matches, false, || {
                consulted = true;
                Some("tk.stored".to_string())
            })
        });

        assert!(token.is_none(), "the child already has it; inject nothing");
        assert!(!consulted, "no reason to read credentials we won't use");
    }

    /// `MapboxAccessToken` is a spelling only `tilesets` knows, so clap never
    /// sees it. Injecting `MAPBOX_ACCESS_TOKEN` on top would silently outrank
    /// a token the user had already supplied.
    #[test]
    fn the_legacy_environment_spelling_counts_too() {
        let token = with_token_env(&[("MapboxAccessToken", "pk.legacy")], || {
            let matches = parse(&["tilesets-cli", "list"]);
            token_for_child(&matches, false, || Some("tk.stored".to_string()))
        });

        assert!(token.is_none());
    }

    #[test]
    fn stored_credentials_are_used_when_nothing_else_offers_one() {
        let token = with_token_env(&[], || {
            let matches = parse(&["tilesets-cli", "list"]);
            token_for_child(&matches, false, || Some("tk.stored".to_string()))
        });

        assert!(matches!(token, Some(ChildToken::Stored(t)) if t == "tk.stored"));
    }

    #[test]
    fn nothing_is_injected_when_there_is_nothing_to_inject() {
        let token = with_token_env(&[], || {
            let matches = parse(&["tilesets-cli", "list"]);
            token_for_child(&matches, false, || None)
        });

        assert!(
            token.is_none(),
            "with nothing to inject the child's environment must be left alone"
        );
    }

    /// The way to say "use my login" without hunting the token out of
    /// credentials.json: `--use-login` skips the environment entirely.
    #[test]
    fn use_login_skips_the_environment() {
        let token = with_token_env(&[("MAPBOX_ACCESS_TOKEN", "pk.environment")], || {
            let matches = parse(&["--use-login", "tilesets-cli", "list"]);
            token_for_child(&matches, true, || Some("tk.stored".to_string()))
        });

        assert!(matches!(token, Some(ChildToken::Stored(t)) if t == "tk.stored"));
    }

    /// Most explicit still wins: a typed token beats the flag that asks for
    /// the stored one.
    #[test]
    fn a_typed_token_outranks_use_login() {
        let mut consulted = false;
        let token = with_token_env(&[], || {
            let matches = parse(&["--use-login", "--token", "sk.typed", "tilesets-cli", "list"]);
            token_for_child(&matches, true, || {
                consulted = true;
                Some("tk.stored".to_string())
            })
        });

        assert!(matches!(token, Some(ChildToken::Flag(t)) if t == "sk.typed"));
        assert!(!consulted);
    }

    /// Nothing stored to honor. `main` turns this into an error rather than
    /// quietly handing over the environment token the flag asked to ignore.
    #[test]
    fn use_login_with_nothing_stored_resolves_to_nothing() {
        let token = with_token_env(&[("MAPBOX_ACCESS_TOKEN", "pk.environment")], || {
            let matches = parse(&["--use-login", "tilesets-cli", "list"]);
            token_for_child(&matches, true, || None)
        });

        assert!(token.is_none());
    }

    /// A `--token` written *after* the subcommand belongs to `tilesets`, which
    /// prefers its own flag over the environment — so it stays the per-call
    /// override even though we also resolve one of our own.
    #[test]
    fn a_forwarded_token_flag_stays_the_childs_business() {
        assert_eq!(
            forwarded(&["tilesets-cli", "--token", "sk.forwarded", "list"]),
            ["--token", "sk.forwarded", "list"]
        );

        let token = with_token_env(&[], || {
            let matches = parse(&["tilesets-cli", "--token", "sk.forwarded", "list"]);
            token_for_child(&matches, false, || Some("tk.stored".to_string()))
        });
        assert!(matches!(token, Some(ChildToken::Stored(_))));
    }

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    /// A fake with a real token's shape: the prefix that identifies one and
    /// enough length to clear the floor that protects short arguments.
    const FAKE_TOKEN: &str = "sk.eyJ1IjoiZmFrZSIsImEiOiJmYWtlIn0.AAAAAAAAAAAAAAAAAAAAAA";

    /// Every way a token can be written on the child's command line.
    ///
    /// `--debug` prints that line, and a `--token` after `tilesets-cli` is
    /// forwarded verbatim, so this is the one place a token still travels in
    /// argv. Unredacted it outlives the process it belonged to — CI logs,
    /// scrollback, a pasted command.
    #[test]
    fn every_spelling_of_a_forwarded_token_is_redacted() {
        let cases: [&[&str]; 6] = [
            &["--token", FAKE_TOKEN, "list", "someone"],
            &["-t", FAKE_TOKEN, "list", "someone"],
            &["list", FAKE_TOKEN],
            // Past a `--` nothing is a flag any more, which is why the shape
            // check exists alongside the flag forms.
            &["list", "--", FAKE_TOKEN],
            &["--token=X", "list"],
            &["-tX", "list"],
        ];

        for case in cases {
            let mut case = argv(case);
            // The two joined forms carry the token inside one argument.
            if let Some(last) = case
                .iter_mut()
                .find(|arg| matches!(arg.to_str(), Some("--token=X") | Some("-tX")))
            {
                let joined = last.to_string_lossy().replace('X', FAKE_TOKEN);
                *last = OsString::from(joined);
            }

            let rendered = super::redacted_argv(&case).join(" ");
            assert!(
                !rendered.contains(FAKE_TOKEN),
                "token survived in {rendered:?}"
            );
            assert!(
                rendered.contains(REDACTED),
                "nothing was redacted in {rendered:?}"
            );
        }
    }

    /// The line still has to be worth printing. A tileset id is
    /// `owner.name`, which is the shape the token check looks at, so the
    /// length floor is what keeps it readable.
    #[test]
    fn ordinary_arguments_survive_redaction() {
        let rendered = super::redacted_argv(&argv(&[
            "list",
            "mapbox.mapbox-streets-v8",
            "--indent",
            "2",
            "--token",
        ]))
        .join(" ");

        assert_eq!(rendered, "list mapbox.mapbox-streets-v8 --indent 2 --token");
    }

    /// A short `pk.`-prefixed argument is far more likely to be an account
    /// called `pk` than a token, and redacting it would cost the reader the
    /// thing they turned `--debug` on to see.
    #[test]
    fn a_short_argument_is_not_mistaken_for_a_token() {
        let rendered = super::redacted_argv(&argv(&["list", "pk.short"])).join(" ");
        assert_eq!(rendered, "list pk.short");
    }

    /// The list is hardcoded, so pin it to the app: adding a global flag to
    /// `mapbox` without deciding whether it is forwardable should fail here
    /// rather than silently produce a confusing error from `tilesets`.
    #[test]
    fn the_misplaced_globals_list_matches_the_app() {
        let app = crate::build_app(&[]);
        // Both spellings: `-o` reached the child unwarned while `--output`
        // was caught, which is precisely the confusion this warning exists to
        // prevent, and a long-only comparison could not see it.
        let mut globals: Vec<(String, bool)> = app
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .flat_map(|arg| {
                let wants_value = super::takes_value(arg);
                [
                    arg.get_long()
                        .map(|long| (format!("--{long}"), wants_value)),
                    arg.get_short()
                        .map(|short| (format!("-{short}"), wants_value)),
                ]
            })
            .flatten()
            .collect();
        globals.sort();

        let mut expected: Vec<(String, bool)> = MAPBOX_ONLY_GLOBALS
            .iter()
            .map(|(spelling, wants_value)| (spelling.to_string(), *wants_value))
            // `tilesets` has its own --token; forwarding both spellings is
            // intended.
            .chain([("--token".to_string(), true), ("-t".to_string(), true)])
            .collect();
        expected.sort();

        assert_eq!(
            globals, expected,
            "a global was added, renamed, or changed whether it takes a value — \
             decide whether it is forwardable and update MAPBOX_ONLY_GLOBALS"
        );
    }

    #[test]
    fn a_missing_binary_explains_how_to_install_it() {
        let err = launch_failed(
            std::path::Path::new(DEFAULT_BINARY),
            false,
            std::io::Error::from(ErrorKind::NotFound),
        );
        let msg = err.to_string();
        // Ungated on purpose, unlike the two below: every arm of this message
        // has to name the escape hatch, Windows included. The override is not
        // Unix-only — `handoff` spawns whatever path it is given there — so a
        // Windows user who has arranged a reachable `tilesets` (a wrapper that
        // shells into WSL) must not be left reading "run it from WSL" with no
        // way to say they already have.
        assert!(msg.contains(BINARY_ENV), "{msg}");

        #[cfg(not(windows))]
        {
            assert!(msg.contains("pipx install mapbox-tilesets"), "{msg}");
            // The constant, not a hand-typed "3.10" — this is what stops the
            // requirement drifting out of sync with `scripts/install.sh`
            // again.
            assert!(msg.contains(PYTHON_MIN_VERSION), "{msg}");
        }

        #[cfg(windows)]
        assert!(msg.contains("WSL"), "{msg}");
    }

    #[test]
    fn a_broken_override_blames_the_env_var_not_the_install() {
        let err = launch_failed(
            std::path::Path::new("/nope/tilesets"),
            true,
            std::io::Error::from(ErrorKind::NotFound),
        );
        let msg = err.to_string();
        assert!(msg.contains(BINARY_ENV), "{msg}");
        assert!(msg.contains("/nope/tilesets"), "{msg}");
        assert!(!msg.contains("pipx"), "{msg}");
    }

    /// A binary that exists but won't start is a different problem, and
    /// install instructions would be a misleading answer to it.
    #[test]
    fn other_launch_failures_are_reported_as_themselves() {
        let err = launch_failed(
            std::path::Path::new(DEFAULT_BINARY),
            false,
            std::io::Error::from(ErrorKind::PermissionDenied),
        );
        let msg = err.to_string();
        assert!(msg.contains("Could not run"), "{msg}");
        assert!(!msg.contains("pipx"), "{msg}");
    }
}
