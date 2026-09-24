//! `mapbox <command> --schema`: the command surface as JSON, for a caller
//! that is a program.
//!
//! `--help` is written for a person — prose, wrapped to a terminal, with the
//! types, the URL and the required-ness left implicit or spread across three
//! places. An agent picking a command needs the same facts as data, and
//! recovering them from help text is guesswork that breaks the next time
//! someone rewords a sentence.
//!
//! Nothing here is a second description of the CLI. The commands are built
//! from the OpenAPI specs at startup ([`crate::spec`]), so this module reads
//! the very `ServiceSpec` values `build_app` reads, and takes the globals
//! and the two hand-written commands off the built [`Command`] tree. Where a
//! fact exists in both, the test `the_schema_lists_the_flags_clap_declares`
//! fails the build if the two ever disagree.
//!
//! The output is JSON in every mode: a schema is the machine-readable half
//! by definition, and there is no second rendering of it worth having. What
//! `--output` still decides is whether it arrives indented.

use std::ffi::OsString;

use anyhow::Result;
use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::generate_skills;
use crate::output::{self, Mode};
use crate::spec::{Numeric, Operation, Parameter, RequestBody, ServiceSpec, ACCOUNT_PLACEHOLDERS};
use crate::tilesets_cli;

/// The `--schema` flag's arg id, and its long spelling.
pub const ARG: &str = "schema";

/// Bumped when a field changes meaning or leaves. Adding a field is not a
/// bump: a consumer that ignores unknown keys is unaffected, and one that
/// does not was never going to survive a new operation either.
const SCHEMA_VERSION: u32 = 1;

/// Said once at the top rather than repeated on every command, because it is
/// true of every command and the alternative is sixty copies of one sentence.
const AUTHENTICATION: &str = "Every command of kind `api` needs a token: pass --token, set \
                              MAPBOX_ACCESS_TOKEN, or run `mapbox auth login`. The CLI sends \
                              it as the `access_token` query parameter, not as a header, so \
                              it is not part of any argument below. The `passthrough` command \
                              authenticates its own way — see its note.";

/// Widened to `pub(crate)` for [`crate::generate_skills`], which renders the
/// same tree walk as Markdown rather than JSON. Nothing outside the crate
/// sees these types: the published surface is the JSON, and a Rust struct
/// promising to match it is a second contract to keep.
#[derive(serde::Serialize)]
pub(crate) struct Schema {
    schema_version: u32,
    pub(crate) cli: Cli,
    /// The command line this schema describes, whole: `mapbox`,
    /// `mapbox styles`, or `mapbox styles list`.
    target: String,
    pub(crate) authentication: &'static str,
    pub(crate) global_options: Vec<Argument>,
    pub(crate) commands: Vec<CommandEntry>,
}

#[derive(serde::Serialize)]
pub(crate) struct Cli {
    pub(crate) name: &'static str,
    pub(crate) version: &'static str,
}

/// What kind of thing a command is, since three of them behave differently
/// enough that a caller has to know which it is holding.
#[derive(serde::Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    /// Generated from an OpenAPI operation: it makes one HTTP request.
    Api,
    /// Hand-written and local — the `auth` commands, and `generate-skills`.
    Builtin,
    /// Arguments are forwarded verbatim to another program.
    Passthrough,
}

#[derive(serde::Serialize)]
pub(crate) struct CommandEntry {
    /// Ready to run, minus the arguments: `mapbox styles list`.
    pub(crate) command: String,
    pub(crate) service: String,
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// Set when the command is on its way out — either this CLI has
    /// deprecated the name or the spec has deprecated the endpoint behind
    /// it. The difference matters to a person, who is told which in words on
    /// stderr; to a program the only question is whether to keep depending
    /// on the command, and that answer is the same either way.
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) deprecated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) request: Option<Request>,
    /// The command that shows one of the things this one lists, when the
    /// spec describes such a pair.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) forwards_to: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<&'static str>,
    /// Other names this command answers to and offers: `--help` lists them
    /// beside the command, and either spelling is as good as the other.
    ///
    /// Empty for every command today: only a visible alias reaches this
    /// field, a visible alias needs a `spec::COMMAND_ALIASES` row with
    /// `show_generated_name: true`, and that table is currently empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) aliases: Vec<&'static str>,
    pub(crate) arguments: Vec<Argument>,
}

#[derive(serde::Serialize)]
pub(crate) struct Request {
    pub(crate) method: String,
    /// With its placeholders still in it, so a reader can see which argument
    /// lands where.
    pub(crate) url: String,
}

#[derive(serde::Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArgKind {
    /// Written in order, with no flag in front of it.
    Positional,
    /// A flag that takes a value.
    Option,
    /// A flag that is either present or absent.
    Flag,
}

#[derive(serde::Serialize)]
pub(crate) struct Argument {
    pub(crate) name: String,
    pub(crate) kind: ArgKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) flag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) short: Option<String>,
    pub(crate) required: bool,
    #[serde(rename = "type")]
    pub(crate) value_type: &'static str,
    /// The only accepted values, when the spec names them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) values: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) repeatable: bool,
    /// Where the value ends up in the request: `path`, `query` or `body`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) location: Option<&'static str>,
    /// `global` for an argument this command needs but does not declare —
    /// it is one of `global_options`, listed here because the request cannot
    /// be built without it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content_type: Option<String>,
    /// The multipart form field the files go under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) multipart_field: Option<String>,
    /// Arguments that cannot be given alongside this one, by `name`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) conflicts_with: Vec<String>,
    /// The group of arguments — this one included — of which the request
    /// needs exactly one. Present instead of `required` where a body may
    /// arrive as either `--data` or `--file`: neither is required on its
    /// own, and giving both is an error.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) required_one_of: Vec<String>,
    /// The URL placeholder this argument's value is substituted into, where
    /// the two are not named the same thing — `--username` fills `{owner}`
    /// and `{account}` as well as `{username}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fills: Option<String>,
    /// Whether passing an empty string leaves this positional out of the
    /// URL — usually a suffix (`.png` off the end of a tile name), sometimes
    /// a whole segment. The argument must still be written: clap requires
    /// every path positional, and the spec's optional ones are omitted by
    /// value, not by absence.
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) omit_with_empty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) default: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Argument {
    /// Everything absent, so each caller writes only the fields that are
    /// true of the argument it is describing.
    fn new(name: impl Into<String>, kind: ArgKind) -> Self {
        Argument {
            name: name.into(),
            kind,
            flag: None,
            short: None,
            required: false,
            value_type: "string",
            values: vec![],
            description: None,
            repeatable: false,
            location: None,
            source: None,
            content_type: None,
            multipart_field: None,
            conflicts_with: vec![],
            required_one_of: vec![],
            fills: None,
            omit_with_empty: false,
            env: None,
            default: None,
        }
    }
}

/// The parse of a command line that asks for a schema rather than a request.
///
/// A schema request names a command without running it, so the arguments
/// that command needs are usually absent — clap rejects the line before
/// anything can look at `--schema`. Rather than hand-parse argv, the line is
/// offered a second time to a copy of the command tree that requires
/// nothing, and clap answers the question it is best placed to answer: was
/// `--schema` really written, or was it the value of some other flag?
///
/// `None` for any line the relaxed tree also rejects — `--help` among them,
/// which is an error clap raises to print something, not to complain.
pub fn requested(app: &Command, argv: Vec<OsString>) -> Option<ArgMatches> {
    let matches = relaxed(app.clone()).try_get_matches_from(argv).ok()?;
    matches.get_flag(ARG).then_some(matches)
}

/// A copy of the command tree that demands nothing: no required argument, no
/// required subcommand, no help in place of an empty line.
///
/// Used only to read a target out of the line. Never to run anything — the
/// requirements it drops are the ones that keep a real invocation honest.
fn relaxed(cmd: Command) -> Command {
    cmd.arg_required_else_help(false)
        .subcommand_required(false)
        .mut_args(|arg| arg.required(false))
        .mut_subcommands(relaxed)
}

/// Writes the schema for whatever command the line named.
pub fn emit(mode: Mode, app: &Command, specs: &[ServiceSpec], matches: &ArgMatches) -> Result<()> {
    let path = target_path(matches);
    let schema = build(app, specs, &path);
    output::emit_value(
        json_mode(mode),
        &serde_json::to_value(schema)?,
        None,
        None,
        // `--schema` describes the binary, not an API response; there is no
        // page after it.
        None,
    )
}

/// `--schema` answers in JSON whatever `--output` says, because there is no
/// text rendering of a schema anyone wants. `text` still means a person is
/// reading, so it indents.
fn json_mode(mode: Mode) -> Mode {
    match mode {
        Mode::Text => Mode::Json { pretty: true },
        json => json,
    }
}

/// The subcommand names on the line, outermost first: `[]`, `["styles"]`,
/// `["styles", "list"]`, or `["styles", "draft", "get"]`.
fn target_path(matches: &ArgMatches) -> Vec<String> {
    let mut path = vec![];
    let mut current = matches;
    while let Some((name, sub)) = current.subcommand() {
        path.push(name.to_string());
        current = sub;
    }
    path
}

pub(crate) fn build(app: &Command, specs: &[ServiceSpec], path: &[String]) -> Schema {
    let target = std::iter::once("mapbox")
        .chain(path.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");

    Schema {
        schema_version: SCHEMA_VERSION,
        cli: Cli {
            name: "mapbox",
            version: env!("CARGO_PKG_VERSION"),
        },
        target,
        authentication: AUTHENTICATION,
        // Whole, at every level: a caller that asked about one command still
        // needs to know a token goes on the front of it.
        global_options: global_options(app),
        commands: commands(app, specs, path),
    }
}

fn global_options(app: &Command) -> Vec<Argument> {
    app.get_arguments()
        .filter(|arg| arg.is_global_set())
        .map(clap_argument)
        .collect()
}

/// Every command the target names: all of them, one service's, or one.
///
/// The target can also name an intermediate group — `mapbox styles draft
/// --schema` — so what is matched is a *prefix* of a command's path rather
/// than its whole name. That is the same relationship `mapbox styles
/// --schema` has always had to the commands under it, one level down.
fn commands(app: &Command, specs: &[ServiceSpec], path: &[String]) -> Vec<CommandEntry> {
    let wanted_service = path.first().map(String::as_str);
    let wanted_path = &path[path.len().min(1)..];
    // The hand-written groups are one level deep and always will be, so they
    // ask the simpler question.
    let wanted_command = path.get(1).map(String::as_str);
    let wants = |service: &str| wanted_service.is_none_or(|w| w == service);

    let mut out = vec![];

    for svc in specs.iter().filter(|svc| wants(&svc.name)) {
        // The same filter `build_service_command` applies. An operation left
        // out of the command surface must be left out here too: a schema
        // that advertises one would send a caller to a command that answers
        // exactly as a typo does.
        for op in svc.operations.iter().filter(|op| op.is_exposed()) {
            if op.command_path.starts_with(wanted_path) {
                out.push(api_command(app, svc, op));
            }
        }
    }

    if wants("auth") {
        out.extend(builtin_commands(app, "auth", wanted_command));
    }

    if wants(generate_skills::COMMAND) && wanted_command.is_none() {
        out.extend(builtin_leaf_command(app, generate_skills::COMMAND));
    }

    if wants(crate::agent_skills::COMMAND) {
        out.extend(builtin_commands(
            app,
            crate::agent_skills::COMMAND,
            wanted_command,
        ));
    }

    if wants(crate::completion::COMMAND) && wanted_command.is_none() {
        out.extend(builtin_leaf_command(app, crate::completion::COMMAND));
    }

    if wants(crate::uninstall::COMMAND) && wanted_command.is_none() {
        out.extend(builtin_leaf_command(app, crate::uninstall::COMMAND));
    }

    if wants(crate::config::COMMAND) {
        out.extend(builtin_commands(
            app,
            crate::config::COMMAND,
            wanted_command,
        ));
    }

    if wants(crate::doctor::COMMAND) && wanted_command.is_none() {
        out.extend(builtin_leaf_command(app, crate::doctor::COMMAND));
    }

    // No flag check needed: builtin_leaf_command's find_subcommand returns
    // None (a no-op .extend) when the flag left it out of `app`.
    if wants(crate::account_usage::COMMAND) && wanted_command.is_none() {
        out.extend(builtin_leaf_command(app, crate::account_usage::COMMAND));
    }

    if wants(tilesets_cli::COMMAND) && wanted_command.is_none() {
        out.extend(passthrough_command(app));
    }

    out
}

/// The `Command` clap built for a command path under a service, or `None`
/// where the tree has no such command.
///
/// A walk rather than one `find_subcommand`, because a path can be more than
/// one segment: `styles draft get` is three levels down from the root.
fn declared_command<'a>(app: &'a Command, service: &str, path: &[String]) -> Option<&'a Command> {
    path.iter()
        .try_fold(app.find_subcommand(service)?, |cmd, segment| {
            cmd.find_subcommand(segment)
        })
}

fn api_command(app: &Command, svc: &ServiceSpec, op: &Operation) -> CommandEntry {
    let declared = declared_command(app, &svc.name, &op.command_path);

    let mut arguments: Vec<Argument> = op
        .path_params
        .iter()
        .map(|param| Argument {
            // Every path positional is required, whatever the spec says,
            // because `build_operation_command` makes it so: the value has to
            // be written even where the spec calls the segment optional. What
            // the spec's `required: false` buys is the right to write nothing
            // *as* that value, which `omit_with_empty` reports rather than
            // contradicting clap here.
            required: true,
            location: Some("path"),
            omit_with_empty: omits_with_empty(param, &op.path_template),
            ..parameter(param, ArgKind::Positional)
        })
        .collect();

    arguments.extend(username_argument(op));

    arguments.extend(op.query_params.iter().map(|param| {
        let kind = if param.is_boolean {
            ArgKind::Flag
        } else {
            ArgKind::Option
        };
        Argument {
            flag: Some(format!("--{}", param.arg_name)),
            required: param.required,
            location: Some("query"),
            ..parameter(param, kind)
        }
    }));

    arguments.extend(
        op.body
            .as_ref()
            .map(|body| body_arguments(body, declared))
            .unwrap_or_default(),
    );

    // Described because it is declared — `the_schema_lists_the_flags_clap_declares`
    // holds the two to each other, and a flag missing here is a flag an agent
    // has no way to learn about. It has no `location`: it is the one argument
    // on a command that does not become part of the request.
    if op.is_mutating() {
        arguments.push(Argument {
            flag: Some(format!("--{}", crate::executor::DRY_RUN_ARG)),
            value_type: "boolean",
            description: declared_help(declared, crate::executor::DRY_RUN_ARG),
            ..Argument::new(crate::executor::DRY_RUN_ARG, ArgKind::Flag)
        });
    }

    CommandEntry {
        command: format!("mapbox {}", op.command()),
        service: svc.name.clone(),
        // The path, not the last word: `draft get` under `styles`. Which
        // keeps `command` exactly `mapbox <service> <name>` for every API
        // command, nested or not.
        name: op.command_path.join(" "),
        kind: Kind::Api,
        summary: op.summary.clone(),
        description: op.description.clone(),
        deprecated: op.deprecated || crate::deprecation::find(&op.command()).is_some(),
        request: Some(Request {
            method: op.method.clone(),
            url: format!("{}{}", op.base_url, op.path_template),
        }),
        detail_command: op
            .detail
            .as_ref()
            .map(|detail| format!("mapbox {}", detail.command)),
        forwards_to: None,
        note: None,
        aliases: op.aliases.clone(),
        arguments,
    }
}

/// The half of an argument that comes from the spec's parameter, whatever
/// the parameter turns out to be.
fn parameter(param: &Parameter, kind: ArgKind) -> Argument {
    Argument {
        value_type: value_type(param),
        values: param.enum_values.clone(),
        description: param.description.clone(),
        ..Argument::new(param.arg_name.clone(), kind)
    }
}

/// Whether writing `""` for this positional leaves it out of the URL.
///
/// Three conditions, each ruling out a way of being wrong about it.
///
/// The spec has to call the parameter optional, because that is what
/// [`crate::executor::substitute_path_param`] keys its segment-drop on. Clap
/// has to accept the empty string, which it will not for a numeric parameter
/// (`bearing`, `pitch`) or for an enum that does not list `""` among its
/// values (`tilesize`) — those are spec-optional and, through this CLI,
/// impossible to omit. And the URL that results has to be a URL.
fn omits_with_empty(param: &Parameter, path_template: &str) -> bool {
    if param.required {
        return false;
    }

    let clap_accepts_empty = if param.enum_values.is_empty() {
        param.numeric.is_none()
    } else {
        param.enum_values.iter().any(String::is_empty)
    };

    clap_accepts_empty && leaves_a_usable_url(path_template, &param.name)
}

/// Whether taking this placeholder out of the template leaves a path the API
/// will read the way the caller meant.
///
/// A placeholder that owns a whole segment takes its slash with it, which the
/// executor handles and which is always safe. Anything else is substituted in
/// place, and that is only safe where the placeholder is not holding two
/// separators apart: `{y}{format}` collapses to `1`, but
/// `{zoom},{bearing},{pitch}` would collapse to `12,,0`.
///
/// Checked by building the string rather than by reasoning about the
/// characters either side, so it stays right for a shape nobody has thought
/// of yet.
fn leaves_a_usable_url(path_template: &str, name: &str) -> bool {
    let placeholder = format!("{{{name}}}");

    if path_template.contains(&format!("/{placeholder}")) {
        return true;
    }
    if !path_template.contains(&placeholder) {
        return false;
    }

    let removed = path_template.replace(&placeholder, "");
    !(removed.contains("//")
        || removed.contains(",,")
        || removed.contains("/,")
        || removed.contains(",/")
        || removed.ends_with(','))
}

fn value_type(param: &Parameter) -> &'static str {
    if param.is_boolean {
        return "boolean";
    }
    match param.numeric {
        Some(Numeric::Integer) => "integer",
        Some(Numeric::Float) => "number",
        None => "string",
    }
}

/// The account the URL asks for, when the URL asks for one.
fn username_argument(op: &Operation) -> Option<Argument> {
    // The placeholder is named as well as filled: eight commands spell it
    // `{owner}` or `{account}`, and an agent matching argument names against
    // the URL would find nothing that fills those.
    let placeholder = ACCOUNT_PLACEHOLDERS
        .iter()
        .map(|name| format!("{{{name}}}"))
        .find(|placeholder| op.path_template.contains(placeholder))?;

    Some(Argument {
        fills: Some(placeholder),
        flag: Some("--username".to_string()),
        short: Some("-u".to_string()),
        // Required in the sense that matters: the URL cannot be built
        // without it. Not in the sense that it must be typed — a login
        // supplies one, which is why it is a global and not a positional.
        required: true,
        location: Some("path"),
        source: Some("global"),
        description: Some(
            "Fills the account placeholder in the URL. See `--username` under global_options \
             for where the value may come from."
                .to_string(),
        ),
        ..Argument::new("username", ArgKind::Option)
    })
}

/// `--data` and `--file`, split the way [`crate::build_operation_command`]
/// splits them, and required the way the spec says the body is.
///
/// The descriptions are read back off the command clap built rather than
/// written again here, so the two can only ever say the same thing.
fn body_arguments(body: &RequestBody, declared: Option<&Command>) -> Vec<Argument> {
    let text = body.text_content_type();
    let takes_data = body.accepts_json() || text.is_some();
    let file_type = body.file_content_type();

    // `starFile` and `initUpload` accept either flag for the same body. When
    // the body is required, neither flag is required on its own — writing
    // both is an error — so the obligation is on the pair.
    let alternatives = takes_data && file_type.is_some();
    let required_one_of = |names: [&str; 2]| {
        if body.required && alternatives {
            names.iter().map(|name| name.to_string()).collect()
        } else {
            vec![]
        }
    };
    let required = body.required && !alternatives;

    let mut out = vec![];

    if takes_data {
        out.push(Argument {
            flag: Some("--data".to_string()),
            short: Some("-d".to_string()),
            required,
            required_one_of: required_one_of(["data", "file"]),
            location: Some("body"),
            content_type: Some(text.unwrap_or("application/json").to_string()),
            conflicts_with: conflicts(alternatives, "file"),
            description: declared_help(declared, "data"),
            ..Argument::new("data", ArgKind::Option)
        });
    }

    if let Some(content_type) = file_type {
        let multipart = body.is_multipart();
        out.push(Argument {
            flag: Some("--file".to_string()),
            value_type: "path",
            required,
            required_one_of: required_one_of(["data", "file"]),
            location: Some("body"),
            content_type: Some(content_type.to_string()),
            // Repeating the flag is what makes a batch a batch.
            repeatable: multipart,
            multipart_field: multipart
                .then(|| body.multipart_field.clone().unwrap_or("file".to_string())),
            conflicts_with: conflicts(alternatives, "data"),
            description: declared_help(declared, "file"),
            ..Argument::new("file", ArgKind::Option)
        });
    }

    out
}

/// Both directions of the `--data`/`--file` conflict. Clap enforces it both
/// ways; naming it on only one of the two left a caller reading the other's
/// entry alone with no sign of it.
fn conflicts(alternatives: bool, other: &str) -> Vec<String> {
    if alternatives {
        vec![other.to_string()]
    } else {
        vec![]
    }
}

/// The help clap shows for one of a command's own arguments.
fn declared_help(cmd: Option<&Command>, id: &str) -> Option<String> {
    cmd?.get_arguments()
        .find(|arg| arg.get_id() == id)?
        .get_help()
        .map(ToString::to_string)
}

/// The hand-written commands under one group, read off the built tree rather
/// than written down again here — `auth` gains a subcommand by being given
/// one in `build_app`, and this follows.
fn builtin_commands(app: &Command, group: &str, wanted: Option<&str>) -> Vec<CommandEntry> {
    let Some(parent) = app.find_subcommand(group) else {
        return vec![];
    };

    parent
        .get_subcommands()
        .filter(|cmd| wanted.is_none_or(|w| w == cmd.get_name()))
        .map(|cmd| CommandEntry {
            command: format!("mapbox {group} {}", cmd.get_name()),
            service: group.to_string(),
            name: cmd.get_name().to_string(),
            kind: Kind::Builtin,
            summary: about(cmd),
            description: long_about(cmd),
            deprecated: crate::deprecation::find(&crate::deprecation::path(group, cmd.get_name()))
                .is_some(),
            request: None,
            detail_command: None,
            forwards_to: None,
            note: None,
            aliases: vec![],
            arguments: own_arguments(cmd),
        })
        .collect()
}

/// What a hand-written command declares for itself — the globals belong to
/// `global_options`, and clap's own `help` is not part of anyone's surface.
fn own_arguments(cmd: &Command) -> Vec<Argument> {
    cmd.get_arguments()
        .filter(|arg| !arg.is_global_set() && arg.get_id() != "help")
        .map(clap_argument)
        .collect()
}

/// A hand-written command that is a leaf: it declares its own arguments and
/// has nothing under it.
///
/// `builtin_commands` above describes the children of a group (`auth`), and
/// `passthrough_command` describes one whose arguments belong to another
/// program. `generate-skills` is neither — it is one command with real
/// arguments of its own — and `every_command_in_the_tree_is_described` fails
/// the build until it is described here.
fn builtin_leaf_command(app: &Command, name: &'static str) -> Option<CommandEntry> {
    let cmd = app.find_subcommand(name)?;

    Some(CommandEntry {
        command: format!("mapbox {name}"),
        // Its own name, in both places: the command sits at the top level, so
        // the group it belongs to is itself. `commands()` matches a
        // `--schema` target against this, and a reference page is written per
        // distinct `service`.
        service: name.to_string(),
        name: name.to_string(),
        kind: Kind::Builtin,
        summary: about(cmd),
        description: long_about(cmd),
        // `generate-skills` cannot itself be deprecated by a spec — it has
        // none — and this CLI has no mechanism for deprecating a
        // hand-written command.
        deprecated: false,
        request: None,
        detail_command: None,
        forwards_to: None,
        note: None,
        aliases: vec![],
        arguments: own_arguments(cmd),
    })
}

/// `tilesets-cli`, whose one argument is everything the child will be given.
fn passthrough_command(app: &Command) -> Option<CommandEntry> {
    let cmd = app.find_subcommand(tilesets_cli::COMMAND)?;

    Some(CommandEntry {
        command: format!("mapbox {}", tilesets_cli::COMMAND),
        service: tilesets_cli::COMMAND.to_string(),
        name: tilesets_cli::COMMAND.to_string(),
        kind: Kind::Passthrough,
        summary: about(cmd),
        description: long_about(cmd),
        deprecated: crate::deprecation::find(tilesets_cli::COMMAND).is_some(),
        request: None,
        detail_command: None,
        forwards_to: Some(tilesets_cli::DEFAULT_BINARY),
        note: Some(
            "Arguments after the subcommand name are forwarded verbatim, so `--schema` and the \
             other mapbox globals only apply written before it: `mapbox --schema tilesets-cli`. \
             Run `mapbox tilesets-cli --help` for what the child accepts.",
        ),
        aliases: vec![],
        arguments: own_arguments(cmd),
    })
}

fn about(cmd: &Command) -> String {
    cmd.get_about().map(ToString::to_string).unwrap_or_default()
}

/// The long help, where a hand-written command has one. It is the same thing
/// an API command's `description` is: everything the summary left out.
fn long_about(cmd: &Command) -> Option<String> {
    cmd.get_long_about().map(ToString::to_string)
}

/// An argument clap owns outright: the globals, and anything a hand-written
/// command declares.
fn clap_argument(arg: &Arg) -> Argument {
    let long = arg.get_long();
    let is_flag = matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::SetFalse);

    let kind = match (long.is_some(), is_flag) {
        (false, _) => ArgKind::Positional,
        (true, true) => ArgKind::Flag,
        (true, false) => ArgKind::Option,
    };

    Argument {
        // The long spelling is the name a caller types. Only the id is left
        // when there is no long one, and `--id` is exactly why the two can
        // differ: its id is `filter-id`, because two style operations take a
        // path parameter called `id`.
        flag: long.map(|long| format!("--{long}")),
        short: arg.get_short().map(|short| format!("-{short}")),
        required: arg.is_required_set(),
        value_type: if is_flag { "boolean" } else { "string" },
        // A flag takes no value, so it has none to accept — whatever its
        // parser would say about one. `--yes` and `--debug` read their
        // environment variables through `FalseyValueParser`, whose twelve
        // spellings clap reports as possible values; published here they
        // described a command line that does not exist, and `mapbox --yes=1`
        // is a usage error. Pinned by
        // `a_flag_promises_no_values_because_it_takes_none`.
        values: if is_flag {
            vec![]
        } else {
            arg.get_possible_values()
                .iter()
                .map(|value| value.get_name().to_string())
                .collect()
        },
        description: arg
            .get_help()
            .map(ToString::to_string)
            .filter(|help| !help.is_empty()),
        // `--file` repeats by being written again; `tilesets-cli`'s
        // passthrough takes many values at once. Both mean "more than one".
        repeatable: matches!(arg.get_action(), ArgAction::Append)
            || arg
                .get_num_args()
                .is_some_and(|range| range.max_values() > 1),
        env: arg.get_env().and_then(|env| env.to_str()).map(String::from),
        default: arg
            .get_default_values()
            .first()
            .and_then(|value| value.to_str())
            .map(String::from),
        ..Argument::new(
            long.unwrap_or_else(|| arg.get_id().as_str()).to_string(),
            kind,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs() -> Vec<ServiceSpec> {
        crate::spec::effective_services().expect("the bundled specs parse")
    }

    /// One argument, reduced to what both sides can be asked about: how it
    /// is written, whether it must be, and its short spelling.
    type Shape = (String, bool, Option<String>);

    /// Every argument clap declares.
    fn declared(cmd: &Command) -> Vec<Shape> {
        let mut shapes: Vec<Shape> = cmd
            .get_arguments()
            .filter(|arg| !arg.is_global_set() && arg.get_id() != "help")
            .map(|arg| {
                let spelling = match arg.get_long() {
                    Some(long) => format!("--{long}"),
                    None => arg.get_id().to_string(),
                };
                let short = arg.get_short().map(|short| format!("-{short}"));
                (spelling, arg.is_required_set(), short)
            })
            .collect();
        shapes.sort();
        shapes
    }

    /// The same, out of the schema. Two exclusions, both deliberate:
    ///
    /// A `global` argument is listed in the schema because the request needs
    /// it, not because the command declares it, so clap has nothing to
    /// compare against. And a body argument's `required` comes from the
    /// spec's `requestBody.required` rather than from clap, which accepts a
    /// bodyless `create-style` and lets the API refuse it — so its
    /// required-ness is compared against the spec by
    /// `a_required_body_is_described_as_required` instead of here.
    fn described(entry: &CommandEntry) -> Vec<Shape> {
        let mut shapes: Vec<Shape> = entry
            .arguments
            .iter()
            .filter(|arg| arg.source.is_none())
            .map(|arg| {
                let spelling = arg.flag.clone().unwrap_or_else(|| arg.name.clone());
                let required = if arg.location == Some("body") {
                    // Whatever clap says, so this comparison stays about the
                    // fields it can speak to.
                    false
                } else {
                    arg.required
                };
                (spelling, required, arg.short.clone())
            })
            .collect();
        shapes.sort();
        shapes
    }

    /// The reason the schema is allowed to read the specs a second time: if
    /// the two derivations ever disagree about what a command takes, this
    /// fails rather than shipping a description of a command that is not
    /// the command.
    #[test]
    fn the_schema_lists_the_flags_clap_declares() {
        let specs = specs();
        let app = crate::build_app(&specs);
        let schema = build(&app, &specs, &[]);

        for entry in &schema.commands {
            // A hand-written leaf is its own service, so there is no second
            // level to walk down to. Everything else is named by its path,
            // which is one segment for most commands and two for a nested
            // one (`draft get`).
            let path: Vec<String> = if entry.service == entry.name {
                vec![]
            } else {
                entry.name.split(' ').map(str::to_string).collect()
            };
            let cmd = declared_command(&app, &entry.service, &path)
                .unwrap_or_else(|| panic!("{} is described but is not a command", entry.command));

            assert_eq!(
                described(entry),
                declared(cmd),
                "the schema and the command tree disagree about `{}`",
                entry.command
            );
        }
    }

    /// A positional is identified by where it sits, so the order it is
    /// described in is part of the contract and not a detail of iteration.
    #[test]
    fn positionals_keep_the_order_they_are_typed_in() {
        let specs = specs();
        let app = crate::build_app(&specs);
        let schema = build(&app, &specs, &[]);

        for entry in &schema.commands {
            let path: Vec<String> = entry.name.split(' ').map(str::to_string).collect();
            let Some(cmd) = declared_command(&app, &entry.service, &path) else {
                continue;
            };

            let clap_order: Vec<String> = cmd
                .get_positionals()
                .map(|arg| arg.get_id().to_string())
                .collect();
            let schema_order: Vec<String> = entry
                .arguments
                .iter()
                .filter(|arg| matches!(arg.kind, ArgKind::Positional))
                .map(|arg| arg.name.clone())
                .collect();

            assert_eq!(schema_order, clap_order, "{}", entry.command);
        }
    }

    /// The other direction: a service or a command added to the tree and
    /// forgotten here would leave an agent unable to discover it at all.
    #[test]
    fn every_command_in_the_tree_is_described() {
        let specs = specs();
        let app = crate::build_app(&specs);

        let mut runnable = crate::runnable_commands(&app);
        runnable.sort();

        let mut described: Vec<String> = build(&app, &specs, &[])
            .commands
            .iter()
            .map(|entry| entry.command.clone())
            .collect();
        described.sort();

        assert_eq!(described, runnable);
    }

    /// `--schema` is a description, and a description of a command that
    /// cannot be run is a wrong description. The command tree leaves those
    /// operations out; so does this.
    #[test]
    fn an_operation_that_is_not_a_command_is_not_described() {
        let specs = specs();
        let app = crate::build_app(&specs);
        let schema = build(&app, &specs, &[]);

        for withheld in [
            "mapbox styles set-style-protected",
            "mapbox styles admin-get-style",
            "mapbox styles download-style-zip",
            "mapbox accounts create-token",
            "mapbox tilesets get-legacy-tile",
        ] {
            assert!(
                !schema.commands.iter().any(|c| c.command == withheld),
                "{withheld} is not a command, so it must not be in the schema"
            );
        }
    }

    /// The spec's `requestBody.required` reaches the schema, for every
    /// operation that has a body. Thirteen of the sixteen say `true`, and
    /// every one of them read `false` until this was fixed.
    #[test]
    fn a_required_body_is_described_as_required() {
        let specs = specs();
        let app = crate::build_app(&specs);
        let schema = build(&app, &specs, &[]);

        let mut checked = 0;
        for svc in &specs {
            for op in svc.operations.iter().filter(|op| op.is_exposed()) {
                let Some(body) = &op.body else { continue };
                let entry = schema
                    .commands
                    .iter()
                    .find(|entry| entry.command == format!("mapbox {}", op.command()))
                    .expect("a command for every exposed operation");

                let body_args: Vec<&Argument> = entry
                    .arguments
                    .iter()
                    .filter(|arg| arg.location == Some("body"))
                    .collect();
                assert!(
                    !body_args.is_empty(),
                    "{} has a body and no flag for it",
                    entry.command
                );

                // One flag carries the obligation itself; two carry it
                // between them, since either satisfies the request and both
                // together are an error.
                if body_args.len() == 1 {
                    assert_eq!(
                        body_args[0].required, body.required,
                        "{} disagrees with its spec about the body",
                        entry.command
                    );
                } else {
                    for arg in &body_args {
                        assert!(
                            !arg.required,
                            "{} makes one alternative mandatory",
                            entry.command
                        );
                        assert_eq!(
                            !arg.required_one_of.is_empty(),
                            body.required,
                            "{} disagrees with its spec about the body",
                            entry.command
                        );
                    }
                }
                checked += 1;
            }
        }
        // Lower than before the open-source review: several body-carrying
        // operations (all of `sources`, several `styles` ones) are stripped
        // from the bundled specs entirely now by the maintainer-only
        // decision record. Eight is the current true count with room to
        // still catch a real regression.
        assert!(
            checked >= 8,
            "only {checked} bodies checked — did the specs move?"
        );
    }

    /// `detail_command` is a command the caller is told to run, so it has to
    /// be one. `link_detail_operations` pairs on paths alone, and the
    /// withheld and unusable sets are full of GETs sitting on exactly the
    /// paths it matches.
    #[test]
    fn every_detail_command_is_a_command() {
        let specs = specs();
        let app = crate::build_app(&specs);
        let schema = build(&app, &specs, &[]);

        let commands: Vec<&str> = schema
            .commands
            .iter()
            .map(|entry| entry.command.as_str())
            .collect();

        for entry in &schema.commands {
            let Some(detail) = &entry.detail_command else {
                continue;
            };
            assert!(
                commands.contains(&detail.as_str()),
                "{} points at `{detail}`, which is not a command",
                entry.command
            );
        }
    }

    /// Every `{placeholder}` in a described URL has something that fills it.
    ///
    /// This is what pins [`ACCOUNT_PLACEHOLDERS`] against the executor: a
    /// spec that spells the account a fourth way, or a parameter `parse_spec`
    /// drops for a new reason, leaves a hole here rather than in a caller's
    /// request.
    #[test]
    fn every_url_placeholder_has_an_argument() {
        let specs = specs();
        let app = crate::build_app(&specs);
        let schema = build(&app, &specs, &[]);

        for entry in &schema.commands {
            let Some(request) = &entry.request else {
                continue;
            };
            for placeholder in request
                .url
                .split('{')
                .skip(1)
                .filter_map(|rest| rest.split('}').next())
            {
                let wrapped = format!("{{{placeholder}}}");
                let filled = entry.arguments.iter().any(|arg| {
                    arg.name == placeholder.replace('_', "-")
                        || arg.fills.as_deref() == Some(&wrapped)
                });
                assert!(
                    filled,
                    "{} shows {wrapped} and nothing fills it",
                    entry.command
                );
            }
        }
    }

    /// The globals are the other half of an invocation, and nothing else
    /// checks that the schema's copy of them is complete.
    #[test]
    fn the_globals_are_listed_whole() {
        let specs = specs();
        let app = crate::build_app(&specs);

        let mut expected: Vec<String> = app
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .map(|arg| match arg.get_long() {
                Some(long) => format!("--{long}"),
                None => arg.get_id().to_string(),
            })
            .collect();
        expected.sort();

        let mut listed: Vec<String> = build(&app, &specs, &[])
            .global_options
            .iter()
            .map(|arg| arg.flag.clone().unwrap_or_else(|| arg.name.clone()))
            .collect();
        listed.sort();

        assert_eq!(listed, expected);
    }

    fn optional(name: &str) -> Parameter {
        Parameter {
            name: name.to_string(),
            arg_name: name.replace('_', "-"),
            required: false,
            description: None,
            enum_values: vec![],
            is_boolean: false,
            numeric: None,
        }
    }

    /// The three ways `omit_with_empty` can be wrong, one case each.
    ///
    /// The live specs exercise only the true branches, so a regression in the
    /// guards would ship silently — the comma case in particular is one no
    /// bundled spec reaches today and one the executor would answer with
    /// `12,,0`.
    #[test]
    fn an_empty_value_is_only_offered_where_it_leaves_a_url() {
        // A whole segment takes its slash with it.
        assert!(omits_with_empty(
            &optional("overlay"),
            "/styles/v1/{username}/{style_id}/static/{overlay}/{lon},{lat}"
        ));
        // A suffix collapses into the segment it sits in.
        assert!(omits_with_empty(
            &optional("quality"),
            "/v4/{tilesets}/{z}/{x}/{y}{format}{quality}"
        ));

        // Between two separators it would leave `12,,0`.
        assert!(!omits_with_empty(
            &optional("bearing"),
            "/static/{lon},{lat},{zoom},{bearing},{pitch}/{width}x{height}"
        ));
        // Clap will not take `""` for either of these.
        assert!(!omits_with_empty(
            &Parameter {
                numeric: Some(Numeric::Float),
                ..optional("pitch")
            },
            "/static/{pitch}/x"
        ));
        assert!(!omits_with_empty(
            &Parameter {
                enum_values: vec!["256".into(), "512".into()],
                ..optional("tilesize")
            },
            "/tiles/{tilesize}/{z}"
        ));
        // An enum that lists the empty string does allow it.
        assert!(omits_with_empty(
            &Parameter {
                enum_values: vec!["@2x".into(), String::new()],
                ..optional("highRes")
            },
            "/tiles/{z}/{x}/{y}{highRes}"
        ));

        // The spec has the last word.
        assert!(!omits_with_empty(
            &Parameter {
                required: true,
                ..optional("overlay")
            },
            "/static/{overlay}/x"
        ));
    }

    /// A schema is JSON even when the caller asked for text, and indented
    /// only when someone is looking at it.
    #[test]
    fn the_mode_is_json_whatever_output_says() {
        assert_eq!(json_mode(Mode::Text), Mode::Json { pretty: true });
        assert_eq!(
            json_mode(Mode::Json { pretty: false }),
            Mode::Json { pretty: false }
        );
    }
}
