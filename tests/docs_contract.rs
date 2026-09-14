//! Holds `docs/commands.md` to the surface the binary reports.
//!
//! The surface itself cannot drift from the specs. The spec tables in
//! `src/spec.rs` compile them in with `include_str!`, so a spec change
//! rebuilds the commands, a spec renamed upstream is a broken build, and a
//! new parameter named after a global fails `no_generated_flag_shadows_a_global`.
//! `docs/commands.md` is the half with none of that: nothing in this repo
//! writes it, and its **Parameters** tables were transcribed from the specs
//! by hand. So the drift that actually happens runs one way — a spec moves,
//! the CLI follows it for free, and the page goes on describing a CLI that no
//! longer exists.
//!
//! Nothing here reaches the network, and that is the point rather than a
//! detail. The other way to notice a stale page is a scheduled job that runs
//! the live API and diffs the captures, which would need a standing secret in
//! a repo whose workflows hold none, would report days after the change, and
//! would be monitoring an API this repo does not own. What is checkable
//! without a token, in `cargo test`, at the moment the change is made, is
//! whether the page has fallen behind our own binary. That is all this file
//! claims to check.
//!
//! Three things it deliberately does not do, written down so the next reader
//! does not have to re-derive the scope:
//!
//!   * It does not enforce every flag. A flag the page declares page-wide is
//!     exempted for all 60 commands, not only for the ones that actually
//!     share it, so 93 of the 176 flag-bearing arguments are enforced and 83
//!     are not. `--data`, `--file` and `--dry-run` are the page-wide
//!     declarations that are *not* in `global_options`: the first two are on
//!     a handful of operations and the third on 24 of them.
//!   * It does not ask for `--dry-run` section by section, which is what
//!     would lift that number. Rejected: the page's design is to state a
//!     page-wide fact once, and 24 near-identical paragraphs is the
//!     duplication that design exists to avoid. Where a reader needs the
//!     split spelled out — the four `auth` commands, three of which take the
//!     flag and one of which does not — the page says so in prose.
//!   * It does not check the captured **Outputs** blocks. They are the half
//!     no test can reach: bytes a real account returned once, a dated
//!     snapshot the page labels as one, re-taken by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

/// The page under test, named once. Also what the failure messages call it,
/// so a reader is told which file to open.
const PAGE: &str = "docs/commands.md";

/// A config directory of this test binary's own. See the same function in
/// `output_contract.rs` for why `--profile` alone is not isolation.
fn sandbox_home() -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("docs-contract-home");
    std::fs::create_dir_all(&home).expect("create sandbox home");
    home
}

/// The whole surface, from the binary that was just built.
///
/// The developer's own environment is cleared for the reason it is cleared in
/// `schema_contract.rs`: a token in the running shell would let a mistake
/// here reach the API, and this file has no business touching it.
fn schema() -> Value {
    let home = sandbox_home();
    let out = Command::new(env!("CARGO_BIN_EXE_mapbox"))
        .env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("MAPBOX_CONFIG_DIR", home.join(".mapbox"))
        .arg("--schema")
        .output()
        .expect("run mapbox");

    assert!(
        out.status.success(),
        "`mapbox --schema` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("`mapbox --schema` prints one JSON document")
}

/// Reached through `CARGO_MANIFEST_DIR` rather than a path relative to the
/// working directory, which `cargo test` does not promise.
fn page() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(PAGE);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn commands(schema: &Value) -> &Vec<Value> {
    schema["commands"].as_array().expect("commands is an array")
}

fn name(command: &Value) -> &str {
    command["command"].as_str().expect("a command name")
}

/// Every flag spelling in one line of the page, added to `found`.
///
/// Hand-rolled rather than a regex so the test adds no dependency, but the
/// character class is the part worth getting right: it has to accept
/// uppercase. A `[a-z-]+` scanner truncates `--sourceEncoding` to `--source`
/// and `--mapboxGLVersion` to `--mapbox`, which then read as two flags the
/// page had failed to mention — drift the scanner invented rather than found.
///
/// Over-matching is safe in the other direction and not worth guarding: this
/// runs only over the page, and the set it fills is only ever asked whether
/// it holds a flag some command actually declares. `-rw-r--r--` in a captured
/// `ls -l` line yields `--r--`, which no command will ever be looking for.
fn flags_in(line: &str, found: &mut BTreeSet<String>) {
    let bytes = line.as_bytes();
    for start in 0..bytes.len().saturating_sub(2) {
        if &bytes[start..start + 2] != b"--" || !bytes[start + 2].is_ascii_alphabetic() {
            continue;
        }
        let end = bytes[start + 2..]
            .iter()
            .position(|b| !(b.is_ascii_alphanumeric() || *b == b'-'))
            .map_or(bytes.len(), |offset| start + 2 + offset);
        found.insert(line[start..end].to_string());
    }
}

/// The page's lines, each paired with whether it sits inside a fenced block.
///
/// Both parsers below need the distinction and disagree about what to do with
/// it, which is why it is answered once here rather than in each. A fence
/// delimiter counts as inside: it is neither a heading nor a flag.
fn lines(page: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut fenced = false;
    page.lines().map(move |line| {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            return (line, true);
        }
        (line, fenced)
    })
}

/// The heading level of a line, or `None` when it is not a heading.
fn heading_depth(line: &str) -> Option<usize> {
    let hashes = line.bytes().take_while(|byte| *byte == b'#').count();
    (hashes > 0 && line.as_bytes().get(hashes) == Some(&b' ')).then_some(hashes)
}

/// The command a `### ` heading is about, if it is about one.
///
/// The page writes the tileset proxy's forwarded argv into its heading —
/// ``### `mapbox tilesets-cli <args…>` `` — so a trailing placeholder is
/// dropped before matching. Cutting at `<` is enough because no command name
/// holds one.
fn command_in_heading(line: &str) -> Option<String> {
    let quoted = line.strip_prefix("### `")?.split('`').next()?;
    let named = quoted.split('<').next().unwrap_or(quoted).trim();
    named.starts_with("mapbox").then(|| named.to_string())
}

/// The page's per-command sections: the command each `### ` heading names,
/// against every flag spelled anywhere beneath it.
///
/// The whole section counts — table, prose and worked examples alike, not
/// only the `#### Parameters` table. The page legitimately introduces a flag
/// in prose (forward's structured input is a paragraph naming nine of
/// them, and `--fresh` is one sentence) or in an example, and a check that
/// read only the tables would be enforcing a house style rather than finding
/// drift.
///
/// A `## ` service heading closes a section as surely as the next `### `
/// does. Missing that reads a service's whole trailing prose as part of its
/// last command's section, and then a real gap anywhere after the first
/// command of a service goes unreported.
fn sections(page: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut sections = BTreeMap::new();
    let mut current: Option<String> = None;

    for (line, fenced) in lines(page) {
        // `#### Parameters` and `#### Outputs` are inside a section, so only
        // the levels above one end it — and a `#`-prefixed line inside a
        // captured block is output, not a heading. No capture holds one
        // today; tracking it means the day one does, the section it sits in
        // does not silently close and take its remaining flags with it.
        if !fenced && heading_depth(line).is_some_and(|depth| depth <= 3) {
            current = command_in_heading(line);
            if let Some(command) = &current {
                sections.insert(command.clone(), BTreeSet::new());
            }
            continue;
        }
        if let Some(command) = &current {
            flags_in(line, sections.get_mut(command).expect("the open section"));
        }
    }

    sections
}

/// The flags the page declares once, for every command.
///
/// Read out of the page rather than listed here: the table under `### What
/// every API command takes`, and the prose below it that declares `--data`
/// and `--file`. Parsing them from the page means promoting a flag to a
/// global stays one edit, in the place a reader looks for it, and this test
/// follows that edit instead of having to be told about it.
///
/// Fenced blocks in that section are skipped. The two example command lines
/// there reach for `--q`, and a flag written into an example is being used,
/// not declared for everything.
fn declared_once_for_every_command(page: &str) -> BTreeSet<String> {
    let mut flags = BTreeSet::new();
    let mut inside = false;

    for (line, fenced) in lines(page) {
        if fenced {
            continue;
        }
        if heading_depth(line).is_some_and(|depth| depth <= 3) {
            inside = line.trim_end() == "### What every API command takes";
            continue;
        }
        if inside {
            flags_in(line, &mut flags);
        }
    }

    // A heading that gets reworded leaves this empty, and an empty exemption
    // set does not fail loudly — it holds every command to flags the page
    // does answer for elsewhere, and buries the real report under them.
    assert!(
        flags.contains("--data") && flags.contains("--file"),
        "the `### What every API command takes` section parsed to {flags:?}, and \
         `--data`/`--file` are declared there — so the heading this reads moved or was \
         reworded, and the page-wide flags are no longer being found"
    );
    flags
}

/// Which spec revision the binary was built from, for a failure message.
///
/// Both assertions below compare the page against the *binary*, and the
/// binary is built from `openapi/` — a vendored copy that only moves when
/// a maintainer regenerates it. So when they disagree there are two
/// candidate culprits, and the message used to name only one of them: the
/// page.
///
/// That is not a hypothetical. Specs three weeks old reported
/// `sources get-datasource` as a command the CLI no longer has — the
/// operation had been briefly absent upstream and was restored days later —
/// and the failure read as "delete the section". Two reviewers and a
/// maintainer took it at face value, while `build.rs` printed a staleness
/// warning on every one of those builds.
///
/// So this says which revision the binary was built from, and — the half
/// that matters — *rules the specs out* when they are fresh, leaving the page
/// as the only remaining explanation.
///
/// The advice changed when the specs moved in-tree: it used to say pull the
/// sibling checkout, which an external clone has no way to do. Now it says
/// plainly that `openapi/` is vendored and regenerating it is a
/// maintainer-only step, rather than pointing at tooling this checkout
/// doesn't have.
fn spec_revision_note() -> String {
    let commit = option_env!("MAPBOX_SPEC_COMMIT");
    let age: Option<u64> = option_env!("MAPBOX_SPEC_AGE_DAYS").and_then(|days| days.parse().ok());

    // Matches `STALE_AFTER_DAYS` in build.rs. Duplicated rather than shared
    // because build.rs cannot export a constant to a test, and a wrong number
    // here costs a wrong hint rather than a wrong result.
    const STALE_AFTER_DAYS: u64 = 14;

    match (commit, age) {
        (Some(commit), Some(days)) if days > STALE_AFTER_DAYS => format!(
            "\n\nBefore editing the page: `openapi/` was derived from openapi-specs \
             {commit}, a commit {days} days old. Specs that far behind are the most \
             common cause of this failure, and the page is usually right. `openapi/` \
             is vendored; regenerating it is a maintainer-only step — open an issue if \
             this page and the specs disagree."
        ),
        (Some(commit), Some(days)) => format!(
            "\n\n(`openapi/` is derived from openapi-specs {commit}, {days} days old, so \
             stale specs are not the explanation — the page really has fallen behind the \
             binary.)"
        ),
        (Some(commit), None) => format!(
            "\n\n(`openapi/` is derived from openapi-specs {commit}; its age could not be \
             determined. `openapi/` is vendored; regenerating it is a maintainer-only \
             step — open an issue if this page and the specs disagree.)"
        ),
        _ => "\n\n(The spec revision this was built against could not be determined. \
              `openapi/` is vendored; regenerating it is a maintainer-only step — open \
              an issue if this page and the specs disagree.)"
            .to_string(),
    }
}

#[test]
fn the_page_has_a_section_for_every_command_and_for_nothing_else() {
    let schema = schema();
    let listed: BTreeSet<&str> = commands(&schema).iter().map(name).collect();
    let page = page();
    let sections = sections(&page);
    let documented: BTreeSet<&str> = sections.keys().map(String::as_str).collect();

    let undocumented: Vec<&&str> = listed.difference(&documented).collect();
    assert!(
        undocumented.is_empty(),
        "the CLI ships these commands and {PAGE} has no section for them: {undocumented:?}\n\
         Give each one a `### ` heading holding its name in backticks, under its service, \
         and a line in the Contents list.{}",
        spec_revision_note()
    );

    let invented: Vec<&&str> = documented.difference(&listed).collect();
    assert!(
        invented.is_empty(),
        "{PAGE} has a section for these and the CLI has no such command: {invented:?}\n\
         A command that was renamed or withheld takes its section with it.{}",
        spec_revision_note()
    );
}

/// One direction, deliberately: a flag the command declares has to be named
/// in its section, but a flag named in a section need not belong to that
/// command.
///
/// The reverse is noise rather than drift. A section's prose legitimately
/// reaches for another command's flags — `styles delete` explains itself in
/// terms of `styles list --deleted`, which is `styles list`'s flag and not
/// its own — and a captured `ls -l` line reads `-rw-r--r--` as a flag called
/// `--r--`. A prototype of this check asserted both ways and reported those
/// as drift; none of them was, and a check whose failures have to be triaged
/// by hand is one nobody reads.
#[test]
fn every_flag_a_command_takes_is_named_in_its_own_section() {
    let schema = schema();
    let page = page();
    let sections = sections(&page);

    let mut exempt = declared_once_for_every_command(&page);
    exempt.extend(
        schema["global_options"]
            .as_array()
            .expect("global_options")
            .iter()
            .filter_map(|option| option["flag"].as_str())
            .map(str::to_string),
    );

    let mut unmentioned = Vec::new();
    for command in commands(&schema) {
        let command_name = name(command);
        // A command with no section at all is the other test's report, not a
        // flag gap on every one of its arguments.
        let Some(named) = sections.get(command_name) else {
            continue;
        };
        for argument in command["arguments"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            // Positionals have no flag; the page describes those in prose.
            let Some(flag) = argument["flag"].as_str() else {
                continue;
            };
            if !named.contains(flag) && !exempt.contains(flag) {
                unmentioned.push(format!("{command_name} takes {flag}"));
            }
        }
    }

    assert!(
        unmentioned.is_empty(),
        "{PAGE} never names these flags in the section for the command that takes them:\n\
         {}\n\
         Name each one where somebody reading that command would find it — its `#### \
         Parameters` table, or the prose beside it. If a flag is on every command, put it \
         in the table under `### What every API command takes` instead, which is where \
         this test reads the page-wide flags from.",
        unmentioned.join("\n")
    );
}
