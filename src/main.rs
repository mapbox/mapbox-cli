use std::process::ExitCode;

use anyhow::Result;
use clap::builder::{
    FalseyValueParser, PossibleValuesParser, StringValueParser, StyledStr, TypedValueParser,
};
use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Arg, ArgAction, ArgMatches, Command};

mod account_usage;
mod agent_detect;
mod agent_skills;
/// Test-only: the checked-in command-surface fixture. It lives in the crate
/// rather than under `tests/` because a hidden alias exists only on the
/// built `clap` tree — see the module's own docs.
#[cfg(test)]
mod api_command_surface;
mod auth;
mod completion;
mod confirm;
mod deprecation;
mod executor;
mod feature_flags;
mod generate_skills;
mod http;
mod link;
mod output;
mod remedy;
mod schema;
mod skill_dest;
mod spec;
mod telemetry;
mod tilesets_cli;
mod uninstall;
mod update_check;

use output::{CliError, Mode};
use remedy::Remedy;
use spec::ServiceSpec;

/// Where this CLI's source and issue tracker live, written down once so that
/// a repository rename is one edit rather than a grep.
///
/// The two installers keep a copy of their own each. They are downloaded and
/// run on their own, with nothing of this crate beside them, so this is the
/// single source for the Rust half rather than for all three.
const REPO_URL: &str = "https://github.com/mapbox/cli";

/// Rejects a value the spec says is a number, while still yielding a
/// `String`.
///
/// `value_parser!(i64)` would store an `i64`, and the executor reads every
/// query parameter with `get_one::<String>` — it would find nothing there and
/// drop the parameter silently, which is worse than not checking at all.
fn numeric_parser(kind: spec::Numeric) -> impl clap::builder::TypedValueParser<Value = String> {
    StringValueParser::new().try_map(move |raw: String| match kind {
        spec::Numeric::Integer => raw.parse::<i64>().map(|_| raw),
        spec::Numeric::Float => raw
            .parse::<f64>()
            .map(|_| raw.clone())
            .map_err(|_| raw.parse::<i64>().unwrap_err()),
    })
}

/// Narrows `allow_hyphen_values` for [`HYPHEN_LEADING_VALUE_PARAMS`] back
/// down: a value shaped like a flag is still refused, so a forgotten value
/// on `--near`/`--bbox`/`--proximity`/`--origin` errors instead of silently
/// eating the next flag — the same failure mode `--q` is guarded against
/// entirely, just narrowed here to let a real coordinate pair through.
fn hyphen_leading_value_parser() -> impl clap::builder::TypedValueParser<Value = String> {
    StringValueParser::new().try_map(|raw: String| {
        if looks_like_a_flag(&raw) {
            Err(format!("`{raw}` looks like a flag, not a value"))
        } else {
            Ok(raw)
        }
    })
}

/// A bare `--foo`, or exactly one ASCII letter after a single `-` — the only
/// two shapes this CLI's flags take, and one no coordinate pair, `ip` value
/// or place name shares (those put a digit right after the `-`, not a
/// letter). Checked by shape rather than by a list of flag names, so a flag
/// added later is covered without a matching update here.
fn looks_like_a_flag(raw: &str) -> bool {
    if raw.starts_with("--") {
        return true;
    }
    match raw.strip_prefix('-') {
        Some(rest) => rest.len() == 1 && rest.starts_with(|c: char| c.is_ascii_alphabetic()),
        None => false,
    }
}

/// A parameter's description, cut to its first sentence for a line of help.
///
/// The spec text is kept whole by `parse_spec` — `--schema` publishes it and
/// a cut there lands inside identifiers and decimals. Help has a line to
/// work with, so it shortens here, at the only place that wants it.
pub fn first_sentence(text: &str) -> &str {
    text.split('.').next().unwrap_or(text).trim()
}

fn help_text(param: &spec::Parameter) -> String {
    param
        .description
        .as_deref()
        .map(first_sentence)
        .unwrap_or_default()
        .to_string()
}

/// The coordinate-shaped parameters whose value legitimately starts with `-`
/// without being a bare number.
///
/// `allow_negative_numbers`, set on every generated command, only recognizes
/// a value that is *itself* a number — `-74.0`, not `-121.9,37.4`. A
/// comma-joined pair still reads as a cluster of short flags, which is what
/// made `--proximity -121.9,37.4` unusable west of Greenwich. These four are
/// every parameter across the twelve specs that takes one.
const HYPHEN_LEADING_VALUE_PARAMS: &[&str] = &["proximity", "bbox", "near", "origin"];

/// Whether clap should accept a `-`-leading value for this parameter.
///
/// Deliberately not every parameter, and not a numeric one either:
/// `allow_hyphen_values` disables clap's forgotten-value check, so
/// `--q --limit` would take `--limit` as the search string instead of
/// erroring. A genuine negative number already parses via
/// `allow_negative_numbers` on the command, so a numeric parameter needs
/// nothing here. Only the four coordinate pairs above, not themselves
/// numbers, do.
fn takes_hyphen_leading_value(param: &spec::Parameter) -> bool {
    HYPHEN_LEADING_VALUE_PARAMS.contains(&param.name.as_str())
}

fn build_operation_command(op: &spec::Operation) -> Command {
    // The marker goes in the one-line `about`, which is what a service's
    // help *lists*: someone choosing between commands sees it before running
    // one. `--schema` leaves the spec's summary as written and reports the
    // deprecation as a field of its own instead, so a program does not have
    // to parse prose to find it.
    let about = if op.deprecated {
        format!("[deprecated] {}", op.summary)
    } else {
        op.summary.clone()
    };

    let mut cmd = Command::new(op.command_name().to_string())
        .about(about)
        .arg_required_else_help(false)
        // Longitudes and latitudes are half negative. Without this clap
        // reads `-74.0` as a cluster of short flags and rejects the command,
        // which made every operation taking a coordinate unusable west of
        // Greenwich or south of the equator — as a positional and as a flag
        // value alike.
        .allow_negative_numbers(true);

    for alias in &op.aliases {
        cmd = cmd.visible_alias(*alias);
    }
    for alias in &op.hidden_aliases {
        cmd = cmd.alias(alias.clone());
    }

    if let Some(desc) = &op.description {
        // Marked here too, not only in `about`. Clap renders `long_about`
        // for `--help` and `about` for `-h`, so a command with a spec
        // description — all 54 of them have one — showed the marker in its
        // service's listing and nowhere in its own help, which is where
        // someone looks once a command seems suspect.
        cmd = cmd.long_about(if op.deprecated {
            format!("[deprecated] {desc}")
        } else {
            desc.clone()
        });
    }

    for param in &op.path_params {
        let mut arg = Arg::new(param.arg_name.clone())
            .required(true)
            .allow_hyphen_values(takes_hyphen_leading_value(param))
            .help(help_text(param));

        // Positionals get the same treatment as flags: clap renders the
        // possible values into the help itself, so the hand-rolled suffix
        // that used to live here is gone with the guesswork it invited.
        if !param.enum_values.is_empty() {
            arg = arg.value_parser(PossibleValuesParser::new(&param.enum_values));
        } else if let Some(kind) = param.numeric {
            arg = arg.value_parser(numeric_parser(kind));
        } else if HYPHEN_LEADING_VALUE_PARAMS.contains(&param.name.as_str()) {
            arg = arg.value_parser(hyphen_leading_value_parser());
        }

        cmd = cmd.arg(arg);
    }

    for param in &op.query_params {
        let arg = if param.is_boolean {
            Arg::new(param.arg_name.clone())
                .long(param.arg_name.clone())
                .action(ArgAction::SetTrue)
                .help(help_text(param))
        } else {
            let mut arg = Arg::new(param.arg_name.clone())
                .long(param.arg_name.clone())
                .required(param.required)
                .allow_hyphen_values(takes_hyphen_leading_value(param))
                .help(help_text(param));

            // The spec says what a value may be; until now that only reached
            // the help text, so a typo travelled to the API and came back as
            // whatever that endpoint says about bad input — often a 404 that
            // blames the wrong thing entirely.
            if !param.enum_values.is_empty() {
                arg = arg.value_parser(PossibleValuesParser::new(&param.enum_values));
            } else if let Some(kind) = param.numeric {
                arg = arg.value_parser(numeric_parser(kind));
            } else if HYPHEN_LEADING_VALUE_PARAMS.contains(&param.name.as_str()) {
                arg = arg.value_parser(hyphen_leading_value_parser());
            }

            arg
        };

        cmd = cmd.arg(arg);
    }

    if let Some(body) = &op.body {
        // Two flags, split by what the spec says the body is. JSON operations
        // keep `--data` exactly as before; the three that want bytes get
        // `--file` instead, because `--data` could only ever have sent them
        // `application/json` and been rejected for it.
        // A text body is still typed on the command line, so it belongs to
        // `--data` rather than `--file` — `starFile`'s whole body is the word
        // `true`.
        let text_body = body.text_content_type();
        let takes_data = body.accepts_json() || text_body.is_some();
        if takes_data {
            cmd = cmd.arg(
                Arg::new("data")
                    .long("data")
                    .short('d')
                    .help(match text_body {
                        Some(content_type) => format!(
                            "Request body, sent verbatim as {content_type}. \
                             `@<path>` reads a file, `@-` reads stdin"
                        ),
                        None => "Request body as a JSON string, or `@<path>` to read a file \
                                 and `@-` to read stdin"
                            .to_string(),
                    }),
            );
        }

        if let Some(content_type) = body.file_content_type() {
            let mut arg = Arg::new("file").long("file").value_name("PATH");

            arg = if body.is_multipart() {
                // The spec names the form field; repeating the flag is what
                // makes a batch a batch.
                let field = body.multipart_field.as_deref().unwrap_or("file");
                arg.action(ArgAction::Append).help(format!(
                    "File to upload, repeatable, sent as multipart field `{field}`"
                ))
            } else {
                arg.help(format!(
                    "File whose bytes are sent as the request body ({content_type})"
                ))
            };

            // Only when both exist: clap asserts that a conflict names a real
            // argument, so declaring one against an absent `--data` panics.
            if takes_data {
                arg = arg.conflicts_with("data");
            }

            cmd = cmd.arg(arg);
        }
    }

    // Only where it can mean something. A `GET` has no plan worth previewing,
    // and the same reasoning that keeps unusable operations off the command
    // surface entirely keeps this flag off the commands it would do nothing
    // for — `--help` should not list an option that is a no-op.
    if op.is_mutating() {
        cmd = cmd.arg(executor::dry_run_arg(executor::DRY_RUN_REQUEST_HELP));
    }

    cmd
}

/// Attaches every operation in `operations` under `parent`, grouping them by
/// the segment of their command path at `depth`.
///
/// A segment one operation owns outright becomes that operation's command. A
/// segment two or more share becomes an intermediate group, filled the same
/// way one level down — which is the whole of what makes `mapbox styles draft
/// get` a `draft` command with a `get` under it, rather than a flat command
/// called `draft-get`. Every path is one segment long until
/// `spec::CLI_COMMAND_EXTENSION` says otherwise, so this is a plain loop for
/// all but one group in the current surface.
fn attach_operations<'a>(
    mut parent: Command,
    service: &str,
    operations: &[&'a spec::Operation],
    depth: usize,
) -> Command {
    // A `Vec` rather than a map: the order operations are declared in is the
    // order `--help` lists them in, and it is the specs' own.
    let mut groups: Vec<(&'a str, Vec<&'a spec::Operation>)> = vec![];
    for op in operations {
        let segment = op.command_path[depth].as_str();
        match groups.iter_mut().find(|(name, _)| *name == segment) {
            Some((_, members)) => members.push(op),
            None => groups.push((segment, vec![op])),
        }
    }

    for (segment, members) in groups {
        if members.len() == 1 && members[0].command_path.len() == depth + 1 {
            parent = parent.subcommand(build_operation_command(members[0]));
            continue;
        }

        let path = format!("{service} {}", members[0].command_path[..=depth].join(" "));
        // Clap would take the shorter path as the group's name and lose the
        // longer ones, silently. A command and a group cannot share a word.
        assert!(
            members.iter().all(|op| op.command_path.len() > depth + 1),
            "`mapbox {path}` is a command and a group of commands at once"
        );

        let mut group = Command::new(segment.to_string()).subcommand_required(true);
        if let Some(about) = spec::command_group_about(&path) {
            group = group.about(about);
        }
        parent = parent.subcommand(attach_operations(group, service, &members, depth + 1));
    }

    parent
}

fn build_service_command(svc: &ServiceSpec) -> Command {
    // `subcommand_required` alone, not paired with `arg_required_else_help`.
    // The pairing used to give a bare `mapbox styles` the full help text —
    // but only when nothing had populated the global `--token` arg. That arg
    // reads `MAPBOX_ACCESS_TOKEN`, and clap counts an env-populated arg as
    // "present" for `arg_required_else_help`'s purposes, so the exact same
    // command line showed full help with no token in the environment and a
    // bare usage error with one set — a caller had no way to know which they
    // would get. `subcommand_required` alone always errors on a missing
    // subcommand, so `report_parse_result` handles it the same way regardless
    // of what happens to be in the environment; `--help` is what shows the
    // full text now, unconditionally.
    let mut cmd = Command::new(svc.name.clone())
        .about(svc.title.clone())
        .subcommand_required(true);

    if let Some(desc) = &svc.description {
        cmd = cmd.long_about(desc.clone());
    }

    // Operations needing a scope nobody can hold are left out of the command
    // surface entirely, so they answer exactly as a mistyped name does. An
    // operation that can never succeed is not a feature to advertise, and a
    // dedicated "this is disabled" reply told a caller which scopes exist
    // without doing anything for them. See issue #9.
    let exposed: Vec<&spec::Operation> =
        svc.operations.iter().filter(|op| op.is_exposed()).collect();
    attach_operations(cmd, &svc.name, &exposed, 0)
}

fn build_app(specs: &[ServiceSpec]) -> Command {
    let mut app = Command::new("mapbox")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Mapbox API CLI — interact with Mapbox APIs from the command line")
        // See `build_service_command`: without this, `mapbox -o json` is a
        // successful parse of no command at all, and pairing it with
        // `arg_required_else_help` made a bare `mapbox` show full help or a
        // short usage error depending on whether `MAPBOX_ACCESS_TOKEN`
        // happened to be set — this alone always errors, consistently.
        .subcommand_required(true)
        .arg(
            Arg::new("token")
                .long("token")
                .short('t')
                .env(auth::CLAP_TOKEN_ENV)
                .hide_env_values(true)
                .global(true)
                .help("Mapbox access token")
                .required(false),
        )
        .arg(
            Arg::new("username")
                .long("username")
                .short('u')
                .env("MAPBOX_USERNAME")
                .global(true)
                .help("Mapbox username")
                .required(false),
        )
        .arg(
            Arg::new("profile")
                .long("profile")
                .global(true)
                .help("Named credential profile to use (default: \"default\")")
                .required(false),
        )
        .arg(
            Arg::new("use-login")
                .long("use-login")
                .action(ArgAction::SetTrue)
                .global(true)
                .help(
                    "Use credentials from `mapbox auth login`, ignoring any \
                     MAPBOX_ACCESS_TOKEN in the environment",
                ),
        )
        .arg(
            Arg::new("debug")
                .long("debug")
                .action(ArgAction::SetTrue)
                .env("MAPBOX_DEBUG")
                // See `--yes` below: without this, `MAPBOX_DEBUG=1` is a usage
                // error on every command rather than a debug flag.
                .value_parser(FalseyValueParser::new())
                .global(true)
                .help("Print request URLs to stderr for debugging"),
        )
        .arg(
            Arg::new(confirm::ARG)
                .long(confirm::ARG)
                .short(confirm::SHORT)
                .action(ArgAction::SetTrue)
                .env(confirm::ENV)
                // `SetTrue`'s own parser accepts only `true` and `false`, so
                // `MAPBOX_YES=1` — the spelling everyone reaches for, and the
                // one a CI config generator emits — was a usage error on every
                // command. Same trap `MAPBOX_OUTPUT` hit from the other side,
                // and the same standard to hold: a variable meant to make
                // scripts easier must never be able to brick the CLI. This
                // parser reads `0`, `false`, `no` and empty as no, everything
                // else as yes. Pinned by
                // `a_falsey_environment_value_is_not_a_yes`.
                .value_parser(FalseyValueParser::new())
                .global(true)
                // No manual env note: unlike `--output`, this arg really
                // does declare `.env()`, so clap appends one itself.
                .help("Assume yes: never ask before a destructive command"),
        )
        .arg(
            Arg::new(http::TIMEOUT_ARG)
                .long(http::TIMEOUT_ARG)
                .value_name("SECONDS")
                .value_parser(http::parse_timeout)
                // Deliberately not `.env()`, for the reason `--output` is not:
                // under `.env()` clap validates the variable with this same
                // parser, so `export MAPBOX_TIMEOUT=` — how a shell clears one
                // — and `MAPBOX_TIMEOUT=30s` would each be a usage error on
                // every command, including the ones needed to recover.
                // `--yes`'s `FalseyValueParser` does not rescue this one: a
                // boolean can read anything it does not recognise as one of
                // its two answers, and a duration has no such reading.
                // `http::read_timeout` reads the variable by hand instead,
                // warns about a value it cannot use, and falls back to the
                // default.
                // A value typed here is a different case and stays strict —
                // it names one flag on one command, and clap says so.
                .global(true)
                .help(
                    "Seconds to wait for one request, connection included. \
                     Defaults to 60, or 900 for an upload [env: MAPBOX_TIMEOUT=]",
                ),
        )
        .arg(
            // The arg's id is not `id`: two style operations take a path
            // parameter of that name, and clap requires ids to be unique
            // within a command. Long names do not collide with positionals,
            // so the flag can still read as `--id`.
            Arg::new(output::FILTER_ARG)
                .long("id")
                .global(true)
                .help("Show only the row with this id, from a command that returns a list"),
        )
        .arg(
            Arg::new(output::ARG)
                .long(output::ARG)
                .short('o')
                .value_parser([output::AUTO, output::TEXT, output::JSON])
                .default_value(output::AUTO)
                // Deliberately not `.env()`: clap would validate the variable
                // and turn `export MAPBOX_OUTPUT=` into a usage error on every
                // command. `Mode::from_matches` reads it leniently instead.
                .global(true)
                .help(
                    "Output format. `auto` reads stdout: a terminal gets text, \
                     a pipe or redirect gets JSON [env: MAPBOX_OUTPUT=]",
                ),
        )
        .arg(
            Arg::new(schema::ARG)
                .long(schema::ARG)
                .action(ArgAction::SetTrue)
                .global(true)
                .help(
                    "Describe the command as JSON instead of running it: its arguments, \
                     their types, and the request it would make",
                ),
        );

    // A service every one of whose operations is withheld would be a group
    // `--help` lists and `subcommand_required(true)` then refuses. None is
    // today; `spec::regroup_by_service` drops the ones left with no
    // operations at all, and this covers the other way of ending up empty.
    for svc in specs
        .iter()
        .filter(|svc| svc.operations.iter().any(|op| op.is_exposed()))
    {
        app = app.subcommand(build_service_command(svc));
    }

    // All three write to the credential store, so all three take
    // `--dry-run` — the flag is worth having only if it is on every command
    // that changes something, and "every mutating command except the ones
    // that touch your credentials" is the kind of exception that makes a
    // safety flag not worth reaching for.
    //
    // Their own wording: `logout` sends no request at all, and what a reader
    // wants to be told before any of the three is which file changes.
    const AUTH_DRY_RUN_HELP: &str =
        "Describe what this would change, then exit without changing it";
    app = app.subcommand(
        Command::new("auth")
            .about("Manage Mapbox credentials")
            .subcommand_required(true)
            .subcommand(
                Command::new("login")
                    .about("Log in to Mapbox via OAuth (opens browser)")
                    .arg(executor::dry_run_arg(AUTH_DRY_RUN_HELP)),
            )
            .subcommand(
                Command::new("logout")
                    .about("Remove stored Mapbox credentials")
                    .arg(executor::dry_run_arg(AUTH_DRY_RUN_HELP)),
            )
            .subcommand(
                Command::new("refresh")
                    .about("Force-refresh the stored access token, regardless of expiry")
                    .arg(executor::dry_run_arg(AUTH_DRY_RUN_HELP)),
            )
            .subcommand(
                // No `--dry-run`: it reads the store and reports, and its
                // `--verify` asks Mapbox about a token rather than changing
                // one. Nothing here to rehearse.
                Command::new("whoami")
                    // `status` because that is what the same command is called
                    // in every tool that has one; `whoami` first because it is
                    // what the question sounds like.
                    .visible_alias("status")
                    .about("Show which token the next command will use, and whose it is")
                    .arg(
                        Arg::new("verify")
                            .long("verify")
                            .action(ArgAction::SetTrue)
                            .help(
                                "Check the token with Mapbox, which reading it locally \
                                 cannot: a revoked token still looks perfectly valid",
                            ),
                    ),
            ),
    );

    // Before the proxy, only so the help lists the two hand-written commands
    // next to each other. It makes no request and needs no token — see the
    // dispatch arm in `run`, which is deliberately ahead of the generic
    // service arm.
    app = app.subcommand(generate_skills::command());

    // Beside `generate-skills` because the two are neighbours a reader will
    // want to tell apart: that one writes a skill describing this CLI, this
    // one installs the published Mapbox domain skills. Both write into the
    // same agent directories, which is why they share `skill_dest`.
    app = app.subcommand(agent_skills::command());

    // Between the other two hand-written leaves, so the help lists the three
    // that make no request together and in the order someone meets them.
    // What it prints is built from `app` itself, which is why nothing here
    // has to know about it: a service added by a spec sync, or a command a
    // feature flag left out, is completed or not completed by having been
    // registered above or not.
    app = app.subcommand(completion::command());

    app = app.subcommand(uninstall::command());

    // See `feature_flags`: absent from the tree, not merely hidden, when disabled.
    if feature_flags::flags::ACCOUNT_USAGE.is_enabled() {
        app = app.subcommand(account_usage::command());
    }

    app.subcommand(tilesets_cli::command())
}

/// Every command in `cmd` that can be run, paired with the path of names
/// taken to reach it.
///
/// A command that requires a subcommand is not one of them — `mapbox styles`
/// and `mapbox styles draft` both do — so the walk stops only where there is
/// nothing further to type. Clap's own `help` is not part of this CLI's
/// surface. Shared by the tests in four modules, which all have to agree on
/// what "a command" means now that one can be more than two words long.
#[cfg(test)]
fn leaf_commands<'a>(cmd: &'a Command, prefix: &[String]) -> Vec<(Vec<String>, &'a Command)> {
    let mut out = vec![];
    for child in cmd.get_subcommands().filter(|c| c.get_name() != "help") {
        let mut path = prefix.to_vec();
        path.push(child.get_name().to_string());
        if child.get_subcommands().any(|c| c.get_name() != "help") {
            out.extend(leaf_commands(child, &path));
        } else {
            out.push((path, child));
        }
    }
    out
}

/// The same walk, as the command lines a caller types: `mapbox styles draft
/// get`.
#[cfg(test)]
fn runnable_commands(app: &Command) -> Vec<String> {
    leaf_commands(app, &[])
        .into_iter()
        .map(|(path, _)| format!("mapbox {}", path.join(" ")))
        .collect()
}

/// `--use-login` with nothing to log in as. Falling back to the environment
/// here would hand over the very token the flag asked to ignore.
fn no_stored_credentials(profile: Option<&str>) -> anyhow::Error {
    anyhow::anyhow!(
        "--use-login was given, but no credentials are stored for profile `{}`. \
         Run `mapbox auth login` first.",
        profile.unwrap_or("default")
    )
}

/// Answers `--schema`, from either of the two places it can be noticed.
fn emit_schema(app: &Command, specs: &[ServiceSpec], matches: &ArgMatches) -> ExitCode {
    let mode = Mode::from_matches(matches);
    match schema::emit(mode, app, specs, matches) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            output::emit_error(mode, &e);
            ExitCode::FAILURE
        }
    }
}

/// The process, either of the two things it can be.
///
/// Almost always a command. The exception is the update-check refresher,
/// which this binary re-executes as a detached child of itself — it parses no
/// command line, loads no specs and prints nothing, so it is answered before
/// any of that happens. See `update_check` for why it is a mode rather than a
/// hidden subcommand.
///
/// The notice is the last thing that happens, and deliberately outside
/// `cli()`: it belongs to every way out, `--version` and a usage error
/// included, and it must come after the result and any error have already
/// been rendered. It cannot change the exit code, which is computed before it
/// runs and returned after.
fn main() -> ExitCode {
    if update_check::is_refresh_child() {
        return update_check::run_refresh_child();
    }

    let code = cli();
    update_check::notify();
    code
}

/// Every failure leaves through here, so that one `--output` decision covers
/// results and errors alike. `run` does the work; `cli` only chooses how
/// what comes back is rendered.
fn cli() -> ExitCode {
    // Kept whole for the pre-parse fallback: `escape_passthrough_args`
    // rewrites the line for clap, and a failure needs to see what the caller
    // actually typed.
    let raw_argv: Vec<std::ffi::OsString> = std::env::args_os().collect();

    let specs: Vec<ServiceSpec> = match spec::effective_services() {
        Ok(specs) => specs,
        // Too early for `--output`: the command line has not been parsed, and
        // cannot be until the specs it is parsed against exist.
        Err(e) => {
            output::emit_error(Mode::early(&raw_argv), &e);
            return ExitCode::FAILURE;
        }
    };

    let app = build_app(&specs);
    let argv = tilesets_cli::escape_passthrough_args(&app, raw_argv.clone());
    // Parsing consumes the tree, and `--schema` still has to read it
    // afterwards — so the parse gets the copy and `app` stays whole.
    let matches = match app.clone().try_get_matches_from(argv.clone()) {
        Ok(matches) => matches,
        // The ordinary way `--schema` arrives: as a line clap has just
        // refused, because naming a command is not the same as supplying
        // what running it would need. `schema::requested` offers the line to
        // a copy of the tree that requires nothing, and lets clap — not a
        // scan of argv — say whether `--schema` was really what was written.
        Err(e) => match schema::requested(&app, argv) {
            Some(matches) => return emit_schema(&app, &specs, &matches),
            None => return report_parse_result(e, &raw_argv),
        },
    };

    // Before anything resolves a token: describing a command makes no
    // request, and asking for a login first would be a poor way to answer a
    // question about what a login is for.
    if matches.get_flag(schema::ARG) {
        return emit_schema(&app, &specs, &matches);
    }

    let mode = Mode::from_matches(&matches);
    match run(&app, &specs, &matches, mode) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            output::emit_error(mode, &e);
            ExitCode::FAILURE
        }
    }
}

/// Renders whatever clap returned instead of a parse.
///
/// `--help` and `--version` arrive here as errors that are not failures, and
/// keep clap's own rendering — nobody wants their help text as a JSON string.
/// A real usage error is a failure a caller may need to distinguish, so under
/// `json` it takes the same shape as every other error. `MissingSubcommand`
/// gets that treatment in text mode too — see the comment below.
///
/// The mode cannot come from the parse that just failed, so `Mode::early`
/// reads `--output` off argv itself — an explicit choice has to survive the
/// error that makes it matter most.
fn report_parse_result(err: clap::Error, raw_argv: &[std::ffi::OsString]) -> ExitCode {
    let err = drop_subcommand_from_short_circuit_usage(err);

    // Clap uses 2 for a usage error and 0 for help/version; preserving that
    // is more useful to a caller than flattening everything to 1.
    let code = u8::try_from(err.exit_code()).unwrap_or(1);
    let mode = Mode::early(raw_argv);

    // Branch on the kind, not on `use_stderr`. Clap renders
    // `DisplayHelpOnMissingArgumentOrSubcommand` as the entire help text and
    // still marks it stderr-bound, so treating it like a usage error made the
    // "message" the command's own about line — `mapbox styles` reported the
    // error as "Mapbox Styles API" and dropped the list of operations.
    let is_help = matches!(
        err.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    );

    // `MissingSubcommand` is exactly the case the code below already builds a
    // short message and a `--help` suggestion for; that used to run only
    // under `json`, so a bare `mapbox` in a terminal got clap's raw dump —
    // the error paragraph, a repeated usage line, and a `--help` hint that
    // says nothing the message above it didn't. Text mode deserves the same
    // one-line-plus-suggestion treatment json already gets.
    if is_help || (!mode.is_json() && err.kind() != ErrorKind::MissingSubcommand) {
        let _ = err.print();
        return ExitCode::from(code);
    }

    // Clap's rendering is an error paragraph, then a blank line, then usage
    // and a hint that are help for a reader who is not going to be one here.
    // Take the whole paragraph, not just its first line: a missing-argument
    // error puts "the following required arguments were not provided:" on
    // line one and the arguments themselves on the lines after it, so a
    // first-line message would name none of them.
    let rendered = err.render().to_string();
    let paragraph: Vec<&str> = rendered
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
        .map(str::trim)
        .collect();
    let joined = paragraph.join(" ");
    let message = joined.trim().trim_start_matches("error:").trim();
    let message = if message.is_empty() {
        "Invalid command line"
    } else {
        message
    };

    // Clap catches a missing subcommand before `run` ever sees it, so give it
    // the code `run`'s own guard uses. A caller that forgot the operation and
    // a caller that misspelled a flag want to react differently.
    let error_code = match err.kind() {
        ErrorKind::MissingSubcommand => "missing_subcommand",
        _ => "usage",
    };

    let error = CliError::new(error_code, message).with_remedy(match err.kind() {
        // The message lists the subcommands, but only for the level that was
        // typed; `--help` is what shows what each of them takes.
        ErrorKind::MissingSubcommand => {
            Remedy::default().with_action(help_for_missing_subcommand(message, raw_argv))
        }
        // Clap rejected the line for a reason of its own — a misspelled flag,
        // a value that would not parse. There is no command that answers
        // that, and inventing one would be worse than the message alone.
        _ => Remedy::default(),
    });

    output::emit_error(mode, &error.into());
    ExitCode::from(code)
}

/// The subcommand placeholder clap ends a usage line with when one is still
/// required. Nothing here sets `subcommand_value_name`, so this is clap's own
/// default spelling.
const SUBCOMMAND_PLACEHOLDER: &str = " <COMMAND>";

/// Takes `<COMMAND>` back off the usage line of a suggestion that cannot take one.
///
/// On an unknown argument clap composes the usage line by pretending the
/// caller typed the suggestion — its parser adds the suggested arg to the
/// matches "to build a proper usage string" — and then appends whatever is
/// still required. `subcommand_required(true)` makes that `<COMMAND>`, and
/// for `--version` and `--help` the result contradicts the tip printed two
/// lines above it: `mapbox --versiomn` suggested `--version` and then said
/// `Usage: mapbox --version <COMMAND>`, which reads as though the suggestion
/// were a prefix to combine with a subcommand. It is not — both flags
/// short-circuit the parse, `mapbox --version` alone is the whole command,
/// and these are the same two flags the output contract already exempts
/// because clap renders them itself.
///
/// Only those two. Every other suggestion composes a line that is true —
/// `mapbox --token <token> styles get` is a real command line — and a
/// missing subcommand or a rejected value still needs its usage line whole,
/// so the kind and the suggestion are both checked before anything is cut.
///
/// This edits clap's rendering rather than replacing it, so the line keeps
/// its styling and any flag the caller really did type. If upstream stops
/// ending the line with the placeholder the guard simply fails and clap's
/// line stands: a line that overpromises is a smaller loss than one this
/// function has mangled.
fn drop_subcommand_from_short_circuit_usage(mut err: clap::Error) -> clap::Error {
    if err.kind() != ErrorKind::UnknownArgument {
        return err;
    }

    // The suggestion is the flag the usage line was composed around, so it —
    // not argv — is what decides whether that line can be true. A typo close
    // to no flag at all gets no suggestion, and no correction either.
    let short_circuits = matches!(
        err.get(ContextKind::SuggestedArg),
        Some(ContextValue::String(flag)) if flag == "--version" || flag == "--help"
    );
    if !short_circuits {
        return err;
    }

    let corrected = {
        let Some(ContextValue::StyledStr(usage)) = err.get(ContextKind::Usage) else {
            return err;
        };
        // `StyledStr` carries its styling as ANSI inside the string, so
        // pushing the trimmed rendering back into a new one round-trips the
        // styling exactly. The placeholder is plain text at the very end,
        // after every styled span, which is what makes it safe to cut.
        let rendered = usage.ansi().to_string();
        let Some(without_subcommand) = rendered.strip_suffix(SUBCOMMAND_PLACEHOLDER) else {
            return err;
        };
        let mut corrected = StyledStr::new();
        corrected.push_str(without_subcommand);
        corrected
    };
    err.insert(ContextKind::Usage, ContextValue::StyledStr(corrected));
    err
}

/// The command clap says needs a subcommand, as something to run.
///
/// Clap's message names it in quotes — `'mapbox styles' requires a
/// subcommand` — and that is the only place the path can be read from here:
/// the parse failed, so there are no matches, and argv cannot be split into
/// command and flags without re-implementing the parser. A phrasing change
/// upstream costs the suggestion and nothing else.
///
/// The program name is taken from argv rather than written down as
/// `"mapbox"`, because clap takes it from there too: on Windows the same
/// message reads `'mapbox.exe' requires a subcommand`, and a hardcoded name
/// silently dropped the suggestion on the one platform that spells it
/// differently. Reading argv also keeps a renamed or aliased binary
/// suggesting a command that exists under the name it was actually invoked
/// as.
fn help_for_missing_subcommand(message: &str, argv: &[std::ffi::OsString]) -> Option<String> {
    // The file name, not the whole path: `target/debug/mapbox` is argv[0]
    // here and `mapbox` is what clap prints.
    let program = std::path::Path::new(argv.first()?).file_name()?.to_str()?;

    let (_, after_quote) = message.split_once('\'')?;
    let (path, _) = after_quote.split_once('\'')?;
    let path = path.trim();

    // Quoted text from anywhere else in a message is not a command line.
    (path == program || path.starts_with(&format!("{program} "))).then(|| format!("{path} --help"))
}

/// The command as it was typed, minus `mapbox`: `styles get`,
/// `styles draft get`, `auth whoami`, `tilesets-cli`.
///
/// Down to the leaf, which is two levels for most commands and three for a
/// nested one (`styles draft get`). `tilesets-cli` is answered before the
/// walk starts rather than walked: clap hands its forwarded arguments back as
/// subcommands of their own, and the deprecation table would be asked about
/// `tilesets-cli upload` and find nothing.
fn typed_path(matches: &ArgMatches) -> Option<String> {
    let (top, rest) = matches.subcommand()?;
    if top == tilesets_cli::COMMAND {
        return Some(top.to_string());
    }

    let mut path = vec![top.to_string()];
    let mut current = rest;
    while let Some((name, sub)) = current.subcommand() {
        path.push(name.to_string());
        current = sub;
    }
    Some(path.join(" "))
}

/// `app` is the built tree, which `generate-skills` describes. It is the same
/// copy `--schema` reads: parsing consumed a clone, so this one is still
/// whole.
fn run(app: &Command, specs: &[ServiceSpec], matches: &ArgMatches, mode: Mode) -> Result<()> {
    let debug = matches.get_flag("debug");
    let assume_yes = matches.get_flag(confirm::ARG);
    let use_login = matches.get_flag("use-login");
    let profile = matches.get_one::<String>("profile").map(String::as_str);
    auth::validate_profile(profile)?;

    // Before anything is sent, asked about or previewed: this is about the
    // command that was typed, not about how it turns out. `--dry-run` warns
    // too — a command on its way out is exactly what a preview should
    // mention.
    if let Some(path) = typed_path(matches) {
        deprecation::warn_command(&path);
    }

    match matches.subcommand() {
        // Ahead of the generic service arm, which would otherwise catch this
        // as an unknown service. `tilesets` makes its own HTTP calls, so the
        // only thing it needs from us is a token — resolved the way every
        // other command resolves one, then injected into its environment
        // rather than its argv (see `tilesets_cli`'s module docs for why).
        Some((tilesets_cli::COMMAND, tilesets_matches)) => {
            tilesets_cli::warn_output_ignored(matches);
            tilesets_cli::warn_yes_ignored(matches);
            let token = tilesets_cli::token_for_child(matches, use_login, || {
                auth::load_fresh_credentials(debug, profile).map(|c| c.access_token)
            });
            if use_login && token.is_none() {
                return Err(no_stored_credentials(profile));
            }
            if token.is_none() {
                // Falling through to whatever the environment holds. If that
                // shadows a login for a different account, say so: the
                // Tilesets API reports a wrong-account token as a bare
                // "Not found", which points nowhere near the cause.
                auth::warn_if_environment_token_shadows_login(
                    profile,
                    "mapbox --use-login tilesets-cli ...",
                );
            }
            let forwarded = tilesets_cli::forwarded_args(tilesets_matches);
            tilesets_cli::warn_about_misplaced_globals(&forwarded);
            tilesets_cli::run(&forwarded, token, debug)?
        }
        // Also ahead of the generic service arm, which would otherwise reach
        // its `expect("unknown service")` and panic. Nothing is loaded for it:
        // it reads the command tree already in memory and writes files, makes
        // no request, and needs no token — which is the point, since an agent
        // asking what this CLI can do has not logged in yet.
        Some((generate_skills::COMMAND, skills_matches)) => {
            generate_skills::run(app, specs, skills_matches, mode)?
        }
        // Ahead of the generic service arm for the same reason as its
        // neighbours: it makes no Mapbox request and needs no token. The one
        // request it does make is to GitHub for a public tarball.
        Some((agent_skills::COMMAND, skills_matches)) => agent_skills::run(
            skills_matches,
            agent_skills::RunFlags { debug, assume_yes },
            mode,
        )?,
        // Ahead of the generic service arm for the same reason again, and
        // handed `app` for the same reason `generate-skills` is: the script
        // it prints is a rendering of the command tree already in memory.
        // No token, no request, and no credential load — a shell asking what
        // this CLI can complete has not logged in either.
        Some((completion::COMMAND, completion_matches)) => {
            completion::warn_output_ignored(matches);
            completion::run(app, completion_matches)?
        }
        // Also ahead of the generic service arm, for the same reason: this
        // makes no request and needs no token, it just deletes a file on
        // disk.
        Some((uninstall::COMMAND, uninstall_matches)) => {
            if executor::wants_dry_run(uninstall_matches) {
                uninstall::describe_plan(mode)?
            } else {
                uninstall::run(assume_yes, mode)?
            }
        }
        // Token resolution mirrors the service arm below, minus path
        // placeholders, a request body, and `--dry-run` — this GET always refreshes.
        Some((account_usage::COMMAND, usage_matches)) => {
            let stored_creds = auth::load_fresh_credentials(debug, profile);
            let token: Option<String> = if use_login {
                auth::typed_token(matches)
            } else {
                matches.get_one::<String>("token").cloned()
            }
            .or_else(|| stored_creds.as_ref().map(|c| c.access_token.clone()));

            if use_login && token.is_none() {
                return Err(no_stored_credentials(profile));
            }

            account_usage::run(
                usage_matches,
                token.as_deref(),
                debug,
                http::requested(matches),
                mode,
            )
            .map_err(|e| auth::with_auth_fix(e, matches, use_login, profile))?
        }
        // Deliberately no credential load here: `login` starts a fresh OAuth
        // flow, `logout` is about to delete the file, and `force_refresh`
        // re-reads under its own lock. Loading would force an unwanted network
        // round-trip on all three. `whoami` is the one that does read them, and
        // does it through `load_credentials` — reporting an expiry must not be
        // what spends the single-use refresh token.
        Some(("auth", auth_matches)) => match auth_matches.subcommand() {
            Some((action, action_matches)) if executor::wants_dry_run(action_matches) => {
                auth::describe_plan(action, profile, mode)?
            }
            Some(("login", _)) => auth::login(debug, profile, mode)?,
            Some(("logout", _)) => auth::logout(profile, mode)?,
            Some(("refresh", _)) => auth::force_refresh(debug, profile, mode)?,
            // Handed the top-level matches, not its own: `--token` is global,
            // and which of the two things it can mean is the answer here.
            Some(("whoami", whoami_matches)) => auth::whoami(
                matches,
                whoami_matches.get_flag("verify"),
                use_login,
                debug,
                profile,
                mode,
            )?,
            _ => unreachable!("`auth` sets subcommand_required(true)"),
        },
        Some((svc_name, svc_matches)) => {
            let svc = specs
                .iter()
                .find(|s| s.name == svc_name)
                .expect("unknown service");

            // Down to the leaf, since a command path may be more than one
            // segment long — `styles draft get` is three matches deep. Every
            // intermediate group sets `subcommand_required(true)`, so the
            // walk can only stop on an operation.
            let mut command_path: Vec<String> = vec![];
            let mut op_matches = svc_matches;
            while let Some((name, sub)) = op_matches.subcommand() {
                command_path.push(name.to_string());
                op_matches = sub;
            }

            if command_path.is_empty() {
                // `subcommand_required` should have caught this; saying so
                // beats the silent exit 0 that a gap here used to produce.
                return Err(CliError::new(
                    "missing_subcommand",
                    format!(
                        "`mapbox {svc_name}` needs an operation. Run `mapbox {svc_name} --help`."
                    ),
                )
                // Same suggestion the clap path attaches, for the same
                // code — see `help_for_missing_subcommand`.
                .with_remedy(
                    Remedy::default().with_action(Some(format!("mapbox {svc_name} --help"))),
                )
                .into());
            }

            // Read before the credentials are touched, which is the whole
            // reason the operation is resolved first: refreshing spends a
            // single-use refresh token and rewrites the credentials file, and
            // a command that promised to change nothing must not do that.
            let op = svc
                .operations
                .iter()
                .find(|o| o.command_path == command_path)
                .expect("unknown operation");

            // Ahead of the credentials for the same reason `dry_run` is read
            // ahead of them: `load_fresh_credentials` spends a single-use
            // refresh token and rewrites the credentials file, so a notice
            // printed after it is a notice printed after the CLI has already
            // sent a request and changed state. It also has to precede the
            // `--use-login` bail below, or the one caller whose token cannot
            // be resolved is the one caller never told the endpoint is going
            // away.
            deprecation::warn_endpoint(op, &op.command());

            let dry_run = executor::wants_dry_run(op_matches);

            // Resolve token: explicit flag/env takes priority, then stored
            // credentials (auto-refreshed, except under `--dry-run` — a stale
            // token still shows the request that would be sent).
            let stored_creds = if dry_run {
                auth::load_credentials(profile)
            } else {
                auth::load_fresh_credentials(debug, profile)
            };
            // `get_one` folds in the `MAPBOX_ACCESS_TOKEN` fallback declared on
            // the arg; `--use-login` asks for that fallback to be skipped, so
            // only a typed flag may still outrank the stored credentials.
            let token: Option<String> = if use_login {
                auth::typed_token(matches)
            } else {
                matches.get_one::<String>("token").cloned()
            }
            .or_else(|| stored_creds.as_ref().map(|c| c.access_token.clone()));

            if use_login && token.is_none() {
                return Err(no_stored_credentials(profile));
            }
            let username: Option<String> = matches
                .get_one::<String>("username")
                .cloned()
                .or_else(|| stored_creds.as_ref().and_then(|c| c.username.clone()));

            executor::execute(
                op,
                op_matches,
                token.as_deref(),
                username.as_deref(),
                executor::RunFlags {
                    debug,
                    assume_yes,
                    dry_run,
                    // Read here rather than beside the other globals at the
                    // top, so that `MAPBOX_TIMEOUT` is only looked at by a
                    // command it can apply to. A warning about an unreadable
                    // one in front of `generate-skills`, which sends nothing,
                    // would be a note about a request that is not happening.
                    timeout: http::requested(matches),
                },
                mode,
            )
            // The executor does not know where its token came from, and that
            // is what decides what to advise about a 401.
            .map_err(|e| auth::with_auth_fix(e, matches, use_login, profile))?;
        }
        None => {
            return Err(CliError::new(
                "missing_subcommand",
                "No command given. Run `mapbox --help` for the list of commands.",
            )
            .with_remedy(Remedy::default().with_action(Some("mapbox --help".to_string())))
            .into())
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Half the world has a negative longitude. Clap reads `-74.0` as a
    /// cluster of short flags unless told otherwise, which made every
    /// operation taking a coordinate unusable west of Greenwich — as a
    /// positional and as a flag value alike.
    #[test]
    fn a_negative_coordinate_is_a_value_not_a_flag() {
        let specs = bundled_specs();
        let app = build_app(&specs);

        let positional = app.clone().try_get_matches_from([
            "mapbox",
            "tilequery",
            "get",
            "mapbox.mapbox-streets-v8",
            "-74.0",
            "40.7",
        ]);
        assert!(
            positional.is_ok(),
            "{:?}",
            positional.err().map(|e| e.to_string())
        );

        let flag_value = app.try_get_matches_from([
            "mapbox",
            "geocoder",
            "reverse-geocode",
            "--longitude",
            "-74.0",
            "--latitude",
            "40.7",
        ]);
        assert!(
            flag_value.is_ok(),
            "{:?}",
            flag_value.err().map(|e| e.to_string())
        );
    }

    /// `allow_negative_numbers` only recognizes a value that is *itself* a
    /// bare number, so a comma-joined pair like `-121.9,37.4` — what
    /// `--proximity`, `--bbox`, `--near` and `--origin` all take — still hit
    /// the cluster-of-short-flags misparse the test above fixed for a lone
    /// coordinate. Reported against `search category`, but the arg is built
    /// the same way on every service that takes one of the four.
    #[test]
    fn a_comma_joined_negative_coordinate_is_a_value_not_a_flag() {
        let specs = bundled_specs();

        for argv in [
            ["mapbox", "search", "category", "coffee", "--proximity"].as_slice(),
            ["mapbox", "search", "forward", "--q", "coffee", "--bbox"].as_slice(),
            [
                "mapbox",
                "geocoder",
                "forward-geocode",
                "--q",
                "x",
                "--proximity",
            ]
            .as_slice(),
        ] {
            let mut argv = argv.to_vec();
            argv.push("-121.90662,37.42827");
            let matches = build_app(&specs).try_get_matches_from(&argv);
            assert!(
                matches.is_ok(),
                "{argv:?}: {:?}",
                matches.err().map(|e| e.to_string())
            );
        }
    }

    /// The other half of the rule above: `allow_hyphen_values` also switches
    /// off the check that catches a forgotten value, so granting it to every
    /// parameter turned `--q --limit` into a search for the literal string
    /// `--limit` — a confident wrong answer on every service, in place of a
    /// usage error. Only a numeric parameter and the four coordinate-shaped
    /// names get it; a plain string keeps the check.
    #[test]
    fn a_forgotten_value_on_a_plain_string_flag_is_a_usage_error() {
        let specs = bundled_specs();

        for argv in [
            ["mapbox", "search", "forward", "--q", "--limit"].as_slice(),
            [
                "mapbox", "search", "forward", "--q", "x", "--types", "--limit",
            ]
            .as_slice(),
            ["mapbox", "geocoder", "forward-geocode", "--q", "--limit"].as_slice(),
        ] {
            let matches = build_app(&specs).try_get_matches_from(argv);
            assert!(
                matches.is_err(),
                "{argv:?} parsed instead of erroring: {:?}",
                matches.ok().map(|_| "ok")
            );
        }
    }

    /// `HYPHEN_LEADING_VALUE_PARAMS` widens `allow_hyphen_values` back on for
    /// four names, and the test above only proves the check still applies to
    /// a *different* flag. Left on its own, `--near --limit` would take
    /// `--limit` as free-form place text — the same silent-wrong-answer
    /// failure the module docs call out for `--q`, just narrowed from every
    /// parameter down to these four. `hyphen_leading_value_parser` closes it
    /// back up for values that start with a *second* hyphen, which no
    /// coordinate pair or place name does.
    #[test]
    fn a_forgotten_value_on_a_coordinate_shaped_flag_is_still_a_usage_error() {
        let specs = bundled_specs();

        for argv in [
            [
                "mapbox", "search", "forward", "--q", "x", "--near", "--limit",
            ]
            .as_slice(),
            [
                "mapbox", "search", "forward", "--q", "x", "--bbox", "--limit",
            ]
            .as_slice(),
            [
                "mapbox",
                "search",
                "forward",
                "--q",
                "x",
                "--proximity",
                "--limit",
            ]
            .as_slice(),
            [
                "mapbox", "search", "forward", "--q", "x", "--origin", "--limit",
            ]
            .as_slice(),
        ] {
            let matches = build_app(&specs).try_get_matches_from(argv);
            assert!(
                matches.is_err(),
                "{argv:?} parsed instead of erroring: {:?}",
                matches.ok().map(|_| "ok")
            );
        }
    }

    /// A numeric parameter used to grant `allow_hyphen_values` too, for a
    /// negative number that `allow_negative_numbers` already covers. What it
    /// actually bought: `--limit -o json` let `-o` through as `--limit`'s
    /// value, then blamed the *next* token, `json`, instead of `--limit`.
    #[test]
    fn a_forgotten_value_on_a_numeric_flag_is_a_usage_error() {
        let specs = bundled_specs();
        let matches = build_app(&specs).try_get_matches_from([
            "mapbox",
            "accounts",
            "list-tokens",
            "-u",
            "me",
            "--limit",
        ]);
        let err = matches
            .expect_err("no value for --limit should not parse")
            .to_string();
        assert!(
            err.contains("--limit"),
            "error should name --limit, got: {err}"
        );

        // The full failing case from review: a trailing `-o json` must not
        // change which flag the error names.
        let matches = build_app(&specs).try_get_matches_from([
            "mapbox",
            "accounts",
            "list-tokens",
            "-u",
            "me",
            "--limit",
            "-o",
            "json",
        ]);
        let err = matches
            .expect_err("-o should not be swallowed as --limit's value")
            .to_string();
        assert!(
            err.contains("--limit"),
            "error should name --limit, not a later token, got: {err}"
        );

        // A genuine negative number must still work without
        // `allow_hyphen_values`: `allow_negative_numbers` covers it.
        assert!(build_app(&specs)
            .try_get_matches_from([
                "mapbox",
                "accounts",
                "list-tokens",
                "-u",
                "me",
                "--limit",
                "-5",
            ])
            .is_ok());
    }

    /// The long-flag half of this guard was already pinned above; a short
    /// flag took a value just as readily and reported no error at all —
    /// `--proximity -o` sent a literal `-o` to the API.
    #[test]
    fn a_short_global_flag_is_not_swallowed_as_a_coordinate_value() {
        let specs = bundled_specs();
        for short in ["-t", "-u", "-o", "-y", "-d"] {
            let argv = [
                "mapbox",
                "search",
                "forward",
                "--q",
                "x",
                "--proximity",
                short,
            ];
            let matches = build_app(&specs).try_get_matches_from(argv);
            assert!(
                matches.is_err(),
                "{argv:?} parsed instead of erroring: {:?}",
                matches.ok().map(|_| "ok")
            );
        }
    }

    /// A list of today's five short flags was tried first; a sixth added
    /// later (`-p`, say) would have slipped through it the way `-o` used to.
    /// `looks_like_a_flag` checks the shape instead, so it needs no such
    /// list — pinned against a letter that is not one of the five.
    #[test]
    fn a_short_flag_not_on_the_list_is_also_not_swallowed() {
        let specs = bundled_specs();
        for argv in [
            [
                "mapbox",
                "search",
                "forward",
                "--q",
                "x",
                "--proximity",
                "-p",
            ],
            [
                "mapbox",
                "search",
                "forward",
                "--q",
                "x",
                "--proximity",
                "-x",
            ],
        ] {
            let matches = build_app(&specs).try_get_matches_from(argv);
            assert!(
                matches.is_err(),
                "{argv:?} parsed instead of erroring: {:?}",
                matches.ok().map(|_| "ok")
            );
        }
    }

    /// The guard above must not cost real coordinate-shaped values their
    /// ability to start with `-`: a lone negative number, a comma-joined
    /// pair, and `bbox`'s four-number form all still need to parse.
    #[test]
    fn a_real_coordinate_value_still_parses_despite_the_flag_guard() {
        let specs = bundled_specs();
        for argv in [
            [
                "mapbox",
                "search",
                "forward",
                "--q",
                "x",
                "--proximity",
                "-121.90662,37.42827",
            ],
            ["mapbox", "search", "forward", "--q", "x", "--near", "-74.0"],
            [
                "mapbox",
                "search",
                "forward",
                "--q",
                "x",
                "--bbox",
                "-87.3857,34.3648,-80.3849,38.4895",
            ],
        ] {
            let matches = build_app(&specs).try_get_matches_from(argv);
            assert!(
                matches.is_ok(),
                "{argv:?}: {:?}",
                matches.err().map(|e| e.to_string())
            );
        }
    }

    /// Specs come from a sibling repo that grows new query parameters without
    /// asking. One named after a global — `output`, `token`, `profile` — is a
    /// clap conflict, and clap only notices while parsing, so it would ship as
    /// a panic on the first run of a freshly synced command rather than as a
    /// CI failure on the sync that caused it.
    /// Withheld for what they do, not for what the platform allows — the
    /// only guard against someone "fixing" the omission is a test that says
    /// it was deliberate.
    /// The spec says `starFile` takes `application/json`; the service
    /// answers `400 Must be plaintext true or false` to exactly that, and
    /// accepts the same `true` as `text/plain`. The override has to survive
    /// spec parsing or the command cannot work at all.
    #[test]
    fn star_file_sends_the_media_type_the_service_accepts() {
        let specs = bundled_specs();
        let Some(star) = specs
            .iter()
            .flat_map(|svc| &svc.operations)
            .find(|op| op.command_name() == "star-file")
        else {
            // `starFile` is disabled and not exposed as a command as of
            // this writing, so it's stripped from the bundled specs
            // entirely — nothing to check until it's re-enabled.
            // BODY_CONTENT_TYPE_OVERRIDES keeps the override documented for
            // when that happens.
            return;
        };

        let body = star.body.as_ref().expect("it takes a body");
        assert_eq!(body.text_content_type(), Some("text/plain"));
        assert!(
            !body.accepts_json(),
            "`--data` must not re-encode the body as JSON"
        );
    }

    #[test]
    fn no_withheld_operation_is_reachable() {
        let specs = bundled_specs();
        let withheld: Vec<String> = specs
            .iter()
            .flat_map(|svc| &svc.operations)
            .filter(|op| op.is_withheld())
            .map(|op| op.command())
            .collect();
        // Every WITHHELD_OPERATIONS entry is also disabled in the
        // maintainer-only decision record right now, so stripping removes
        // them from the bundled specs before this ever sees them — an
        // empty `withheld` here means "still withheld, just
        // doubly so" (physically absent, not merely filtered), not "this
        // guard broke". If one becomes `enabled` there without a matching
        // change here, it reappears in this list and the loop below still
        // has to prove it's unreachable.

        let app = build_app(&specs);
        let reachable = runnable_commands(&app);
        for command in withheld {
            assert!(
                !reachable.contains(&format!("mapbox {command}")),
                "`mapbox {command}` is withheld but reachable"
            );
        }
    }

    /// A service's own liveness probe is not a command anyone would reach
    /// for. Excluded for a different reason from the unregistrable-scope
    /// ones, so checked separately.
    ///
    /// Every liveness probe is disabled in the maintainer-only decision
    /// record now (rasterarrays' needed a synthetic operationId first,
    /// since its spec doesn't declare one for any operation), so the
    /// vendoring step removes every one of them from the bundled specs
    /// before this test ever runs. `probes` is legitimately always empty
    /// now: physically absent, not merely filtered, which is the stronger
    /// guarantee.
    #[test]
    fn no_liveness_probe_is_reachable() {
        let specs = bundled_specs();
        let probes: Vec<String> = specs
            .iter()
            .flat_map(|svc| &svc.operations)
            .filter(|op| op.is_liveness_probe())
            .map(|op| op.command())
            .collect();

        let app = build_app(&specs);
        let reachable = runnable_commands(&app);
        for command in probes {
            assert!(
                !reachable.contains(&format!("mapbox {command}")),
                "`mapbox {command}` is a liveness probe but reachable"
            );
        }
    }

    /// Operations needing an unregistrable scope must not reach the command
    /// surface — they would otherwise advertise, in `--help` and in a
    /// distinctive error, something no caller can ever use.
    #[test]
    fn no_disabled_operation_is_reachable() {
        let specs = bundled_specs();

        let disabled: Vec<String> = specs
            .iter()
            .flat_map(|svc| &svc.operations)
            .filter(|op| op.disabled_scope.is_some())
            .map(|op| op.command())
            .collect();
        // Every current UNSUPPORTED_OPERATIONS entry is also `disabled` (or
        // `tbd`, which strips the same way) in the maintainer-only decision
        // record, so this is legitimately empty now rather than a broken guard — the
        // operations are absent from the bundled specs, not merely filtered
        // here. Nothing left to assert on `disabled` itself; the loop below
        // still holds for whatever, if anything, shows up.

        let app = build_app(&specs);
        for (path, _) in leaf_commands(&app, &[]) {
            let command = path.join(" ");
            assert!(
                !disabled.contains(&command),
                "`mapbox {command}` is disabled but reachable"
            );
        }
    }

    /// Help still shows one sentence. This is the whole of what stops the
    /// description change from rewriting every command's help — `spec.rs` now
    /// keeps the paragraph, and this is where it gets cut back.
    #[test]
    fn help_shortens_a_description_to_its_first_sentence() {
        assert_eq!(
            first_sentence("Tileset ID in the format `username.id`. Order matters."),
            "Tileset ID in the format `username"
        );
        assert_eq!(first_sentence("  padded.  rest"), "padded");
        assert_eq!(first_sentence("no full stop here"), "no full stop here");
        assert_eq!(first_sentence(""), "");
    }

    /// `MAPBOX_YES=0` has to mean no.
    ///
    /// Nothing else connects the variable to the bool `confirm::decide`
    /// receives. The integration tests can only see that no value is a *usage
    /// error*, because with no terminal nothing is asked whatever the answer
    /// would have been — so a swap to `BoolishValueParser`, or a hand-rolled
    /// "is it set at all" check, would turn `MAPBOX_YES=0` (set by someone who
    /// means "always ask me") into a yes, and deletes would go through unasked
    /// at a terminal with the suite still green.
    ///
    /// One body, and the variable is removed at the end: the environment is
    /// process-wide and these tests run in parallel.
    #[test]
    fn the_environment_variable_maps_the_way_a_reader_expects() {
        fn assume_yes_with(value: &str) -> bool {
            std::env::set_var(confirm::ENV, value);
            let matches = build_app(&[])
                .try_get_matches_from(["mapbox", "auth", "logout"])
                .expect("a hand-written command parses without the specs");
            matches.get_flag(confirm::ARG)
        }

        for no in ["0", "false", "no", "off", ""] {
            assert!(!assume_yes_with(no), "MAPBOX_YES={no:?} was read as a yes");
        }
        for yes in ["1", "true", "yes", "on"] {
            assert!(assume_yes_with(yes), "MAPBOX_YES={yes:?} was read as a no");
        }

        std::env::remove_var(confirm::ENV);
        let matches = build_app(&[])
            .try_get_matches_from(["mapbox", "auth", "logout"])
            .expect("a hand-written command parses without the specs");
        assert!(
            !matches.get_flag(confirm::ARG),
            "unset must be a no, not merely absent"
        );
    }

    #[test]
    fn no_generated_flag_shadows_a_global() {
        let specs = bundled_specs();
        let app = build_app(&specs);

        let globals: Vec<String> = app
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .flat_map(|arg| {
                // Shorts share the namespace too: this change added `-o`
                // alongside `-t` and `-u`, and only `-d` is generated today.
                let long = arg.get_long().map(|l| format!("--{l}"));
                let short = arg.get_short().map(|s| format!("-{s}"));
                [long, short]
            })
            .flatten()
            .collect();

        for (path, operation) in leaf_commands(&app, &[]) {
            for arg in operation.get_arguments() {
                let spellings = [
                    arg.get_long().map(|l| format!("--{l}")),
                    arg.get_short().map(|s| format!("-{s}")),
                ];
                for spelling in spellings.into_iter().flatten() {
                    assert!(
                        !globals.contains(&spelling),
                        "`mapbox {}` generates a {spelling} that collides with the \
                         global of the same name — rename the global, or teach spec.rs \
                         to rename the parameter",
                        path.join(" "),
                    );
                }
            }
        }
    }

    /// `--dry-run` is only worth reaching for if it is on every command that
    /// changes something — a caller who has to remember which mutations
    /// support it will not use it on any of them. So this walks the real
    /// command surface and pins the flag to the spec's own methods in both
    /// directions: present on every mutating operation, absent from every
    /// read.
    ///
    /// The reverse half matters as much. A `--dry-run` on a `GET` would be a
    /// no-op flag in that command's `--help`, and the next reader would have
    /// to run it to find out which kind it was.
    #[test]
    fn dry_run_is_offered_on_exactly_the_mutating_operations() {
        let specs = bundled_specs();
        let app = build_app(&specs);
        let mut mutating = 0usize;

        for (path, operation) in leaf_commands(&app, &[]) {
            let Some(op) = specs
                .iter()
                .filter(|spec| spec.name == path[0])
                .flat_map(|spec| &spec.operations)
                .find(|op| op.command_path == path[1..])
            else {
                // A hand-written command — `auth`, the proxy — which the
                // `auth` test below covers instead.
                continue;
            };

            let offered = operation
                .get_arguments()
                .any(|arg| arg.get_long() == Some(executor::DRY_RUN_ARG));

            assert_eq!(
                offered,
                op.is_mutating(),
                "`mapbox {}` is a {} and {} --dry-run",
                op.command(),
                op.method,
                if offered { "offers" } else { "does not offer" },
            );
            mutating += usize::from(op.is_mutating());
        }

        // A rename in `openapi-specs` that emptied this loop would otherwise
        // let every assertion above pass by never running.
        assert!(mutating > 0, "the bundled specs describe no mutations");
    }

    /// The `auth` subcommands are hand-written rather than generated, so
    /// nothing derived from the specs covers them: a new one ships with
    /// whatever it was given, and the decision about `--dry-run` is easy to
    /// not make at all.
    ///
    /// So the list is written out here rather than counted. Adding a
    /// subcommand fails this test until it appears on one side or the
    /// other, which is the point — `whoami` reads the store and reports,
    /// and its `--verify` asks Mapbox about a token rather than changing
    /// one, so it belongs with the reads.
    #[test]
    fn the_auth_subcommands_that_write_offer_dry_run() {
        const WRITES: [&str; 3] = ["login", "logout", "refresh"];
        const READS: [&str; 1] = ["whoami"];

        let specs = bundled_specs();
        let app = build_app(&specs);
        let auth = app
            .get_subcommands()
            .find(|cmd| cmd.get_name() == "auth")
            .expect("`auth` is a subcommand");

        let mut seen: Vec<&str> = vec![];
        for action in auth.get_subcommands() {
            let name = action.get_name();
            let offered = action
                .get_arguments()
                .any(|arg| arg.get_long() == Some(executor::DRY_RUN_ARG));

            if WRITES.contains(&name) {
                assert!(
                    offered,
                    "`mapbox auth {name}` changes stored credentials but offers no --dry-run",
                );
            } else if READS.contains(&name) {
                assert!(
                    !offered,
                    "`mapbox auth {name}` changes nothing, so --dry-run has nothing to say",
                );
            } else {
                panic!(
                    "`mapbox auth {name}` is new: add it to WRITES or READS above, \
                     depending on whether it changes anything"
                );
            }
            seen.push(name);
        }

        seen.sort_unstable();
        let mut expected: Vec<&str> = WRITES.iter().chain(&READS).copied().collect();
        expected.sort_unstable();
        assert_eq!(seen, expected, "auth lost a subcommand");
    }

    /// The same trap `no_generated_flag_shadows_a_global` catches, one level
    /// down. `--dry-run` is not global — it is added to the mutating
    /// operations individually — so that test cannot see it, and a spec that
    /// grows a query parameter of this name would collide with it on exactly
    /// the operations that have it. Clap notices duplicate ids while
    /// building, which means the failure would be a panic on the next sync
    /// rather than a message naming the parameter.
    #[test]
    fn no_generated_flag_shadows_dry_run() {
        for spec in bundled_specs() {
            for op in &spec.operations {
                if !op.is_mutating() {
                    continue;
                }
                for param in op.path_params.iter().chain(&op.query_params) {
                    assert_ne!(
                        param.arg_name,
                        executor::DRY_RUN_ARG,
                        "`mapbox {}` generates a --{} that collides with the dry-run \
                         flag — teach spec.rs to rename the parameter",
                        op.command(),
                        executor::DRY_RUN_ARG,
                    );
                }
            }
        }
    }

    fn bundled_specs() -> Vec<ServiceSpec> {
        spec::effective_services().expect("the bundled specs parse")
    }

    /// A spec that says `enum: [created, modified]` used to say so only in
    /// the help text, so a typo travelled to the API and came back as
    /// whatever that endpoint says about bad input — often a 404 blaming
    /// something else.
    #[test]
    fn every_spec_enum_reaches_the_parser() {
        let specs = bundled_specs();
        let app = build_app(&specs);
        let mut checked = 0usize;

        let leaves = leaf_commands(&app, &[]);
        for svc in &specs {
            for op in &svc.operations {
                let Some((_, op_cmd)) = leaves.iter().find(|(path, _)| {
                    path.first() == Some(&svc.name) && path[1..] == op.command_path
                }) else {
                    continue;
                };

                for param in op
                    .path_params
                    .iter()
                    .chain(&op.query_params)
                    .filter(|p| !p.is_boolean && !p.enum_values.is_empty())
                {
                    let arg = op_cmd
                        .get_arguments()
                        .find(|a| a.get_id() == param.arg_name.as_str())
                        .expect("parameter is built as an arg");
                    let allowed: Vec<String> = arg
                        .get_possible_values()
                        .iter()
                        .map(|v| v.get_name().to_string())
                        .collect();

                    assert_eq!(
                        allowed,
                        param.enum_values,
                        "`{}` {} does not enforce its spec enum",
                        op.command(),
                        param.arg_name
                    );
                    checked += 1;
                }
            }
        }

        // If a spec sync ever drops this to zero the loop above asserts
        // nothing — fail rather than pass vacuously.
        assert!(checked > 0, "no enum parameters left to check");
    }

    /// Every intermediate command group in the surface has a line of help.
    ///
    /// A group exists because `spec::CLI_COMMAND_EXTENSION` filed two
    /// commands under one word, and no spec describes that word — so the
    /// line is hand-written in `spec::COMMAND_GROUPS`, and a group added by
    /// a later config change would otherwise ship with a blank entry in its
    /// service's `--help`.
    #[test]
    fn every_command_group_is_described() {
        let specs = bundled_specs();
        let app = build_app(&specs);

        for (path, leaf) in leaf_commands(&app, &[]) {
            // Everything between the service and the leaf is a group.
            for depth in 2..path.len() {
                let group = path[..depth].join(" ");
                assert!(
                    spec::command_group_about(&group).is_some(),
                    "`mapbox {group}` is a command group with no line in \
                     spec::COMMAND_GROUPS saying what it holds"
                );
            }
            let _ = leaf;
        }
    }

    /// The numeric parser has to reject text while still yielding a String:
    /// the executor reads every parameter with `get_one::<String>`, and a
    /// parser that stored an `i64` would leave it finding nothing.
    #[test]
    fn the_numeric_parser_rejects_text_and_keeps_the_string() {
        let cmd = Command::new("t").arg(
            Arg::new("n")
                .long("n")
                .value_parser(numeric_parser(spec::Numeric::Integer)),
        );

        let ok = cmd
            .clone()
            .try_get_matches_from(["t", "--n", "42"])
            .expect("42 is an integer");
        assert_eq!(ok.get_one::<String>("n").map(String::as_str), Some("42"));

        assert!(cmd
            .clone()
            .try_get_matches_from(["t", "--n", "abc"])
            .is_err());

        let float = Command::new("t").arg(
            Arg::new("n")
                .long("n")
                .value_parser(numeric_parser(spec::Numeric::Float)),
        );
        assert!(float
            .clone()
            .try_get_matches_from(["t", "--n", "1.5"])
            .is_ok());
        assert!(float.try_get_matches_from(["t", "--n", "1.5.5"]).is_err());
    }

    /// The documentation link on a failure is hand-maintained — no spec
    /// declares `externalDocs` — so nothing but this stops a newly added
    /// service from shipping errors with no page to read.
    #[test]
    fn every_service_has_a_documentation_page() {
        for svc in bundled_specs() {
            assert!(
                remedy::docs_for_service(&svc.name).is_some(),
                "`{}` has no entry in SERVICE_DOCS, so its failures carry no link",
                svc.name
            );
        }
    }

    /// The other direction: a page for a service that no longer exists is a
    /// link nothing can reach, and a rename would leave both behind.
    #[test]
    fn every_documentation_page_belongs_to_a_service() {
        let services = bundled_specs();
        for service in remedy::documented_services() {
            assert!(
                services.iter().any(|svc| svc.name == service),
                "SERVICE_DOCS lists `{service}`, which is not a service"
            );
        }
    }

    /// The suggestion has to be the command that was typed, at the level it
    /// was typed at — and nothing at all when the message is not one of
    /// clap's missing-subcommand ones.
    #[test]
    fn the_help_offered_is_the_command_that_was_typed() {
        let argv = |program: &str| vec![std::ffi::OsString::from(program)];

        assert_eq!(
            help_for_missing_subcommand(
                "'mapbox styles' requires a subcommand but one was not provided \
                 [subcommands: list, create]",
                &argv("target/debug/mapbox")
            )
            .as_deref(),
            Some("mapbox styles --help")
        );
        assert_eq!(
            help_for_missing_subcommand(
                "'mapbox' requires a subcommand but one was not provided",
                &argv("mapbox")
            )
            .as_deref(),
            Some("mapbox --help")
        );

        assert_eq!(
            help_for_missing_subcommand("unexpected argument '--nonsense' found", &argv("mapbox")),
            None,
            "a quoted flag is not a command line"
        );
        assert_eq!(
            help_for_missing_subcommand("nothing quoted here", &argv("mapbox")),
            None
        );
    }

    /// The Windows build failed on exactly this: clap names the program as it
    /// was invoked, and `mapbox.exe` is not `mapbox`, so a hardcoded name
    /// dropped the suggestion on the one platform that spells it differently
    /// — silently, since the field is absent when there is nothing to say.
    #[test]
    fn the_program_name_is_whatever_argv_says_it_is() {
        let windows = help_for_missing_subcommand(
            "'mapbox.exe styles' requires a subcommand but one was not provided",
            &[std::ffi::OsString::from("mapbox.exe")],
        );
        assert_eq!(windows.as_deref(), Some("mapbox.exe styles --help"));

        // Splitting `C:\bin\mapbox.exe` down to its file name is `Path`'s
        // job and it only does it where the separator means that, so this
        // half can only run where it is true. On Unix the same string is one
        // long file name — which is why the assertion above passes a bare
        // program name rather than a fake Windows path.
        #[cfg(windows)]
        {
            let full_path = help_for_missing_subcommand(
                "'mapbox.exe styles' requires a subcommand but one was not provided",
                &[std::ffi::OsString::from(r"C:\bin\mapbox.exe")],
            );
            assert_eq!(full_path.as_deref(), Some("mapbox.exe styles --help"));
        }

        // A binary invoked under another name suggests that name, not this
        // crate's.
        let renamed = help_for_missing_subcommand(
            "'mbx styles' requires a subcommand but one was not provided",
            &[std::ffi::OsString::from("/usr/local/bin/mbx")],
        );
        assert_eq!(renamed.as_deref(), Some("mbx styles --help"));

        // And a message about a different program is still not a command
        // line: the quoted name has to be the one that was run.
        let mismatched = help_for_missing_subcommand(
            "'something-else styles' requires a subcommand but one was not provided",
            &[std::ffi::OsString::from("mapbox")],
        );
        assert_eq!(mismatched, None);
    }

    /// Every suggestion the table can emit, against every command the CLI
    /// really has: each one has to be a line this binary would accept. A
    /// suggestion that does not parse costs the caller a second failure to
    /// discover the first one was guessing — and the fixtures in
    /// `remedy.rs` cannot catch it, because the shape that breaks it comes
    /// from the specs (a sprite listing needs a style of its own, which the
    /// first version of this silently dropped).
    ///
    /// Checked through the same two steps `main` takes — clap, then the
    /// relaxed tree `--schema` is offered to — since that pair, not clap
    /// alone, is what decides whether a line runs.
    #[test]
    fn every_suggestion_is_a_command_line_that_runs() {
        let specs = bundled_specs();

        for svc in &specs {
            for op in svc.operations.iter().filter(|op| op.is_exposed()) {
                let supplied = an_invocation_of(&specs, svc, op);
                for username in [Some("someone"), None] {
                    for status in [400, 401, 403, 404, 409, 422, 429, 500, 503] {
                        let remedy = remedy::for_http(status, op, &supplied, username);
                        for action in &remedy.next_actions {
                            assert!(
                                would_run(&specs, action),
                                "{} (HTTP {status}) suggests `{action}`, \
                                 which this CLI would reject",
                                op.command()
                            );
                        }
                    }
                }
            }
        }
    }

    /// An operation's own matches, as if a caller had run it with a value for
    /// every path parameter — which is the state a 404 comes back from, and
    /// the only place a suggestion can get those values.
    fn an_invocation_of(
        specs: &[ServiceSpec],
        svc: &ServiceSpec,
        op: &spec::Operation,
    ) -> ArgMatches {
        let mut argv = vec!["mapbox".to_string(), svc.name.clone()];
        argv.extend(op.command_path.iter().cloned());
        argv.extend(op.path_params.iter().map(a_value_for));
        // `--schema` so the relaxed tree answers. What matters here is the
        // positionals, not whether the rest of the line is complete.
        argv.push("--schema".to_string());

        let app = build_app(specs);
        let owned: Vec<std::ffi::OsString> = argv.iter().map(std::ffi::OsString::from).collect();
        let top = schema::requested(&app, owned)
            .unwrap_or_else(|| panic!("`{}` did not parse at all", argv.join(" ")));
        // Down to the leaf: a nested command is three matches deep.
        let mut operation = &top;
        while let Some((_, sub)) = operation.subcommand() {
            operation = sub;
        }
        operation.clone()
    }

    /// A value the parameter's own parser accepts, so the invocation above is
    /// one clap really would have produced.
    fn a_value_for(param: &spec::Parameter) -> String {
        match param.enum_values.first() {
            Some(allowed) => allowed.clone(),
            None if param.numeric.is_some() => "1".to_string(),
            None => "value".to_string(),
        }
    }

    /// Whether the CLI would accept this line.
    fn would_run(specs: &[ServiceSpec], line: &str) -> bool {
        let app = build_app(specs);
        let argv: Vec<&str> = line.split_whitespace().collect();
        let owned: Vec<std::ffi::OsString> = argv.iter().map(std::ffi::OsString::from).collect();
        app.clone().try_get_matches_from(&argv).is_ok() || schema::requested(&app, owned).is_some()
    }

    /// End to end, through a real generated command.
    #[test]
    fn a_generated_numeric_parameter_rejects_text() {
        let specs = bundled_specs();
        let err = build_app(&specs)
            .try_get_matches_from(["mapbox", "accounts", "list-tokens", "--limit", "abc"])
            .expect_err("`--limit abc` should not parse");

        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }

    /// Clap's rendering of a command line it refused, corrected the way a
    /// caller would see it.
    fn rejection(specs: &[ServiceSpec], argv: &[&str]) -> String {
        let Err(err) = build_app(specs).try_get_matches_from(argv) else {
            panic!("`{}` should not parse", argv.join(" "))
        };
        drop_subcommand_from_short_circuit_usage(err)
            .render()
            .to_string()
    }

    /// The one line of a rendering that says how the command is meant to be
    /// written — the line this whole correction is about.
    fn usage_line(rendered: &str) -> &str {
        rendered
            .lines()
            .find(|line| line.starts_with("Usage:"))
            .unwrap_or_else(|| panic!("no usage line in:\n{rendered}"))
    }

    /// `mapbox --versiomn` suggested `--version` and then said
    /// `Usage: mapbox --version <COMMAND>`, contradicting the tip two lines
    /// above it and reading as though the suggestion were a prefix to combine
    /// with a subcommand. `mapbox --version` alone is the whole command.
    #[test]
    fn a_suggested_short_circuit_flag_is_not_offered_a_subcommand() {
        let specs = bundled_specs();

        for (typo, suggested) in [("--versiomn", "--version"), ("--helpp", "--help")] {
            let rendered = rejection(&specs, &["mapbox", typo]);

            // The tip is the half that was already right, and the reason the
            // usage line was worth correcting rather than dropping.
            assert!(
                rendered.contains(&format!("a similar argument exists: '{suggested}'")),
                "`{typo}` lost its suggestion:\n{rendered}"
            );
            assert_eq!(
                usage_line(&rendered),
                format!("Usage: mapbox {suggested}"),
                "`{suggested}` takes no subcommand:\n{rendered}"
            );
        }
    }

    /// The correction is for the two flags that end the parse, and nothing
    /// else: a subcommand really does belong on the line clap composes for
    /// every other suggestion.
    #[test]
    fn a_suggested_ordinary_flag_keeps_its_subcommand() {
        let specs = bundled_specs();
        let rendered = rejection(&specs, &["mapbox", "--tokne"]);
        let usage = usage_line(&rendered);

        assert!(
            usage.contains("--token") && usage.ends_with("<COMMAND>"),
            "`mapbox --token <token> styles get` is a real command line: {usage}"
        );
    }

    /// A mistyped subcommand is a different clap error with its own usage
    /// line, and the correction must not reach it.
    #[test]
    fn a_mistyped_subcommand_keeps_its_usage_line() {
        let specs = bundled_specs();

        assert_eq!(
            usage_line(&rejection(&specs, &["mapbox", "stylez"])),
            "Usage: mapbox [OPTIONS] <COMMAND>"
        );
    }

    /// A missing subcommand keeps its usage line too — the one case where
    /// `<COMMAND>` is the entire point of the message.
    #[test]
    fn a_missing_subcommand_keeps_its_usage_line() {
        let specs = bundled_specs();

        assert_eq!(
            usage_line(&rejection(&specs, &["mapbox", "styles"])),
            "Usage: mapbox styles [OPTIONS] <COMMAND>"
        );
    }

    /// Every path `typed_path` can return, which is the only thing the
    /// deprecation table may be keyed by.
    fn typeable_paths(app: &Command) -> Vec<String> {
        leaf_commands(app, &[])
            .into_iter()
            .map(|(path, _)| path.join(" "))
            .collect()
    }

    /// A deprecation that names something `typed_path` never produces warns
    /// nobody, and the table is written by hand — so it is held to the paths
    /// that can actually be typed.
    ///
    /// This used to walk `find_subcommand`, which accepts strictly more than
    /// that and hid two traps. `find_subcommand` matches aliases, but clap
    /// resolves an alias to its canonical name before `typed_path` sees it,
    /// so an entry naming `auth status` passed the check and could never
    /// fire. It also accepts a bare service name, which cannot fire either:
    /// `subcommand_required(true)` means `mapbox maps` fails at parse.
    #[test]
    fn every_deprecated_command_is_a_typeable_path() {
        let specs = bundled_specs();
        let paths = typeable_paths(&build_app(&specs));

        for entry in deprecation::DEPRECATED_COMMANDS {
            assert!(
                paths.iter().any(|path| path == entry.command),
                "`mapbox {}` is in DEPRECATED_COMMANDS but is not a path anyone can type",
                entry.command
            );
        }
    }

    /// The check above is vacuous while the table is empty, so this pins the
    /// thing it relies on: that the set really is canonical names only, and
    /// really does reject the two shapes that look valid.
    #[test]
    fn the_typeable_paths_are_canonical_names_only() {
        let specs = bundled_specs();
        let paths = typeable_paths(&build_app(&specs));

        assert!(paths.contains(&"styles delete".to_string()));
        // Three words, since a command path can nest.
        assert!(paths.contains(&"styles draft get".to_string()));
        assert!(paths.contains(&"auth whoami".to_string()));
        assert!(paths.contains(&tilesets_cli::COMMAND.to_string()));

        // `status` is a visible alias of `whoami`; `styles` is a service and
        // `styles draft` a group, and neither can be typed on its own.
        assert!(!paths.contains(&"auth status".to_string()));
        assert!(!paths.contains(&"styles".to_string()));
        assert!(!paths.contains(&"styles draft".to_string()));
    }

    /// The spec's `deprecated: true` has to reach the help a person reads,
    /// not just the warning they get after running it.
    #[test]
    fn a_deprecated_operation_is_marked_in_the_help_that_lists_it() {
        let svc = spec::parse_spec(
            "svc",
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /old:
    get:
      operationId: getOld
      summary: Fetch a thing
      description: The long one.
      deprecated: true
  /new:
    get:
      operationId: getNew
      summary: Fetch a thing
      description: The long one.
"#,
        )
        .expect("fixture parses");

        let rendered = |name: &str| {
            let cmd = build_service_command(&svc);
            let cmd = cmd.find_subcommand(name).expect("command exists");
            (
                cmd.get_about().map(ToString::to_string).unwrap_or_default(),
                cmd.get_long_about()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            )
        };

        // Both, because clap renders `about` for `-h` and `long_about` for
        // `--help`: marking only the first left the marker out of the
        // deprecated command's own help.
        assert_eq!(
            rendered("get-old"),
            (
                "[deprecated] Fetch a thing".to_string(),
                "[deprecated] The long one.".to_string()
            )
        );
        assert_eq!(
            rendered("get-new"),
            ("Fetch a thing".to_string(), "The long one.".to_string())
        );
    }
}
