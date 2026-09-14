//! Rules about the source itself, checked by reading it.
//!
//! `src/http.rs`'s `no_module_builds_its_own_client` established the shape:
//! when an invariant cannot be expressed in the type system and a lint does
//! not exist for it, scan the source and fail on the pattern. This file is
//! the home for the ones that are about the crate rather than about one
//! module.
//!
//! A grep is a blunt instrument and these are deliberately blunt. They do not
//! prove a call is correct; they make a call *conspicuous*, so that adding one
//! is a decision somebody made rather than a line in a diff nobody looked at
//! twice.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Every `src/*.rs`, as (file name, contents).
fn sources() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = vec![];
    for entry in std::fs::read_dir(&dir).expect("read src/") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let name = path
                .file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned();
            out.push((name, std::fs::read_to_string(&path).expect("read a source")));
        }
    }
    out.sort();
    assert!(out.len() > 10, "src/ looks empty: {out:?}");
    out
}

/// The modules allowed to delete things from the filesystem.
///
/// Deleting is the operation in this crate with no undo, and the one where a
/// path assembled from somewhere else does the most damage. The list is short
/// on purpose, and every entry deletes something it is *named* for:
///
/// - `agent_skills` — the staging directory it renames skills out of, and the
///   skill directory `install --force` replaces.
/// - `auth` — `logout`, and the scratch file `write_private` renames from.
/// - `executor` — nothing durable; the temp file a `--file` upload streams.
/// - `generate_skills` — the staged skill directory it renames into place.
/// - `skill_dest` — a test scratch directory.
/// - `uninstall` — the binary itself, which is the whole command.
const MAY_DELETE: &[&str] = &[
    "agent_skills.rs",
    "auth.rs",
    "executor.rs",
    "generate_skills.rs",
    "skill_dest.rs",
    "uninstall.rs",
];

/// A new module that deletes files has to say so here first.
///
/// This exists because of a real bug, and the failure it is aimed at is not
/// "somebody called `remove_dir_all`" but what surrounded the call:
/// `agent_skills::uninstall` took a name straight off the command line, joined
/// it to a destination and deleted the result. `Path::join` on an *absolute*
/// argument discards the left-hand side entirely, so `--dir ./skills` bounded
/// nothing, and `..` was resolved by the OS at removal time. The same module
/// already rejected an escaping path when it came out of a tarball.
///
/// No lint catches that, and no grep can tell a checked join from an unchecked
/// one. What this can do is make every deletion site something a reviewer has
/// to have agreed to, and put the question in front of whoever adds the next
/// one: *where did this path come from, and what stops it leaving the
/// directory you meant?*
#[test]
fn only_named_modules_delete_from_the_filesystem() {
    let allowed: BTreeSet<&str> = MAY_DELETE.iter().copied().collect();
    let mut unexpected = vec![];

    for (name, source) in sources() {
        if allowed.contains(name.as_str()) {
            continue;
        }
        for call in ["remove_dir_all(", "remove_file(", "remove_dir("] {
            if source.contains(call) {
                unexpected.push(format!("src/{name} calls {call}…)"));
            }
        }
    }

    assert!(
        unexpected.is_empty(),
        "these modules delete from the filesystem and are not in MAY_DELETE:\n  {}\n\n\
         Deleting has no undo, and a path built from a caller's input is how a delete \
         leaves the directory it was meant for — join it through a check that the result \
         is still underneath, the way `agent_skills::is_skill_name` does, then add the \
         module to `MAY_DELETE` in tests/source_guards.rs with a line saying what it \
         removes.",
        unexpected.join("\n  ")
    );
}

/// The list must not rot in the other direction either.
///
/// An entry left behind after the deletes it covered were removed is an entry
/// that silently permits the next one — the same reason a maintainer-only
/// drift check reports ignore-file lines whose spec is gone.
#[test]
fn every_module_allowed_to_delete_still_does() {
    let sources = sources();
    let mut stale = vec![];

    for allowed in MAY_DELETE {
        let Some((_, source)) = sources.iter().find(|(name, _)| name == allowed) else {
            stale.push(format!("src/{allowed} no longer exists"));
            continue;
        };
        let deletes = ["remove_dir_all(", "remove_file(", "remove_dir("]
            .iter()
            .any(|call| source.contains(call));
        if !deletes {
            stale.push(format!("src/{allowed} no longer deletes anything"));
        }
    }

    assert!(
        stale.is_empty(),
        "MAY_DELETE in tests/source_guards.rs has entries that are no longer true:\n  {}\n\
         Remove them, so the list keeps meaning what it says.",
        stale.join("\n  ")
    );
}

/// stdout is the result, and `output::emit` is the only thing that writes it.
///
/// `clippy::print_stdout` (denied in Cargo.toml) covers the macros. This
/// covers the other way in: taking the handle and writing to it directly,
/// which is what `output` itself does and what nothing else should.
///
/// Two modules are exceptions the output contract already names, and both are
/// listed here with the reason rather than passing unexplained: `completion`,
/// because a shell script is the result and an envelope around it is
/// unsourceable, and `executor`, because a binary response is bytes — a PNG
/// wrapped in JSON is a corrupted PNG. Both are in README's list of the four
/// things `--output` does not apply to.
///
/// `telemetry` is a third, and does not write at all — it only reads
/// `stdout().is_terminal()`, same as `output.rs` already does.
#[test]
fn only_output_completion_and_binary_responses_write_to_stdout() {
    const MAY_WRITE_STDOUT: &[&str] =
        &["output.rs", "completion.rs", "executor.rs", "telemetry.rs"];

    let mut unexpected = vec![];
    for (name, source) in sources() {
        if MAY_WRITE_STDOUT.contains(&name.as_str()) {
            continue;
        }
        if source.contains("stdout()") {
            unexpected.push(format!("src/{name}"));
        }
    }

    assert!(
        unexpected.is_empty(),
        "these modules reach for stdout directly:\n  {}\n\n\
         A result goes through `output::emit`, which is where `--output` is honoured; \
         anything else belongs on stderr through `output::progress`. If a command really \
         does own its bytes — as `completion` does — add it to MAY_WRITE_STDOUT with the \
         reason.",
        unexpected.join("\n  ")
    );
}
