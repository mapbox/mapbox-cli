//! `mapbox mcp` — register a Mapbox MCP server with a coding agent's CLI.
//!
//! Not the same kind of "install" as [`crate::agent_skills`] or
//! [`crate::generate_skills`]. Those write a directory this CLI fully owns;
//! an MCP server has to be registered into a config store that belongs to
//! the agent and may already list other servers, so writing it directly
//! would mean parsing and rewriting someone else's file. Every client here
//! offers its own way to do that write instead, so this shells out rather
//! than reimplementing it — the same reasoning [`crate::tilesets_cli`] has
//! for exec-ing `tilesets` rather than reimplementing it.
//!
//! # Servers and clients
//!
//! [`SERVERS`] is every Mapbox MCP server this CLI knows how to point a
//! client at — a name, a label, and the hosted HTTP endpoint that serves it.
//! [`CLIENTS`] is every coding-agent CLI this command knows how to drive, and
//! [`ClientKind`] is the one real fork in how that happens:
//!
//! - **[`ClientKind::Verb`]** — Claude Code and Codex each have their own
//!   `<binary> mcp add`/`<binary> mcp get`, so this shells out to that rather
//!   than touching either one's config file. The two still need their own
//!   `verb_add` arm apiece: Claude Code takes `add --transport http <name>
//!   <url>`, Codex takes `add <name> --url <url>`, and Codex additionally
//!   starts an OAuth flow as *part of* `add` for a server that advertises
//!   support for it — verified live against the real Mapbox hosted
//!   endpoint, where that flow itself fails (an incompatibility between
//!   Codex's OAuth client and this server, not something this command can
//!   fix) while the config entry is written regardless. See [`verb_add`]'s
//!   Codex arm for how that's told apart from an add that really failed.
//! - **[`ClientKind::AddMcpFlag`]** — VS Code and Cursor have no `mcp`
//!   subcommand at all, but both expose a top-level `--add-mcp '<json>'`
//!   flag, and the same input JSON works for both — confirmed live, in an
//!   isolated `--user-data-dir`, that it correctly merges with an existing,
//!   differently-named entry on each. Where it lands is genuinely different
//!   per client, though, and [`AddMcpConfig`] names that per row rather than
//!   assuming one editor's behavior for its fork: VS Code writes a small,
//!   dedicated `User/mcp.json`; Cursor writes into `User/settings.json`
//!   under an `"mcp"` key instead — and that difference showed up *during
//!   this feature's own development*, when updating the installed Cursor
//!   (which additionally had a broken `--add-mcp` before the update, a
//!   separate, now-fixed bug) moved its own storage from the former shape to
//!   the latter. Neither client's `--add-mcp` refuses a duplicate name —
//!   both **silently overwrite** an existing entry under the *same* name if
//!   its content differs — so [`file_get`] reads the config file directly
//!   (never writes it) to decide whether to call `--add-mcp` at all, since
//!   neither the flag nor either file format gives this command anything to
//!   check first on its own. Both editors' `--mcp-workspace`-style
//!   per-project flag was also tested live and found non-functional in the
//!   installed versions (Cursor's is not a recognized option at all;
//!   neither one is documented for `code`), so both always register in the
//!   user profile regardless of `--global`, disclosed with one printed line
//!   rather than silently ignored.
//!
//! Adding a server is one row in [`SERVERS`], since every function here is
//! generic over `Client`/`Server`. Adding a *client* of a kind already here
//! is a new `Client` row plus one new arm in `verb_add`/`verb_get`'s match
//! (for `Verb`) or just a new [`AddMcpConfig`] (for `AddMcpFlag`, since
//! `add_mcp_flag_add`/`file_get` are already generic over where the config
//! lives). A client that needs a config file hand-edited with no
//! CLI-driven write path at all — Claude Desktop, Goose — needs a third
//! `ClientKind` and is real, separate work, not covered here.
//!
//! Deliberately not [`crate::skill_dest::Agent`]: that table is entirely
//! about *where a skill file goes under an agent's home directory*, and its
//! own module docs say "installed" means a directory exists, not a `PATH`
//! lookup. What this needs is the opposite signal — is the client's own CLI
//! binary invocable at all — so it keeps its own, much smaller table.
//!
//! # Checking first
//!
//! Every client here is checked before ever attempting to register a
//! server, and the check means something slightly different per kind: for
//! `Verb`, `<binary> mcp get <name>` (a spawn failure with
//! [`std::io::ErrorKind::NotFound`] means the client isn't there, a
//! non-zero exit means the server isn't registered yet, success means it
//! already is — stronger than parsing `add`'s own failure text, which for
//! Claude Code is plain, uncoded prose and for Codex doesn't exist at all,
//! since Codex's `add` doesn't refuse a duplicate on its own). For
//! `AddMcpFlag`, reading the client's own config file directly (see
//! [`AddMcpConfig`]), since nothing about the flag or either file format
//! offers an existence check any other way.

use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::Result;
use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgAction, ArgMatches, Command as ClapCommand};
use serde_json::json;

use crate::executor;
use crate::output::{self, CliError, Mode};
use crate::remedy::Remedy;

pub const COMMAND: &str = "mcp";

const LIST: &str = "list";
const INSTALL: &str = "install";

const SERVER_ARG: &str = "server";
const CLIENT_ARG: &str = "client";
const GLOBAL_ARG: &str = "global";

/// One Mapbox MCP server this command can point a client at.
struct Server {
    /// `--server` value, and the name it's registered under in the client.
    flag: &'static str,
    label: &'static str,
    /// The hosted, OAuth-authenticated endpoint — no npm package, no token,
    /// no Node version to worry about. See the module docs for why this is
    /// the only transport supported so far.
    url: &'static str,
}

const SERVERS: &[Server] = &[
    Server {
        flag: "mapbox",
        label: "Mapbox MCP",
        url: "https://mcp.mapbox.com/mcp",
    },
    Server {
        flag: "mapbox-devkit",
        label: "Mapbox DevKit MCP",
        url: "https://mcp-devkit.mapbox.com/mcp",
    },
];

/// How a [`Client`] is driven. See the module docs for what each means.
#[derive(PartialEq, Eq)]
enum ClientKind {
    Verb,
    AddMcpFlag,
}

/// One coding-agent CLI this command knows how to drive.
struct Client {
    /// `--client` value.
    flag: &'static str,
    label: &'static str,
    /// Overrides which binary answers to `flag`, the same escape hatch
    /// `MAPBOX_TILESETS_CLI` gives `tilesets_cli` — a binary not on `PATH`,
    /// or a wrapper script standing in for it in a test.
    binary_env: &'static str,
    default_binary: &'static str,
    kind: ClientKind,
    /// `AddMcpFlag` only. `None` for `Verb` clients, which have no config
    /// file this command ever reads.
    add_mcp: Option<AddMcpConfig>,
}

/// Where an `AddMcpFlag` client's own MCP registrations actually live —
/// genuinely different per client, and confirmed live for the exact
/// installed version at the time, not assumed from one editor's behavior
/// applying to its fork. This could drift with a future release the same
/// way it already did once *during this feature's own development*: an
/// older installed Cursor wrote a dedicated `mcp.json` (like VS Code still
/// does); updating it moved the same data into `settings.json` under an
/// `"mcp"` key instead, with no `mcp.json` written at all.
struct AddMcpConfig {
    /// Directory name this editor's fork uses under the OS's config-home
    /// (`dirs::config_dir()` — `~/Library/Application Support` on macOS,
    /// `$XDG_CONFIG_HOME`/`~/.config` on Linux, `%APPDATA%` on Windows) —
    /// "Code", not "VS Code".
    app_dir_name: &'static str,
    /// File under `User/` that holds the servers this flag writes.
    file_name: &'static str,
    /// Path to the servers object inside that file's JSON, root first:
    /// `["servers"]` for VS Code's dedicated file, `["mcp", "servers"]` for
    /// Cursor's entry inside its general settings.
    servers_path: &'static [&'static str],
    /// Overrides the resolved config-home base directory entirely — the
    /// same kind of escape hatch `binary_env` gives the binary path, and
    /// how tests point this at a scratch directory rather than the real
    /// one.
    env_override: &'static str,
}

const CLIENTS: &[Client] = &[
    Client {
        flag: "claude-code",
        label: "Claude Code",
        binary_env: "MAPBOX_CLAUDE_CLI",
        default_binary: "claude",
        kind: ClientKind::Verb,
        add_mcp: None,
    },
    Client {
        flag: "codex",
        label: "Codex",
        binary_env: "MAPBOX_CODEX_CLI",
        default_binary: "codex",
        kind: ClientKind::Verb,
        add_mcp: None,
    },
    Client {
        flag: "vscode",
        label: "VS Code",
        binary_env: "MAPBOX_CODE_CLI",
        default_binary: "code",
        kind: ClientKind::AddMcpFlag,
        add_mcp: Some(AddMcpConfig {
            app_dir_name: "Code",
            file_name: "mcp.json",
            servers_path: &["servers"],
            env_override: "MAPBOX_CODE_CONFIG_DIR",
        }),
    },
    Client {
        flag: "cursor",
        label: "Cursor",
        binary_env: "MAPBOX_CURSOR_CLI",
        default_binary: "cursor",
        kind: ClientKind::AddMcpFlag,
        add_mcp: Some(AddMcpConfig {
            app_dir_name: "Cursor",
            file_name: "settings.json",
            servers_path: &["mcp", "servers"],
            env_override: "MAPBOX_CURSOR_CONFIG_DIR",
        }),
    },
];

impl Client {
    /// The binary to run: the override if set to something non-empty,
    /// otherwise the bare name resolved against `PATH` — identical to
    /// `tilesets_cli::binary`.
    fn binary(&self) -> PathBuf {
        match std::env::var_os(self.binary_env) {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => PathBuf::from(self.default_binary),
        }
    }
}

/// `binary`, then — on Windows only, and only when it carries no extension
/// of its own — the same name again with `.cmd`, `.bat` and `.exe`
/// appended, in that order.
///
/// `CreateProcessW`, what `std::process::Command` calls on Windows, does
/// not probe `PATHEXT` the way `cmd.exe` does for a bare name, so a CLI
/// installed as a `.cmd` shim (every npm-installed one — VS Code's and
/// Cursor's own launchers are `.cmd` too) is invisible to a plain
/// `Command::new("code")` there. Not verified live (no Windows machine
/// here), but this is the documented shape of the gap, and trying the
/// extensions costs nothing extra on a bare name that already resolves.
fn command_candidates(binary: &Path) -> Vec<PathBuf> {
    if !cfg!(windows) || binary.extension().is_some() {
        return vec![binary.to_path_buf()];
    }
    let mut out = vec![binary.to_path_buf()];
    for ext in ["cmd", "bat", "exe"] {
        let mut candidate = binary.as_os_str().to_os_string();
        candidate.push(".");
        candidate.push(ext);
        out.push(PathBuf::from(candidate));
    }
    out
}

/// Runs `client`'s binary with `args`, captured, trying
/// [`command_candidates`] in turn until one is not `NotFound`. For a quick
/// check (`--version`, `mcp get`) where nothing the child prints needs to
/// reach a person live — see [`spawn_live`] for the one call site where
/// that is not true.
fn spawn(client: &Client, args: &[&str]) -> io::Result<std::process::Output> {
    let mut last_err = None;
    for candidate in command_candidates(&client.binary()) {
        match Command::new(&candidate).args(args).output() {
            Ok(output) => return Ok(output),
            Err(e) if e.kind() == io::ErrorKind::NotFound => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)))
}

/// [`spawn`], but for the one family of calls that can have something worth
/// a person seeing *while it runs*: registering a server. Codex's own `add`
/// starts an OAuth flow for a server that advertises support for it, which
/// can print a URL to open — captured alone, as every other call here is,
/// that URL is never shown and the run looks like it hung. Forwards both of
/// the child's streams to this process's own stderr line by line as they
/// arrive, on a second thread for stdout so draining one pipe can never
/// block behind the other filling up, and returns the exit status plus
/// everything printed, concatenated, for a caller that still wants the text
/// (an error detail, Codex's login-incomplete message).
fn spawn_live(client: &Client, args: &[&str]) -> io::Result<(ExitStatus, String)> {
    let mut last_err = None;
    for candidate in command_candidates(&client.binary()) {
        let mut child = match Command::new(&candidate)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                last_err = Some(e);
                continue;
            }
            Err(e) => return Err(e),
        };

        let stdout = child.stdout.take().expect("stdout is piped above");
        let stderr = child.stderr.take().expect("stderr is piped above");

        let stdout_thread = std::thread::spawn(move || {
            let mut captured = String::new();
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                eprintln!("{line}");
                captured.push_str(&line);
                captured.push('\n');
            }
            captured
        });

        let mut captured = String::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("{line}");
            captured.push_str(&line);
            captured.push('\n');
        }
        captured.push_str(&stdout_thread.join().unwrap_or_default());

        let status = child.wait()?;
        return Ok((status, captured));
    }
    Err(last_err.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)))
}

pub fn command() -> ClapCommand {
    let server_flags: Vec<&'static str> = SERVERS.iter().map(|s| s.flag).collect();
    let client_flags: Vec<&'static str> = CLIENTS.iter().map(|c| c.flag).collect();

    ClapCommand::new(COMMAND)
        .about("Set up a Mapbox MCP server for a coding agent")
        .long_about(
            "Registers a Mapbox MCP server — direct tool-calling access to Mapbox's APIs, \
             not just guidance about them — with a coding agent's own CLI or config store.\n\n\
             Different from `mapbox agent-skills` and `mapbox generate-skills`, which write \
             files this CLI fully owns. An MCP server has to be added to a config store that \
             belongs to the agent and may already list other servers, so this shells out to \
             the agent's own tooling (`claude mcp add`, `codex mcp add`, `code --add-mcp`) \
             rather than editing that store directly.\n\n\
             Against the hosted Mapbox MCP endpoints only — no token, no npm package, no Node \
             version to manage. `mapbox mcp list` names every server and client this command \
             knows about.",
        )
        .subcommand_required(true)
        .subcommand(
            ClapCommand::new(LIST).about("List known MCP servers and clients, and what's already installed"),
        )
        .subcommand(
            ClapCommand::new(INSTALL)
                .about("Register a server with a client. With neither flag, every known server for every detected client")
                .arg(
                    Arg::new(SERVER_ARG)
                        .long(SERVER_ARG)
                        .value_name("SERVER")
                        .action(ArgAction::Append)
                        .value_parser(PossibleValuesParser::new(server_flags))
                        .help("Server to install, repeatable. Defaults to every known server"),
                )
                .arg(
                    Arg::new(CLIENT_ARG)
                        .long(CLIENT_ARG)
                        .value_name("CLIENT")
                        .action(ArgAction::Append)
                        .value_parser(PossibleValuesParser::new(client_flags))
                        .help("Client to install for, repeatable. Defaults to whichever clients are detected"),
                )
                .arg(
                    Arg::new(GLOBAL_ARG)
                        .long(GLOBAL_ARG)
                        .action(ArgAction::SetTrue)
                        .help(
                            "Register for every project rather than this one. Some clients \
                             have no per-project scope and register globally either way",
                        ),
                )
                .arg(executor::dry_run_arg(
                    "Report what would be installed, then exit without installing it",
                )),
        )
}

/// What checking a (client, server) pair before installing found.
enum GetOutcome {
    Installed,
    NotInstalled,
    ClientNotFound,
    /// `AddMcpFlag` only: the config file exists but didn't parse as JSON.
    /// Never guessed past — see [`file_get`].
    Unreadable,
}

fn client_get(client: &Client, server_name: &str) -> GetOutcome {
    match client.kind {
        ClientKind::Verb => verb_get(client, server_name),
        ClientKind::AddMcpFlag => file_get(client, server_name),
    }
}

/// `<binary> mcp get <name>` — the same invocation for every `Verb` client;
/// only `add` differs per client. See the module docs.
fn verb_get(client: &Client, server_name: &str) -> GetOutcome {
    match spawn(client, &["mcp", "get", server_name]) {
        Ok(output) if output.status.success() => GetOutcome::Installed,
        Ok(_) => GetOutcome::NotInstalled,
        // Every spawn failure, not only `NotFound`: this client's `mcp get`
        // cannot be driven either way, which is what `ClientNotFound`
        // means here.
        Err(_) => GetOutcome::ClientNotFound,
    }
}

/// `AddMcpFlag` clients: reads the config file directly rather than writing
/// it. A file that doesn't exist yet is "not installed" (a fresh client);
/// one that exists but can't be read or doesn't parse is `Unreadable`
/// rather than guessed past as either state — proceeding past content this
/// command can't understand is exactly the kind of guess that could
/// silently discard something `--add-mcp` itself would have clobbered.
/// Parsed as JSONC, not plain JSON: VS Code's `mcp.json` and Cursor's
/// `settings.json` both allow comments and a trailing comma, and a normal,
/// untouched file of either kind otherwise reads as unreadable on first
/// contact.
fn file_get(client: &Client, server_name: &str) -> GetOutcome {
    if !client_reachable(client) {
        return GetOutcome::ClientNotFound;
    }
    let Some(config) = &client.add_mcp else {
        return GetOutcome::NotInstalled;
    };
    let Some(path) = config_path(config) else {
        return GetOutcome::NotInstalled;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // Only "the file isn't there yet" means "not installed." Anything
        // else reading it, no permission, it's a directory, invalid UTF-8,
        // means this command cannot tell, and must not guess: a `NotFound`
        // only check here was the actual bug — every other error used to
        // fall into "not installed" too, which then ran `--add-mcp` over a
        // file this command had never actually looked at.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return GetOutcome::NotInstalled,
        Err(_) => return GetOutcome::Unreadable,
    };
    let value: serde_json::Value =
        match jsonc_parser::parse_to_serde_value(&text, &jsonc_parser::ParseOptions::default()) {
            Ok(value) => value,
            Err(_) => return GetOutcome::Unreadable,
        };
    let mut cursor = &value;
    for key in config.servers_path {
        match cursor.get(key) {
            Some(next) => cursor = next,
            None => return GetOutcome::NotInstalled,
        }
    }
    if cursor.get(server_name).is_some() {
        GetOutcome::Installed
    } else {
        GetOutcome::NotInstalled
    }
}

/// An `AddMcpFlag` client's config file, honoring `env_override` before
/// falling back to `dirs::config_dir()` — the macOS/Linux/Windows-correct
/// base, confirmed live against the real path these editors write to
/// (`~/Library/Application Support/<Name>/User/...` on macOS). Deliberately
/// not `skill_dest`'s `XdgConfig` base, which exists precisely because
/// *that* module's agents read `~/.config` even on macOS — the opposite of
/// what was verified here.
fn config_path(config: &AddMcpConfig) -> Option<PathBuf> {
    if let Some(value) = std::env::var_os(config.env_override) {
        if !value.is_empty() {
            return Some(PathBuf::from(value).join("User").join(config.file_name));
        }
    }
    Some(
        dirs::config_dir()?
            .join(config.app_dir_name)
            .join("User")
            .join(config.file_name),
    )
}

/// Whether `client`'s CLI can be run at all, without asking about any
/// particular server. `--version` rather than a bogus `mcp get NAME`: it is
/// what the binary is *for*, so a client that answers has no reason to
/// treat it specially, unlike a lookup for a server name this command made
/// up. The same check for every kind: `AddMcpFlag` clients answer
/// `--version` too, confirmed live for both `code` and `cursor`.
fn client_reachable(client: &Client) -> bool {
    spawn(client, &["--version"]).is_ok()
}

/// What attempting to register a server found, once it wasn't already
/// there. Distinct from a plain success because Codex's own exit code
/// answers a different question than "was this written" — see `verb_add`'s
/// Codex arm.
enum AddOutcome {
    Installed,
    /// The config entry was written (confirmed via a follow-up `get`), but
    /// the client's own login/OAuth step that normally accompanies it did
    /// not complete. `String` is the detail to show, not a failure.
    InstalledLoginIncomplete(String),
}

fn client_add(client: &Client, server: &Server, global: bool) -> Result<AddOutcome> {
    match client.kind {
        ClientKind::Verb => verb_add(client, server, global),
        ClientKind::AddMcpFlag => add_mcp_flag_add(client, server, global),
    }
}

fn verb_add(client: &Client, server: &Server, global: bool) -> Result<AddOutcome> {
    match client.flag {
        "claude-code" => {
            let mut args = vec!["mcp", "add", "--transport", "http", server.flag, server.url];
            if global {
                args.push("--scope");
                args.push("user");
            }
            let (status, captured) =
                spawn_live(client, &args).map_err(|e| spawn_failed(client, e))?;
            if status.success() {
                Ok(AddOutcome::Installed)
            } else {
                Err(add_failed(client, server, &captured))
            }
        }
        "codex" => {
            // No local/user scope distinction in codex's own CLI (every
            // add is what it calls a "global" server), so `global` is
            // unused here — same as it is for VS Code.
            let (status, captured) =
                spawn_live(client, &["mcp", "add", server.flag, "--url", server.url])
                    .map_err(|e| spawn_failed(client, e))?;

            // Codex starts an OAuth flow as part of `add` for a server
            // that advertises support for it, and its exit code reflects
            // whether *that* succeeded, not whether the entry was written
            // — confirmed live: against the real Mapbox hosted endpoint,
            // the OAuth step itself fails (an incompatibility between
            // Codex's OAuth client and this server), but `codex mcp get`
            // immediately afterward shows the entry present regardless.
            // So the truth this reports is a follow-up `get`, not this
            // exit code. Its output is forwarded live rather than only
            // captured (`spawn_live`, not `spawn`) because that OAuth step
            // can print a URL to open, and a captured-only child would
            // leave it unshown while the run looks hung.
            let written = matches!(verb_get(client, server.flag), GetOutcome::Installed);
            if !written {
                return Err(add_failed(client, server, &captured));
            }
            if status.success() {
                Ok(AddOutcome::Installed)
            } else {
                Ok(AddOutcome::InstalledLoginIncomplete(
                    captured.trim().to_string(),
                ))
            }
        }
        _ => unreachable!("every `Verb` client has an arm here"),
    }
}

/// `<binary> --add-mcp '<json>'`. `global` is accepted for symmetry with
/// `verb_add` but unused: neither VS Code's nor Cursor's `--add-mcp` has a
/// working per-project scope (checked directly: `code --help` names no such
/// flag at all, and Cursor's documented `--mcp-workspace` is rejected by
/// its own CLI as an unrecognized option in the version tested), so this
/// always registers in the user profile and says so once, plainly, rather
/// than silently ignoring what was asked for.
fn add_mcp_flag_add(client: &Client, server: &Server, global: bool) -> Result<AddOutcome> {
    if !global {
        output::progress(&format!(
            "{} has no per-project scope for this; registering in the user profile instead.",
            client.label
        ));
    }

    let payload = json!({
        "name": server.flag,
        "type": "http",
        "url": server.url,
    })
    .to_string();

    let (status, captured) =
        spawn_live(client, &["--add-mcp", &payload]).map_err(|e| spawn_failed(client, e))?;

    if status.success() {
        Ok(AddOutcome::Installed)
    } else {
        Err(add_failed(client, server, &captured))
    }
}

fn add_failed(client: &Client, server: &Server, captured: &str) -> anyhow::Error {
    CliError::new(
        "error",
        format!(
            "{} could not register {}: {}",
            client.label,
            server.label,
            captured.trim()
        ),
    )
    .into()
}

fn spawn_failed(client: &Client, err: io::Error) -> CliError {
    if err.kind() == io::ErrorKind::NotFound {
        return client_not_found(std::slice::from_ref(client));
    }
    CliError::new(
        "error",
        format!("Could not run `{}`: {err}", client.default_binary),
    )
}

/// The `no_agent_detected`-shaped error: no `--client` was named, and no
/// known client's CLI could be run. `known` is `CLIENTS` unless a specific
/// client was asked for and wasn't there, in which case it names just that
/// one — a different, unrelated case from "nothing was named at all," the
/// same asymmetry `skill_dest::resolve` draws for `--agent`.
fn client_not_found(known: &[Client]) -> CliError {
    let flags: Vec<&str> = known.iter().map(|c| c.flag).collect();
    CliError::new(
        "mcp_client_not_found",
        format!(
            "No supported coding-agent CLI was found: {}.",
            flags.join(", ")
        ),
    )
    .with_remedy(Remedy::default().with_fix(&format!(
        "Install one of these CLIs, or pass --client to name one anyway ({}).",
        flags.join(", ")
    )))
}

pub fn run(matches: &ArgMatches, mode: Mode) -> Result<()> {
    let (action, action_matches) = matches
        .subcommand()
        .expect("`mcp` sets subcommand_required(true)");

    match action {
        LIST => list(mode),
        INSTALL => install(action_matches, mode),
        _ => unreachable!("`mcp` declares only two subcommands"),
    }
}

fn wanted_servers(matches: &ArgMatches) -> Vec<&'static Server> {
    let requested: Vec<&str> = matches
        .get_many::<String>(SERVER_ARG)
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    if requested.is_empty() {
        return SERVERS.iter().collect();
    }
    SERVERS
        .iter()
        .filter(|s| requested.contains(&s.flag))
        .collect()
}

fn wanted_clients(matches: &ArgMatches) -> Result<Vec<&'static Client>> {
    let requested: Vec<&str> = matches
        .get_many::<String>(CLIENT_ARG)
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();

    if !requested.is_empty() {
        // Clap already refused anything not a known flag, so every one of
        // these resolves.
        return Ok(CLIENTS
            .iter()
            .filter(|c| requested.contains(&c.flag))
            .collect());
    }

    let detected: Vec<&'static Client> = CLIENTS.iter().filter(|c| client_reachable(c)).collect();

    if detected.is_empty() {
        return Err(client_not_found(CLIENTS).into());
    }
    Ok(detected)
}

/// Every status either `list` or `install` can report, in both the text a
/// person reads and the JSON value a script matches on. A fixed table
/// rather than deriving one rendering from the other: that was the actual
/// bug behind `installed__login_incomplete` (two underscores) — replacing
/// each space *and* comma in "installed, login incomplete" one character at
/// a time hits both the comma and the space that follows it. Also what
/// keeps `list` and `install` naming the same state the same way, since
/// both read `.json()` off the one list here instead of building their own
/// spelling.
enum Status {
    Installed,
    NotInstalled,
    AlreadyInstalled,
    WouldInstall,
    InstalledLoginIncomplete,
    Failed,
    ClientNotFound,
    ConfigUnreadable,
}

impl Status {
    fn text(&self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::NotInstalled => "not installed",
            Self::AlreadyInstalled => "already installed",
            Self::WouldInstall => "would install",
            Self::InstalledLoginIncomplete => "installed, login incomplete",
            Self::Failed => "failed",
            Self::ClientNotFound => "client not found",
            Self::ConfigUnreadable => "config unreadable",
        }
    }

    fn json(&self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::NotInstalled => "not_installed",
            Self::AlreadyInstalled => "already_installed",
            Self::WouldInstall => "would_install",
            Self::InstalledLoginIncomplete => "installed_login_incomplete",
            Self::Failed => "failed",
            Self::ClientNotFound => "client_not_found",
            Self::ConfigUnreadable => "config_unreadable",
        }
    }
}

fn status_for(outcome: &GetOutcome) -> Status {
    match outcome {
        GetOutcome::Installed => Status::Installed,
        GetOutcome::NotInstalled => Status::NotInstalled,
        GetOutcome::ClientNotFound => Status::ClientNotFound,
        GetOutcome::Unreadable => Status::ConfigUnreadable,
    }
}

fn list(mode: Mode) -> Result<()> {
    let mut lines = Vec::new();
    let mut rows = Vec::new();

    for client in CLIENTS {
        let reachable = client_reachable(client);
        for server in SERVERS {
            let status = if !reachable {
                Status::ClientNotFound
            } else {
                status_for(&client_get(client, server.flag))
            };
            lines.push(format!(
                "{:14}  {:10}  {}",
                server.flag,
                client.flag,
                status.text()
            ));
            rows.push(json!({
                "server": server.flag,
                "client": client.flag,
                "status": status.json(),
            }));
        }
    }

    output::emit(mode, &lines.join("\n"), json!({ "servers": rows }))
}

fn install(matches: &ArgMatches, mode: Mode) -> Result<()> {
    let servers = wanted_servers(matches);
    let clients = wanted_clients(matches)?;
    let global = matches.get_flag(GLOBAL_ARG);
    let dry_run = executor::wants_dry_run(matches);

    let mut lines = Vec::new();
    let mut results = Vec::new();
    let mut any_failed = false;

    for client in &clients {
        for server in &servers {
            let outcome = client_get(client, server.flag);
            let (status, error) = match outcome {
                GetOutcome::Installed => (Status::AlreadyInstalled, None),
                GetOutcome::ClientNotFound => {
                    lines.push(format!(
                        "{}: {} is not on PATH, skipped.",
                        server.label, client.label
                    ));
                    results.push(json!({
                        "server": server.flag,
                        "client": client.flag,
                        "status": Status::ClientNotFound.json(),
                    }));
                    continue;
                }
                GetOutcome::Unreadable => {
                    lines.push(format!(
                        "{}: {}'s config could not be read, skipped.",
                        server.label, client.label
                    ));
                    results.push(json!({
                        "server": server.flag,
                        "client": client.flag,
                        "status": Status::ConfigUnreadable.json(),
                    }));
                    continue;
                }
                GetOutcome::NotInstalled if dry_run => (Status::WouldInstall, None),
                GetOutcome::NotInstalled => match client_add(client, server, global) {
                    Ok(AddOutcome::Installed) => (Status::Installed, None),
                    Ok(AddOutcome::InstalledLoginIncomplete(detail)) => {
                        (Status::InstalledLoginIncomplete, Some(detail))
                    }
                    Err(e) => {
                        any_failed = true;
                        (Status::Failed, Some(e.to_string()))
                    }
                },
            };

            lines.push(format!(
                "{}: {} for {} — {}.",
                server.label,
                server.url,
                client.label,
                status.text()
            ));
            let mut row = json!({
                "server": server.flag,
                "client": client.flag,
                "status": status.json(),
            });
            if let Some(message) = error {
                row["error"] = json!(message);
            }
            results.push(row);
        }
    }

    output::emit(mode, &lines.join("\n"), json!({ "results": results }))?;

    // The structured result above already names which pair failed and why
    // — this only decides the exit code, for a caller that checks that and
    // nothing else. Skipped pairs (client not found, config unreadable)
    // don't count: nothing was attempted there to call a failure.
    if any_failed {
        return Err(CliError::new(
            "error",
            "At least one server could not be registered; see the results above.",
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_server_has_a_distinct_flag() {
        let flags: std::collections::BTreeSet<&str> = SERVERS.iter().map(|s| s.flag).collect();
        assert_eq!(flags.len(), SERVERS.len());
    }

    #[test]
    fn every_client_has_a_distinct_flag() {
        let flags: std::collections::BTreeSet<&str> = CLIENTS.iter().map(|c| c.flag).collect();
        assert_eq!(flags.len(), CLIENTS.len());
    }

    #[test]
    fn every_verb_client_has_an_add_arm() {
        for client in CLIENTS.iter().filter(|c| c.kind == ClientKind::Verb) {
            assert!(
                ["claude-code", "codex"].contains(&client.flag),
                "{} is a Verb client with no arm in verb_add's match",
                client.flag
            );
        }
    }

    #[test]
    fn every_add_mcp_flag_client_names_its_config_file() {
        for client in CLIENTS.iter().filter(|c| c.kind == ClientKind::AddMcpFlag) {
            assert!(
                client.add_mcp.is_some(),
                "{} is an AddMcpFlag client with no AddMcpConfig",
                client.flag
            );
        }
    }

    #[test]
    fn the_command_line_parses() {
        command()
            .try_get_matches_from([
                "mcp",
                "install",
                "--server",
                "mapbox",
                "--client",
                "claude-code",
            ])
            .expect("parses");
        command()
            .try_get_matches_from(["mcp", "list"])
            .expect("parses");
    }

    /// The actual bug: deriving JSON from the display text by replacing
    /// each space and comma one at a time turned "installed, login
    /// incomplete" into "installed__login_incomplete" (two underscores,
    /// one per character replaced, not one per word boundary). A fixed
    /// table per status can't make that mistake.
    #[test]
    fn every_json_status_is_single_underscore_snake_case() {
        for status in [
            Status::Installed,
            Status::NotInstalled,
            Status::AlreadyInstalled,
            Status::WouldInstall,
            Status::InstalledLoginIncomplete,
            Status::Failed,
            Status::ClientNotFound,
            Status::ConfigUnreadable,
        ] {
            let json = status.json();
            assert!(!json.contains("__"), "{json:?} has a double underscore");
            assert!(!json.contains(' '), "{json:?} has a space");
            assert!(!json.contains(','), "{json:?} has a comma");
            assert_eq!(json, json.to_lowercase(), "{json:?} is not lowercase");
        }
        assert_eq!(
            Status::InstalledLoginIncomplete.json(),
            "installed_login_incomplete"
        );
    }

    /// `list` and `install` share one status table now, rather than each
    /// spelling the same state differently in JSON (`"not installed"` with
    /// a space from one, `"client_not_found"` snake_case from the other).
    #[test]
    fn list_and_install_json_statuses_agree_for_shared_states() {
        assert_eq!(
            status_for(&GetOutcome::ClientNotFound).json(),
            Status::ClientNotFound.json()
        );
        assert_eq!(
            status_for(&GetOutcome::Unreadable).json(),
            Status::ConfigUnreadable.json()
        );
        assert_eq!(
            status_for(&GetOutcome::Installed).json(),
            Status::Installed.json()
        );
    }

    #[test]
    fn a_bare_name_tries_cmd_bat_and_exe_only_on_windows() {
        let candidates = command_candidates(Path::new("code"));
        if cfg!(windows) {
            let exts: Vec<_> = candidates
                .iter()
                .skip(1)
                .map(|p| p.extension().and_then(|e| e.to_str()).unwrap_or(""))
                .collect();
            assert_eq!(exts, ["cmd", "bat", "exe"]);
            assert_eq!(candidates[0], Path::new("code"));
        } else {
            assert_eq!(candidates, vec![PathBuf::from("code")]);
        }
    }

    #[test]
    fn a_name_that_already_has_an_extension_is_tried_once() {
        // Windows or not: a caller that named `.exe` explicitly meant it,
        // and guessing further extensions on top would be wrong either way.
        assert_eq!(
            command_candidates(Path::new("code.exe")),
            vec![PathBuf::from("code.exe")]
        );
    }
}
