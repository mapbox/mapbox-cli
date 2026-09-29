//! `mapbox mcp` — register a Mapbox MCP server with a coding agent's CLI.
//!
//! Not the same kind of "install" as [`crate::agent_skills`] or
//! [`crate::generate_skills`]. Those write a directory this CLI fully owns;
//! an MCP server has to be registered into a config store that belongs to
//! the agent and may already list other servers, so writing it directly
//! would mean parsing and rewriting someone else's file. Claude Code has its
//! own CLI verb for this instead (`claude mcp add`), so this shells out to
//! it rather than editing `~/.claude.json` by hand — the same reasoning
//! [`crate::tilesets_cli`] has for exec-ing `tilesets` rather than
//! reimplementing it.
//!
//! # Servers and clients
//!
//! [`SERVERS`] is every Mapbox MCP server this CLI knows how to point a
//! client at — a name, a label, and the hosted HTTP endpoint that serves it.
//! [`CLIENTS`] is every coding-agent CLI this command knows how to drive —
//! today, only Claude Code. Adding a server is one row in [`SERVERS`], since
//! [`client_get`]/[`client_add`] are generic over `Client` rather than
//! hardcoded to `claude`. Adding a *client* is only that cheap when the new
//! one happens to speak the same `<binary> mcp get <name>` / `<binary> mcp
//! add --transport http <name> <url>` verbs Claude Code does — a client with
//! a genuinely different CLI (or one that needs a config file edited
//! instead of a CLI at all, like Claude Desktop or VS Code, see the module
//! docs' opening paragraph) needs its own pair of functions, not just a row.
//!
//! Deliberately not [`crate::skill_dest::Agent`]: that table is entirely
//! about *where a skill file goes under an agent's home directory*, and its
//! own module docs say "installed" means a directory exists, not a `PATH`
//! lookup. What this needs is the opposite signal — is the client's own CLI
//! binary invocable at all — so it keeps its own, much smaller table.
//!
//! # One request per (server, client) pair, no state of its own
//!
//! `claude mcp get <name>` is both the "is this even installed" check and
//! the "is `claude` on `PATH` at all" check in one call: a spawn failure
//! with [`std::io::ErrorKind::NotFound`] means the client isn't there, a
//! non-zero exit means the server isn't registered yet, and success means
//! it already is. That's a stronger idempotency check than `claude mcp
//! add`'s own duplicate-add failure, which is plain text
//! ("MCP server NAME already exists in SCOPE config") with no code to match
//! on — checking `get` first also means never spamming stderr with a
//! failure that would otherwise be normal, expected output.

use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;
use std::process::Command;

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
    /// `--server` value, and the name it's registered under in the client
    /// (`claude mcp add <name> <url>`).
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

/// One coding-agent CLI this command knows how to drive.
struct Client {
    /// `--client` value.
    flag: &'static str,
    label: &'static str,
    /// Overrides which binary answers to `flag`, the same escape hatch
    /// `MAPBOX_TILESETS_CLI` gives `tilesets_cli` — a `claude` not on `PATH`,
    /// or a wrapper script standing in for it.
    binary_env: &'static str,
    default_binary: &'static str,
}

const CLIENTS: &[Client] = &[Client {
    flag: "claude-code",
    label: "Claude Code",
    binary_env: "MAPBOX_CLAUDE_CLI",
    default_binary: "claude",
}];

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

pub fn command() -> ClapCommand {
    let server_flags: Vec<&'static str> = SERVERS.iter().map(|s| s.flag).collect();
    let client_flags: Vec<&'static str> = CLIENTS.iter().map(|c| c.flag).collect();

    ClapCommand::new(COMMAND)
        .about("Set up a Mapbox MCP server for a coding agent")
        .long_about(
            "Registers a Mapbox MCP server — direct tool-calling access to Mapbox's APIs, \
             not just guidance about them — with a coding agent's own CLI.\n\n\
             Different from `mapbox agent-skills` and `mapbox generate-skills`, which write \
             files this CLI fully owns. An MCP server has to be added to a config store that \
             belongs to the agent and may already list other servers, so this shells out to \
             the agent's own CLI (`claude mcp add`) rather than editing that file directly.\n\n\
             Only Claude Code is supported today, against the hosted Mapbox MCP endpoints — \
             no token, no npm package, no Node version to manage. `mapbox mcp list` names \
             every server and client this command knows about.",
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
                        .help("Register for every project rather than this one"),
                )
                .arg(executor::dry_run_arg(
                    "Report what would be installed, then exit without installing it",
                )),
        )
}

/// Whether `claude mcp get <name>` found the server, found nothing, or
/// couldn't be run at all.
enum GetOutcome {
    Installed,
    NotInstalled,
    ClientNotFound,
}

fn client_get(client: &Client, server_name: &str) -> GetOutcome {
    match Command::new(client.binary())
        .args(["mcp", "get", server_name])
        .output()
    {
        Ok(output) if output.status.success() => GetOutcome::Installed,
        Ok(_) => GetOutcome::NotInstalled,
        Err(e) if e.kind() == io::ErrorKind::NotFound => GetOutcome::ClientNotFound,
        // Some other failure to spawn at all (permissions, an
        // interpreter missing for a script wrapper): treated the same as
        // "not found," since either way this client can't be driven.
        Err(_) => GetOutcome::ClientNotFound,
    }
}

/// Whether `client`'s CLI can be run at all, without asking about any
/// particular server. `--version` rather than a bogus `mcp get NAME`: it is
/// what the binary is *for*, so a client that answers has no reason to
/// treat it specially, unlike a lookup for a server name this command made
/// up.
fn client_reachable(client: &Client) -> bool {
    Command::new(client.binary())
        .arg("--version")
        .output()
        .is_ok()
}

/// `claude mcp add --transport http <name> <url> [--scope user]`.
fn client_add(client: &Client, server: &Server, global: bool) -> Result<()> {
    let mut args: Vec<&OsStr> = vec![
        OsStr::new("mcp"),
        OsStr::new("add"),
        OsStr::new("--transport"),
        OsStr::new("http"),
        OsStr::new(server.flag),
        OsStr::new(server.url),
    ];
    if global {
        args.push(OsStr::new("--scope"));
        args.push(OsStr::new("user"));
    }

    let output = Command::new(client.binary())
        .args(&args)
        .output()
        .map_err(|e| spawn_failed(client, e))?;

    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = if detail.trim().is_empty() {
        String::from_utf8_lossy(&output.stdout).into_owned()
    } else {
        detail.into_owned()
    };
    Err(CliError::new(
        "error",
        format!(
            "{} could not register {}: {}",
            client.label,
            server.label,
            detail.trim()
        ),
    )
    .into())
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

fn list(mode: Mode) -> Result<()> {
    let mut lines = Vec::new();
    let mut rows = Vec::new();

    for client in CLIENTS {
        let reachable = client_reachable(client);
        for server in SERVERS {
            let status = if !reachable {
                "client not found"
            } else {
                match client_get(client, server.flag) {
                    GetOutcome::Installed => "installed",
                    GetOutcome::NotInstalled | GetOutcome::ClientNotFound => "not installed",
                }
            };
            lines.push(format!("{:14}  {:10}  {status}", server.flag, client.flag));
            rows.push(json!({
                "server": server.flag,
                "client": client.flag,
                "status": status,
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

    for client in &clients {
        for server in &servers {
            let outcome = client_get(client, server.flag);
            let (status, error) = match outcome {
                GetOutcome::Installed => ("already installed", None),
                GetOutcome::ClientNotFound => {
                    lines.push(format!(
                        "{}: {} is not on PATH, skipped.",
                        server.label, client.label
                    ));
                    results.push(json!({
                        "server": server.flag,
                        "client": client.flag,
                        "status": "client_not_found",
                    }));
                    continue;
                }
                GetOutcome::NotInstalled if dry_run => ("would install", None),
                GetOutcome::NotInstalled => match client_add(client, server, global) {
                    Ok(()) => ("installed", None),
                    Err(e) => ("failed", Some(e.to_string())),
                },
            };

            lines.push(format!(
                "{}: {} for {} — {status}.",
                server.label, server.url, client.label
            ));
            let mut row = json!({
                "server": server.flag,
                "client": client.flag,
                "status": status.replace(' ', "_"),
            });
            if let Some(message) = error {
                row["error"] = json!(message);
            }
            results.push(row);
        }
    }

    output::emit(mode, &lines.join("\n"), json!({ "results": results }))
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
}
