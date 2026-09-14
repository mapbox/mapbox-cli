//! Locks the name and every alias of every API command down against a
//! checked-in fixture, so a change to either shows up as a failing test
//! rather than as a caller's script breaking after a merge.
//!
//! What this adds over `tests/docs_contract.rs`, which also notices a
//! renamed command:
//!
//!   * **Aliases**, which is the whole reason this file exists. A hidden
//!     alias is published nowhere on purpose — not `--help`, not `--schema`,
//!     not `docs/commands.md` — so `docs_contract.rs`, which compares the
//!     page against the schema, cannot see one appear, vanish, or swap
//!     places with the command name. A `spec::COMMAND_ALIASES` row deleted
//!     by hand would take
//!     `mapbox tilequery get-v4tilesets-tilequery-lon-lat-json` away from
//!     everyone still typing it and break no other test in this repo.
//!   * **A name it can compare against**, for the renames both files do see.
//!     `docs_contract.rs` fails when the page and the binary disagree, which
//!     they will after an upstream `operationId` rename — but the fix is to
//!     edit the page, and once edited nothing records that a published
//!     command name changed. This fixture is the record: the diff names the
//!     old spelling and the new one, in a file a reviewer reads.
//!
//! A unit test inside the crate rather than an integration test under
//! `tests/`, which is what every other contract test here is. Those run the
//! built binary and read `--schema`, and that is exactly what cannot work:
//! a hidden alias is absent from the schema by design, so a fixture built
//! from the schema records `(none)` for the one command that has an alias
//! and pins the surface as if it had none. `clap`'s own
//! [`Command::get_all_aliases`] is the only thing that can see one, and
//! reaching it needs the command tree in-process. Being a child module of
//! the crate root is what buys that — `crate::build_app` stays private and
//! `--schema` stays unchanged.
//!
//! Regenerating the fixture after an intentional rename or alias change:
//! `UPDATE_API_COMMAND_SURFACE=1 cargo test --bins api_command_surface`,
//! which rewrites `tests/fixtures/api_command_surface.txt` instead of
//! failing. Then `git diff` it to see exactly what changed before
//! committing.

use std::path::PathBuf;

use clap::Command;

use crate::spec::{effective_services, ServiceSpec};

const FIXTURE: &str = "tests/fixtures/api_command_surface.txt";

fn bundled_specs() -> Vec<ServiceSpec> {
    effective_services().expect("the bundled specs parse")
}

/// One line per API command: its full `mapbox <service> <name>` and every
/// alias, sorted so the file's own diff is the only thing that changes when
/// the surface does.
///
/// Driven off the specs rather than off the tree's subcommands, so "API
/// command" means the same thing it means in [`crate::schema`]: generated
/// from an exposed operation, not hand-written and not the passthrough. The
/// aliases then come off the tree, which is the only place a hidden one
/// exists.
fn surface_lines(app: &Command, specs: &[ServiceSpec]) -> Vec<String> {
    let mut lines: Vec<String> = vec![];

    for svc in specs {
        for op in svc.operations.iter().filter(|op| op.is_exposed()) {
            // Down the whole command path, since one can nest: `styles draft
            // get` is three levels from the root.
            let cmd = op
                .command_path
                .iter()
                .try_fold(
                    app.find_subcommand(&svc.name).unwrap_or_else(|| {
                        panic!("`mapbox {}` has operations and is not a command", svc.name)
                    }),
                    |cmd, segment| cmd.find_subcommand(segment),
                )
                .unwrap_or_else(|| {
                    panic!(
                        "`mapbox {}` is an exposed operation and not a command",
                        op.command()
                    )
                });

            // `(hidden)` on the ones nothing publishes, so flipping a
            // `COMMAND_ALIASES` row's `show_generated_name` — which swaps a
            // command's name with its alias and moves the other between
            // visible and hidden — moves this line instead of leaving it
            // alone.
            let visible: Vec<&str> = cmd.get_visible_aliases().collect();
            let mut aliases: Vec<String> = cmd
                .get_all_aliases()
                .map(|alias| {
                    if visible.contains(&alias) {
                        alias.to_string()
                    } else {
                        format!("{alias} (hidden)")
                    }
                })
                .collect();
            aliases.sort();

            let aliases = if aliases.is_empty() {
                "(none)".to_string()
            } else {
                aliases.join(", ")
            };
            lines.push(format!("mapbox {} | aliases: {aliases}", op.command()));
        }
    }

    lines.sort();
    lines
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
}

#[test]
fn the_api_command_surface_matches_the_checked_in_fixture() {
    let specs = bundled_specs();
    let current = surface_lines(&crate::build_app(&specs), &specs);
    let path = fixture_path();

    if std::env::var("UPDATE_API_COMMAND_SURFACE").is_ok() {
        std::fs::write(&path, current.join("\n") + "\n").unwrap_or_else(|e| {
            panic!("write {}: {e}", path.display());
        });
        return;
    }

    let recorded = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e}\n\
             This fixture doesn't exist yet — create it by running this test once with \
             UPDATE_API_COMMAND_SURFACE=1 set.",
            path.display()
        )
    });
    let recorded: Vec<&str> = recorded.lines().collect();

    if current != recorded {
        let added: Vec<&String> = current
            .iter()
            .filter(|l| !recorded.contains(&l.as_str()))
            .collect();
        let removed: Vec<&&str> = recorded
            .iter()
            .filter(|l| !current.contains(&l.to_string()))
            .collect();
        panic!(
            "the API command surface no longer matches {FIXTURE}.\n\
             \n\
             Added:\n{}\n\
             Removed:\n{}\n\
             \n\
             If this is intentional — a spec rename, a deliberate alias change — rerun with \
             UPDATE_API_COMMAND_SURFACE=1 set to rewrite the fixture, then review the diff \
             before committing it. If it isn't, something renamed a command or changed an \
             alias without meaning to.",
            if added.is_empty() {
                "  (none)".to_string()
            } else {
                added
                    .iter()
                    .map(|l| format!("  + {l}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            if removed.is_empty() {
                "  (none)".to_string()
            } else {
                removed
                    .iter()
                    .map(|l| format!("  - {l}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        );
    }
}
