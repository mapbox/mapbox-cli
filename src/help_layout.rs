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
//! clap has no notion of command groups, so this renders the list itself and
//! hands it to clap as the root's help template. Nothing else changes: the
//! command tree that parsing, completion, `--schema` and suggestions read is
//! the one `build_app` built.
//!
//! A command missing from [`GROUPS`] still shows, under "Other", so a slip
//! here cannot hide a command — but `every_command_has_a_place` fails first.

use clap::builder::styling::Styles;
use clap::builder::StyledStr;
use clap::Command;

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

/// One-line descriptions for commands whose own `about` this crate does not
/// write: the API commands, which carry their spec's title ("Mapbox Tokens
/// API") — that says which API, not what the command does — and clap's
/// `help`. Used in this list only; `--schema` and `generate-skills` keep the
/// spec's wording.
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
    ("help", "Print help for mapbox or a command"),
];

const OTHER: &str = "Other";

/// Gives `app` a help template that lists its commands by group. Call it
/// last: the options section is rendered here, from the arguments `app`
/// already has.
pub fn apply(app: Command) -> Command {
    let styles = Styles::default();
    let header = *styles.get_header();
    let literal = *styles.get_literal();

    let rows = rows(&app);
    let width = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);

    let mut commands = String::new();
    for (heading, rows) in grouped(rows) {
        commands.push_str(&format!("{header}{heading}:{header:#}\n"));
        for (name, about) in rows {
            commands.push_str(&format!(
                "  {literal}{name}{literal:#}{pad}  {about}\n",
                pad = " ".repeat(width - name.len()),
            ));
        }
        commands.push('\n');
    }

    // clap writes its single "Commands:" list into `{all-args}` whenever
    // the command has a visible subcommand, so the options come from a copy
    // with none. Hiding them on `app` itself would drop them from
    // completion and suggestions too.
    let options = app
        .clone()
        .mut_subcommands(|c| c.hide(true))
        .disable_help_subcommand(true)
        .help_template("{all-args}")
        .render_help();

    let template = format!(
        "{{before-help}}{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{commands}{}{{after-help}}",
        // `{after-help}` brings its own blank line.
        options.ansi().to_string().trim_end()
    );
    app.help_template(StyledStr::from(template))
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
