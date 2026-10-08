//! The layout of top-level `mapbox --help`: commands in named groups rather
//! than one flat list.
//!
//! Commands are grouped by what the reader is trying to do, not by how the
//! command is built — an API command and a hand-written one sit side by side
//! when they serve the same task:
//!
//! - **Maps and data**: create or fetch map content — styles, sprites,
//!   fonts, tiles, static images.
//! - **Search**: find places and addresses, and report problems with them.
//! - **Account**: credentials, tokens and usage — who you are and what you
//!   have used.
//! - **Coding agents**: wire Mapbox into a coding agent.
//! - **CLI**: manage the CLI itself.
//!
//! Routing APIs (Directions, Matrix, Isochrone, Map Matching, Optimization)
//! are expected to land as a "Navigation" group of their own.
//!
//! clap has no notion of command groups, and aligns each option heading on
//! its own column, so this renders the whole top-level page itself — one
//! column for every section — and hands it to clap as the root's help
//! template. Nothing else changes: the command tree that parsing,
//! completion, `--schema` and suggestions read is the one `build_app` built,
//! and subcommand help is clap's own, in the same [`styles`].
//!
//! A command missing from [`GROUPS`] still shows, under "Other", so a slip
//! here cannot hide a command — but `every_command_has_a_place` fails first.

use clap::builder::styling::{Style, Styles};
use clap::builder::StyledStr;
use clap::{Arg, Command};

/// Help groups in display order, each listing its commands in display order.
const GROUPS: &[(&str, &[&str])] = &[
    (
        "Maps and data",
        &["styles", "sprites", "fonts", "tilesets", "static"],
    ),
    ("Search", &["search", "geocoder", "feedback"]),
    ("Account", &["auth", "accounts", "usage"]),
    ("Coding agents", &["mcp", "agent-skills", "generate-skills"]),
    (
        "CLI",
        &[
            "config",
            "history",
            "doctor",
            "completion",
            "tilesets-cli",
            "uninstall",
            "help",
        ],
    ),
];

/// One-line descriptions for this list, where a command's own `about` does
/// not serve: the API commands, which carry their spec's title ("Mapbox
/// Tokens API") — that says which API, not what the command does — clap's
/// `help`, and a hand-written `about` too long for one line here. Used in
/// this list only; `--schema` and generated skills keep the command's own.
const DESCRIPTIONS: &[(&str, &str)] = &[
    ("styles", "Create, read, update and delete map styles"),
    ("sprites", "Add and remove images in a style's sprite"),
    ("fonts", "List, upload and delete fonts"),
    (
        "tilesets",
        "Fetch vector and raster tiles, and query features at a point",
    ),
    ("static", "Render static map images and tiles"),
    (
        "search",
        "Find addresses and places by text, coordinate or category",
    ),
    ("geocoder", "Forward, reverse and batch geocoding"),
    ("feedback", "Submit and list feedback about Mapbox data"),
    ("accounts", "List access tokens and their scopes"),
    (
        "generate-skills",
        "Write Agent Skills describing this CLI's own commands",
    ),
    ("help", "Print help for mapbox or a command"),
];

/// Where the rest of Mapbox's agent tooling lives. An agent that only
/// installed the CLI finds these here or nowhere: it reads `--help` when
/// stuck, and the install-time pointers are easy to skip.
/// `tests/output_contract.rs` checks each URL is printed, not that it still
/// resolves; that is checked by hand.
const LINKS: &[(&str, &str)] = &[
    ("CLI docs", "https://docs.mapbox.com/cli/"),
    (
        "Agent setup",
        "https://cli.mapbox.com/agent-setup/prompt.md",
    ),
    (
        "Agent Skills",
        "https://github.com/mapbox/mapbox-agent-skills",
    ),
    ("MCP server", "https://github.com/mapbox/mcp-server"),
    ("API docs", "https://docs.mapbox.com/api/overview/"),
];

const OTHER: &str = "Other";
const OPTIONS: &str = "Options";

/// The widest the page gets, terminal or not: past 100 columns a
/// description runs too far from its flag to read as one line.
const MAX_WIDTH: usize = 100;

/// Gives `app` a help template holding the whole top-level page. Call it
/// last: the page is rendered from the commands and arguments `app` already
/// has.
pub fn apply(app: Command) -> Command {
    let mut sections = command_sections(&app);
    sections.extend(option_sections(&app));
    sections.push((
        "Learn more".to_string(),
        LINKS
            .iter()
            .map(|(label, url)| Row::new(label, label.to_string(), vec![Piece::plain(url)]))
            .collect(),
        Block::Names,
    ));

    let page = render(&sections, page_width(), &Palette::new(app.get_styles()));
    // Rendered now rather than through `{usage}`: help is shown by the
    // parsing copy, whose global options are hidden (see `for_parsing`), and
    // clap would drop `[OPTIONS]` from a usage line with none visible.
    let usage = app.clone().render_usage().ansi().to_string();
    let template = format!("{{before-help}}{{about-with-newline}}\n{usage}\n\n{page}");
    app.help_template(StyledStr::from(template))
}

/// The styles this page uses, read from the ones clap was given (see
/// `output::theme`), so a test can render either palette.
struct Palette {
    header: Style,
    literal: Style,
    placeholder: Style,
    /// Notes and hints. clap calls it `context`: its `[env: …]` and
    /// `[default: …]` notes, which these sit beside.
    muted: Style,
}

impl Palette {
    fn new(styles: &Styles) -> Self {
        Palette {
            header: *styles.get_header(),
            literal: *styles.get_literal(),
            placeholder: *styles.get_placeholder(),
            muted: *styles.get_context(),
        }
    }
}

/// A word, or a run of words that must stay on one line, and how it looks.
/// `prefix` and `suffix` are plain punctuation touching a styled piece with
/// no space between: the parentheses in "(`tilesets`)".
struct Piece {
    prefix: String,
    text: String,
    kind: Kind,
    suffix: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Plain,
    Literal,
    Note,
}

impl Piece {
    fn new(text: &str, kind: Kind) -> Self {
        Piece {
            prefix: String::new(),
            text: text.to_string(),
            kind,
            suffix: String::new(),
        }
    }

    fn plain(text: &str) -> Self {
        Piece::new(text, Kind::Plain)
    }

    fn width(&self) -> usize {
        [&self.prefix, &self.text, &self.suffix]
            .iter()
            .map(|s| s.chars().count())
            .sum()
    }
}

/// One line of a section: what to type (or a label) on the left, what it
/// does on the right.
struct Row {
    left_width: usize,
    left: String,
    description: Vec<Piece>,
}

impl Row {
    fn new(plain_left: &str, styled_left: String, description: Vec<Piece>) -> Self {
        Row {
            left_width: plain_left.chars().count(),
            left: styled_left,
            description,
        }
    }
}

/// A heading, its rows, and which column its descriptions start on.
type Section = (String, Vec<Row>, Block);

/// Commands (and the link labels beside them) share one description column,
/// options another. One column for everything let the longest option —
/// `-u, --username <USERNAME>` — push every command's description a dozen
/// columns away from its name.
#[derive(Clone, Copy, PartialEq)]
enum Block {
    Names,
    Options,
}

fn command_sections(app: &Command) -> Vec<Section> {
    let literal = *app.get_styles().get_literal();
    grouped(rows(app))
        .into_iter()
        .map(|(heading, rows)| {
            let rows = rows
                .into_iter()
                .map(|(name, about)| {
                    Row::new(&name, format!("{literal}{name}{literal:#}"), pieces(&about))
                })
                .collect();
            (heading.to_string(), rows, Block::Names)
        })
        .collect()
}

/// The root's own options under their `help_heading`s, in the order the
/// headings first appear. `-h` and `-V` are clap's and carry no heading;
/// they close the last section rather than sit alone in one.
fn option_sections(app: &Command) -> Vec<Section> {
    let palette = Palette::new(app.get_styles());
    let mut sections: Vec<Section> = Vec::new();
    for arg in app
        .get_arguments()
        .filter(|a| !a.is_hide_set() && !a.is_positional())
    {
        let heading = arg.get_help_heading().unwrap_or(OPTIONS);
        let row = option_row(arg, &palette);
        match sections.iter_mut().find(|(h, _, _)| h == heading) {
            Some((_, rows, _)) => rows.push(row),
            None => sections.push((heading.to_string(), vec![row], Block::Options)),
        }
    }

    let mut builtin = vec![flag_row(Some('h'), "help", None, "Print help", &palette)];
    if app.get_version().is_some() && !app.is_disable_version_flag_set() {
        builtin.push(flag_row(
            Some('V'),
            "version",
            None,
            "Print version",
            &palette,
        ));
    }
    match sections.last_mut() {
        Some((_, rows, _)) => rows.extend(builtin),
        None => sections.push((OPTIONS.to_string(), builtin, Block::Options)),
    }
    sections
}

fn option_row(arg: &Arg, palette: &Palette) -> Row {
    let value = arg.get_action().takes_values().then(|| {
        arg.get_value_names()
            .and_then(|names| names.first().map(ToString::to_string))
            .unwrap_or_else(|| arg.get_id().as_str().to_uppercase())
    });
    let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
    let env = arg.get_env().map(|e| e.to_string_lossy().into_owned());
    flag_row(
        arg.get_short(),
        arg.get_long().unwrap_or_default(),
        value,
        &help,
        palette,
    )
    .with_env(env)
}

fn flag_row(
    short: Option<char>,
    long: &str,
    value: Option<String>,
    help: &str,
    palette: &Palette,
) -> Row {
    let (lit, ph) = (palette.literal, palette.placeholder);
    let (short_plain, short_styled) = match short {
        Some(c) => (format!("-{c}, "), format!("{lit}-{c}{lit:#}, ")),
        None => ("    ".to_string(), "    ".to_string()),
    };
    let (value_plain, value_styled) = match &value {
        Some(v) => (format!(" <{v}>"), format!(" {ph}<{v}>{ph:#}")),
        None => (String::new(), String::new()),
    };
    let plain = format!("{short_plain}--{long}{value_plain}");
    let styled = format!("{short_styled}{lit}--{long}{lit:#}{value_styled}");
    Row::new(&plain, styled, pieces(help))
}

impl Row {
    /// Appends the arg's environment variable as a note. A help text
    /// that names one by hand — `--timeout` and `--output` read theirs
    /// leniently rather than through clap — has its note kept whole the same way.
    fn with_env(mut self, env: Option<String>) -> Self {
        if let Some(name) = env {
            self.description
                .push(Piece::new(&format!("[env: {name}]"), Kind::Note));
        }
        self
    }
}

/// Splits help text into pieces: words, `code` spans kept whole and shown
/// without their backticks, and a trailing `[env: …]` note kept whole.
fn pieces(text: &str) -> Vec<Piece> {
    let (body, note) = match text.rfind(" [env: ") {
        Some(at) if text.ends_with(']') => (&text[..at], Some(&text[at + 1..])),
        _ => (text, None),
    };

    let mut out: Vec<Piece> = Vec::new();
    // Text touching a code span with no space between — "(" before it, ","
    // after it — belongs to the span, not to a word of its own.
    let mut prefix = String::new();
    for (i, part) in body.split('`').enumerate() {
        if i % 2 == 1 {
            let mut piece = Piece::new(part, Kind::Literal);
            piece.prefix = std::mem::take(&mut prefix);
            out.push(piece);
            continue;
        }
        let mut words: Vec<&str> = part.split_whitespace().collect();
        if !part.starts_with(char::is_whitespace) && !words.is_empty() {
            if let Some(prev) = out.last_mut().filter(|p| p.kind == Kind::Literal) {
                prev.suffix = words.remove(0).to_string();
            }
        }
        if !part.ends_with(char::is_whitespace) && i + 1 < body.split('`').count() {
            prefix = words.pop().unwrap_or_default().to_string();
        }
        out.extend(words.into_iter().map(Piece::plain));
    }
    out.extend(note.map(|n| Piece::new(n, Kind::Note)));
    out
}

fn render(sections: &[Section], width: usize, palette: &Palette) -> String {
    let column = |block: Block| {
        2 + sections
            .iter()
            .filter(|(_, _, b)| *b == block)
            .flat_map(|(_, rows, _)| rows.iter().map(|r| r.left_width))
            .max()
            .unwrap_or(0)
            + 2
    };
    let header = palette.header;

    let mut page = String::new();
    for (heading, rows, block) in sections {
        let column = column(*block);
        let room = width.saturating_sub(column).max(20);
        page.push_str(&format!("{header}{heading}:{header:#}\n"));
        for row in rows {
            page.push_str("  ");
            page.push_str(&row.left);
            let mut lines = wrap(&row.description, room).into_iter();
            if let Some(first) = lines.next() {
                page.push_str(&" ".repeat(column - 2 - row.left_width));
                page.push_str(&line_text(&first, palette));
            }
            for line in lines {
                page.push('\n');
                page.push_str(&" ".repeat(column));
                page.push_str(&line_text(&line, palette));
            }
            page.push('\n');
        }
        page.push('\n');
    }
    let lit = palette.literal;
    page.push_str(&format!(
        "{muted}Run{muted:#} {lit}mapbox <command> --help{lit:#} \
         {muted}for a command's own options.{muted:#}",
        muted = palette.muted,
    ));
    page
}

/// Greedy word wrap of `pieces` into lines no wider than `room`. A piece
/// wider than `room` gets a line of its own rather than being split.
fn wrap(pieces: &[Piece], room: usize) -> Vec<Vec<&Piece>> {
    let mut lines: Vec<Vec<&Piece>> = Vec::new();
    let mut current: Vec<&Piece> = Vec::new();
    let mut used = 0;
    for piece in pieces {
        let len = piece.width();
        let needed = if current.is_empty() {
            len
        } else {
            used + 1 + len
        };
        if !current.is_empty() && needed > room {
            lines.push(std::mem::take(&mut current));
            used = len;
        } else {
            used = needed;
        }
        current.push(piece);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// A wrapped line's pieces, joined. Only notes are styled: in a
/// description a code span is prose, and the names on the left are what is
/// bold.
fn line_text(line: &[&Piece], palette: &Palette) -> String {
    let muted = palette.muted;
    line.iter()
        .map(|piece| match piece.kind {
            Kind::Note => format!(
                "{}{muted}{}{muted:#}{}",
                piece.prefix, piece.text, piece.suffix
            ),
            Kind::Plain | Kind::Literal => {
                format!("{}{}{}", piece.prefix, piece.text, piece.suffix)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The terminal's width, as clap would read it, capped at [`MAX_WIDTH`].
fn page_width() -> usize {
    let detected = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|c| *c > 0)
        .or_else(|| terminal_size::terminal_size().map(|(w, _)| usize::from(w.0)));
    detected.map_or(MAX_WIDTH, |w| w.min(MAX_WIDTH))
}

/// Whether argv asks for help, read before clap parses it: `-h` or `--help`
/// anywhere before `--`, or `help` as the first word that is not an option.
/// The one kind of run allowed to ask the terminal for its background (see
/// `output::theme::allow_background_query`). A miss costs only the fallback
/// palette; a false match on a command that prompts would cost the answer
/// typed ahead, which is why `help` counts only as the subcommand.
pub fn help_requested(argv: &[std::ffi::OsString]) -> bool {
    let mut first_word = true;
    for arg in argv.iter().skip(1).filter_map(|arg| arg.to_str()) {
        match arg {
            "--" => return false,
            "-h" | "--help" => return true,
            "help" if first_word => return true,
            _ if !arg.starts_with('-') => first_word = false,
            _ => {}
        }
    }
    false
}

/// Whether argv asks for the top-level page itself — `mapbox --help`,
/// `mapbox -h` or `mapbox help` with no command named — the help that opens
/// with the banner. Options before it don't count as words, though one that
/// takes a value (`-o text --help`) is missed, which costs only the banner.
pub fn top_level_help_requested(argv: &[std::ffi::OsString]) -> bool {
    let args: Vec<&str> = argv
        .iter()
        .skip(1)
        .filter_map(|arg| arg.to_str())
        .take_while(|arg| *arg != "--")
        .collect();
    let words: Vec<&str> = args
        .iter()
        .copied()
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    match words.as_slice() {
        [] => args.iter().any(|arg| matches!(*arg, "-h" | "--help")),
        ["help"] => true,
        _ => false,
    }
}

/// The copy of the tree that parses, and so renders every subcommand's
/// help: compact, and about the command itself.
///
/// clap switches `--help` to its long layout whenever a command has a
/// `long_about`, and nearly every command here does: each option's
/// description on a line of its own, a blank line between options, and
/// `[env: …]` as a paragraph. Here the long description moves to the top of
/// the command's own page instead, which leaves clap nothing long to show.
///
/// The eleven global options are listed on the top-level page and named in
/// one line on every other. Spelled out on each, they outweighed what the
/// command itself takes, and clap aligned each of their headings on a
/// column of its own. Hidden only from help: they still parse, and clap
/// still suggests them for a typo.
///
/// Only the parsing copy: `--schema`, completion and `generate-skills` read
/// the tree `build_app` returned. Backticks go too — in a terminal they are
/// noise, not markup.
pub fn for_parsing(app: Command) -> Command {
    let footer = global_options_footer(&app);
    app.mut_args(|arg| {
        let arg = plain_arg_help(arg);
        if arg.is_global_set() {
            arg.hide(true)
        } else {
            arg
        }
    })
    .mut_subcommands(|cmd| compact(cmd, &footer))
}

/// One line closing a subcommand's help, standing in for the global
/// options. Phrased like the top-level page's closing line, and like
/// kubectl's pointer to its global options.
fn global_options_footer(app: &Command) -> StyledStr {
    let styles = app.get_styles();
    let (lit, muted) = (*styles.get_literal(), *styles.get_context());
    StyledStr::from(format!(
        "{muted}Run{muted:#} {lit}mapbox --help{lit:#} \
         {muted}for the global options, which apply to every command.{muted:#}"
    ))
}

fn compact(cmd: Command, footer: &StyledStr) -> Command {
    let long = cmd
        .get_long_about()
        .map(|text| without_backticks(&text.to_string()));
    let about = cmd
        .get_about()
        .map(|text| without_backticks(&text.to_string()));
    let mut cmd = cmd
        .mut_args(plain_arg_help)
        .mut_subcommands(|sub| compact(sub, footer))
        .after_help(footer.clone());
    if let Some(about) = about {
        cmd = cmd.about(about);
    }
    match long {
        // `before_help` shows on the command's own page and not in its
        // parent's list, where `about` stays the one-line summary.
        Some(long) => cmd
            .long_about(None::<&'static str>)
            .before_help(long)
            .help_template("{before-help}{usage-heading} {usage}\n\n{all-args}{after-help}"),
        None => cmd,
    }
}

fn plain_arg_help(arg: Arg) -> Arg {
    match arg
        .get_help()
        .map(|help| without_backticks(&help.to_string()))
    {
        Some(help) => arg.help(help),
        None => arg,
    }
}

fn without_backticks(text: &str) -> String {
    text.replace('`', "")
}

/// `(name, description)` for each command the help lists. clap adds its
/// `help` command only while building the tree, which has not happened yet
/// and should not happen early: `--schema` reads the unbuilt tree.
fn rows(app: &Command) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = app
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| {
            let name = c.get_name().to_string();
            let about = description(&name)
                .map(str::to_string)
                .or_else(|| c.get_about().map(ToString::to_string))
                .unwrap_or_default();
            (name, about)
        })
        .collect();
    if !app.is_disable_help_subcommand_set() && !rows.is_empty() {
        rows.push((
            "help".to_string(),
            description("help").unwrap_or_default().to_string(),
        ));
    }
    rows
}

/// `rows` sorted into [`GROUPS`], with anything unlisted under "Other".
fn grouped(rows: Vec<(String, String)>) -> Vec<(&'static str, Vec<(String, String)>)> {
    let mut groups: Vec<(&'static str, Vec<(String, String)>)> = GROUPS
        .iter()
        .map(|(heading, names)| {
            let listed = names
                .iter()
                .filter_map(|n| rows.iter().find(|(name, _)| name == n).cloned())
                .collect();
            (*heading, listed)
        })
        .filter(|(_, listed): &(_, Vec<_>)| !listed.is_empty())
        .collect();

    let unlisted: Vec<(String, String)> = rows
        .into_iter()
        .filter(|(name, _)| {
            !GROUPS
                .iter()
                .any(|(_, names)| names.contains(&name.as_str()))
        })
        .collect();
    if !unlisted.is_empty() {
        groups.push((OTHER, unlisted));
    }
    groups
}

fn description(name: &str) -> Option<&'static str> {
    DESCRIPTIONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, text)| *text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> Command {
        crate::build_app(&crate::spec::effective_services().expect("the bundled specs parse"))
    }

    /// The guard behind the "Other" fallback: a command added without a
    /// place in [`GROUPS`], or an entry left behind by a rename, fails here
    /// rather than drifting into a group nobody chose.
    #[test]
    fn every_command_has_a_place() {
        let rows = rows(&app());
        let visible: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();

        let mut problems = Vec::new();
        for name in &visible {
            let places = GROUPS
                .iter()
                .filter(|(_, names)| names.contains(name))
                .count();
            match places {
                1 => {}
                0 => problems.push(format!(
                    "`{name}` is not in any help group. Add it to GROUPS in src/help_layout.rs."
                )),
                _ => problems.push(format!(
                    "`{name}` is in {places} help groups. Keep it in one."
                )),
            }
        }
        for (_, names) in GROUPS {
            for name in *names {
                if !visible.contains(name) {
                    problems.push(format!(
                        "`{name}` is in GROUPS but is not a command. Remove it from src/help_layout.rs."
                    ));
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// An API command's own `about` is its spec's title, which says which
    /// API rather than what the command does.
    #[test]
    fn every_api_command_has_a_description() {
        let specs = crate::spec::effective_services().expect("the bundled specs parse");
        let missing: Vec<String> = specs
            .iter()
            .map(|svc| svc.name.to_string())
            .filter(|name| !DESCRIPTIONS.iter().any(|(n, _)| n == name))
            .map(|name| {
                format!(
                    "`{name}` has no description. Add one to DESCRIPTIONS in src/help_layout.rs."
                )
            })
            .collect();
        assert!(missing.is_empty(), "{}", missing.join("\n"));
    }

    /// Help and clap's errors use bold and the palettes' fixed colors, never
    /// one of the sixteen a terminal theme decides — and without 24-bit
    /// color, no color at all. See `output::theme`.
    #[test]
    fn theme_safe_colors_only() {
        use crate::output::theme::{styles_in, Look, LOOKS};
        for &look in LOOKS {
            let truecolor = look != Look::Bold;
            let app = app().styles(styles_in(look));
            let parsing = for_parsing(apply(app));
            for argv in [
                &["mapbox", "--help"][..],
                &["mapbox", "tilesets", "query", "--help"],
                &["mapbox", "help", "styles"],
                &["mapbox", "--versiomn"],
                &["mapbox", "styles", "list", "--outptu", "json"],
                &["mapbox", "styles", "get"],
            ] {
                let err = parsing
                    .clone()
                    .try_get_matches_from(argv)
                    .expect_err("help and mistakes stop the parse");
                let text = err.render().ansi().to_string();
                let problems = unsafe_styles(&text, truecolor);
                assert!(
                    problems.is_empty(),
                    "{argv:?} ({look:?}) uses {problems:?}:\n{text}"
                );
            }
        }
    }

    /// The SGR parameters in `text` that are neither a reset, bold, nor —
    /// with `truecolor` — a 24-bit color from one of the palettes.
    fn unsafe_styles(text: &str, truecolor: bool) -> Vec<String> {
        let palette: Vec<String> = crate::output::theme::colors()
            .iter()
            .map(|c| format!("{};{};{}", c.0, c.1, c.2))
            .collect();
        let mut problems = Vec::new();
        for sequence in text.split("\x1b[").skip(1) {
            let params: Vec<&str> = sequence
                .split('m')
                .next()
                .unwrap_or_default()
                .split(';')
                .collect();
            let mut i = 0;
            while i < params.len() {
                match params[i] {
                    "" | "0" | "1" | "22" | "39" => i += 1,
                    "38" if truecolor
                        && params.get(i + 1) == Some(&"2")
                        && params.len() >= i + 5 =>
                    {
                        let rgb = params[i + 2..i + 5].join(";");
                        if !palette.contains(&rgb) {
                            problems.push(format!("38;2;{rgb}"));
                        }
                        i += 5;
                    }
                    other => {
                        problems.push(other.to_string());
                        i += 1;
                    }
                }
            }
        }
        problems.sort();
        problems.dedup();
        problems
    }

    #[test]
    fn help_is_recognized_only_where_it_asks_for_help() {
        let asks = |args: &[&str]| {
            let argv: Vec<std::ffi::OsString> = std::iter::once("mapbox")
                .chain(args.iter().copied())
                .map(Into::into)
                .collect();
            help_requested(&argv)
        };
        assert!(asks(&["--help"]));
        assert!(asks(&["styles", "list", "-h"]));
        assert!(asks(&["help", "styles"]));
        assert!(asks(&["--no-color", "help"]));
        // A style named "help" is a value, not a request: deleting it
        // prompts, and the prompt must keep its answer.
        assert!(!asks(&["styles", "delete", "help"]));
        assert!(!asks(&["tilesets-cli", "--", "--help"]));
        assert!(!asks(&["styles", "list"]));
    }

    #[test]
    fn only_the_top_level_page_counts_as_top_level_help() {
        let asks = |args: &[&str]| {
            let argv: Vec<std::ffi::OsString> = std::iter::once("mapbox")
                .chain(args.iter().copied())
                .map(Into::into)
                .collect();
            top_level_help_requested(&argv)
        };
        assert!(asks(&["--help"]));
        assert!(asks(&["-h"]));
        assert!(asks(&["help"]));
        assert!(asks(&["--no-color", "--help"]));
        assert!(!asks(&["styles", "--help"]));
        assert!(!asks(&["help", "styles"]));
        assert!(!asks(&[]));
    }

    fn shown(text: &str) -> Vec<String> {
        pieces(text)
            .iter()
            .map(|p| format!("{}{}{}", p.prefix, p.text, p.suffix))
            .collect()
    }

    /// A code span loses its backticks but keeps the punctuation that
    /// touches it, with no space added on either side.
    #[test]
    fn a_code_span_keeps_the_punctuation_around_it() {
        assert_eq!(
            shown("Run the Tilesets CLI (`tilesets`, installed separately)"),
            [
                "Run",
                "the",
                "Tilesets",
                "CLI",
                "(tilesets,",
                "installed",
                "separately)"
            ]
        );
        assert_eq!(
            shown("Use `mapbox auth login` credentials"),
            ["Use", "mapbox auth login", "credentials"]
        );
    }

    #[test]
    fn a_trailing_env_note_is_one_piece() {
        let pieces = pieces("Seconds per request [env: MAPBOX_TIMEOUT]");
        let last = pieces.last().expect("pieces");
        assert_eq!(last.text, "[env: MAPBOX_TIMEOUT]");
        assert!(last.kind == Kind::Note);
    }

    #[test]
    fn an_unlisted_command_still_shows_under_other() {
        let rows = vec![
            ("styles".to_string(), String::new()),
            ("brand-new".to_string(), String::new()),
        ];

        let groups = grouped(rows);
        let other = groups.iter().find(|(heading, _)| *heading == OTHER);
        assert_eq!(
            other.map(|(_, rows)| rows
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()),
            Some(vec!["brand-new"])
        );
    }
}
