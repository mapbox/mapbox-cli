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
/// - `telemetry_sink` — its own dated event files past the retention window,
///   matched by exact `YYYY-MM-DD.jsonl` names inside `~/.mapbox/.telemetry`,
///   and a state file there it replaces.
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
    "telemetry_sink.rs",
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
         A result goes through `output::emit`, which is where `--output` is honored; \
         anything else belongs on stderr through `output::progress`. If a command really \
         does own its bytes — as `completion` does — add it to MAY_WRITE_STDOUT with the \
         reason.",
        unexpected.join("\n  ")
    );
}

/// Every request goes through `http::send`, which is what records it for the
/// run's telemetry event.
///
/// A `reqwest` client has no response hook, so a request sent with
/// `RequestBuilder::send` anywhere else is one the event's `network` never
/// counts — silently, since nothing fails. `http.rs` is exempt: it is where
/// `send` is defined, and its own tests call the builder directly.
#[test]
fn only_http_sends_requests() {
    let unexpected: Vec<String> = sources()
        .into_iter()
        .filter(|(name, source)| name != "http.rs" && source.contains(".send()"))
        .map(|(name, _)| format!("src/{name}"))
        .collect();

    assert!(
        unexpected.is_empty(),
        "these modules call `.send()` directly:\n  {}\n\n\
         Wrap the request builder in `http::send(...)` instead, so the request is \
         counted in the telemetry event.",
        unexpected.join("\n  ")
    );
}

/// Modules that turn a Mapbox API failure into a `CliError::http`, and so
/// must carry the response's `X-Request-Id` into it.
const CARRIES_A_REQUEST_ID: &[&str] = &["account_usage.rs", "auth.rs", "executor.rs"];

/// Modules that raise `CliError::http` and deliberately carry no request id.
///
/// `agent_skills.rs` talks to GitHub codeload, which identifies requests with
/// `x-github-request-id`. That is not something Mapbox support can look up,
/// so an id there would point at the wrong company — worse than none.
const NO_REQUEST_ID_TO_CARRY: &[&str] = &["agent_skills.rs"];

/// `output.rs` defines `CliError::http` rather than calling it over a wire.
const NOT_A_SEND_PATH: &[&str] = &["output.rs"];

/// A new path that reports an API failure has to decide about the request id.
///
/// The failure this is aimed at is not a missing field — it is the shape of
/// the mistake that made #117 worth filing: `bytes()` and `text()` both
/// consume the response, so every header not read *before* the body is gone
/// for good. Someone adding a fifth send path will read the status and the
/// body, because those are what the code after it needs, and the id will be
/// unrecoverable by the time anyone wants it. Being on one of two lists is a
/// decision; being on neither is an oversight, which is what this catches.
#[test]
fn every_mapbox_failure_path_carries_the_request_id() {
    for (name, body) in sources() {
        let file = name.as_str();
        if NOT_A_SEND_PATH.contains(&file) {
            continue;
        }
        let raises = body.contains("CliError::http(");
        let carries = CARRIES_A_REQUEST_ID.contains(&file);
        let exempt = NO_REQUEST_ID_TO_CARRY.contains(&file);

        assert!(
            !(carries && exempt),
            "{file} is on both lists; it cannot both carry an id and have none to carry"
        );

        if carries {
            assert!(
                raises,
                "{file} is listed in CARRIES_A_REQUEST_ID but no longer raises \
                 CliError::http — drop it from the list"
            );
            assert!(
                body.contains("with_request_id") || body.contains("request_id("),
                "{file} reports an API failure without carrying its request id. \
                 Read `executor::request_id(response.headers())` before the body, \
                 because `text()`/`bytes()` consume the response and the header is \
                 gone afterwards."
            );
        } else if exempt {
            assert!(
                raises,
                "{file} is listed in NO_REQUEST_ID_TO_CARRY but no longer raises \
                 CliError::http — drop it from the list"
            );
        } else {
            assert!(
                !raises,
                "{file} raises CliError::http but is on neither request-id list. \
                 Either carry the id (see `executor::request_id`) and add it to \
                 CARRIES_A_REQUEST_ID, or add it to NO_REQUEST_ID_TO_CARRY with the \
                 reason this endpoint has no id Mapbox support could look up."
            );
        }
    }
}

/// Every marker the `User-Agent` can carry, and the words in README.md that
/// disclose it.
///
/// The left side is what `telemetry::telemetry_markers` emits; the right side
/// is a phrase that has to appear in the Privacy section. Adding a marker
/// without a row here fails `every_telemetry_marker_is_disclosed`, and so does
/// rewording the disclosure out from under one.
const DISCLOSED: &[(&str, &str)] = &[
    ("os/", "OS/architecture"),
    ("arch/", "OS/architecture"),
    ("env/ci", "run in a CI"),
    ("agent/", "AI coding agent"),
    ("stdin_tty/", "stdin and stdout are attached to a terminal"),
    ("stdout_tty/", "stdin and stdout are attached to a terminal"),
    ("command/", "service a Mapbox API command belongs to"),
];

/// A marker nobody wrote down is a thing we collect and do not admit to.
///
/// This exists because it happened. The Privacy section claimed we collect
/// exit codes, which we never have; described `command/` as the command name
/// when it is the service; and did not mention the terminal markers at all,
/// which we send on every request. Prose and code drifted because nothing
/// compared them.
///
/// Deliberately blunt, in the spirit of the other guards here. It does not
/// prove the disclosure is *well* written — only that no marker is missing
/// from it, and that the sentence disclosing one cannot quietly be edited
/// away.
#[test]
fn every_telemetry_marker_is_disclosed() {
    let telemetry =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/telemetry.rs"))
            .expect("read src/telemetry.rs");
    let readme =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .expect("read README.md");

    // The Privacy section alone: a marker named anywhere else in the README
    // is not a disclosure.
    let privacy = readme
        .split_once("### Privacy")
        .expect("README.md has a Privacy section")
        .1;
    let privacy = privacy.split("\n## ").next().unwrap_or(privacy);

    for (marker, disclosure) in DISCLOSED {
        assert!(
            privacy.contains(disclosure),
            "README.md's Privacy section no longer says {disclosure:?}, which is what \
             discloses the `{marker}` marker. Either put it back or update DISCLOSED."
        );
    }

    // The other direction: a marker emitted but never written down. Matching
    // the emission sites rather than the runtime output, because two of them
    // are conditional on the environment the test happens to run in.
    for prefix in ["os/", "arch/", "env/ci", "agent/", "stdin_tty/", "command/"] {
        assert!(
            telemetry.contains(prefix),
            "`{prefix}` is in DISCLOSED but src/telemetry.rs no longer emits it — \
             drop the row, and the sentence in README.md with it"
        );
    }

    let emitted =
        telemetry.matches("markers.push").count() + telemetry.matches("markers.extend").count();
    assert_eq!(
        emitted, 4,
        "telemetry_markers gained or lost a marker. Add a DISCLOSED row and a \
         sentence in README.md's Privacy section, then update this count."
    );
}

/// British spellings, and the American spelling to use instead.
///
/// Not a dictionary. This is the set that actually drifted into this
/// repository — 91 lines across 27 files before anyone noticed — so it is the
/// set most likely to come back. A word missing from here is not permission
/// for it; it is a word nobody has written yet.
///
/// Each entry is a prefix, so one row covers a word's whole family: its
/// past tense, its plural and its adverb need no rows of their own.
///
/// `cancelled` is deliberately absent. It is the machine-readable `code` in
/// the JSON error contract, documented in `docs/commands.md` and asserted in
/// `tests/non_interactive.rs`, which makes it a compatibility promise rather
/// than a spelling. Renaming it is a breaking change for anyone matching on
/// it, and that is somebody's decision rather than this test's.
const BRITISH: &[(&str, &str)] = &[
    ("acknowledgement", "acknowledgment"),
    ("amongst", "among"),
    ("analyse", "analyze"),
    ("apologis", "apologiz"),
    ("artefact", "artifact"),
    ("behaviour", "behavior"),
    ("catalogue", "catalog"),
    ("centre", "center"),
    ("colour", "color"),
    ("criticis", "criticiz"),
    ("defence", "defense"),
    ("favour", "favor"),
    ("honour", "honor"),
    ("initialis", "initializ"),
    ("judgement", "judgment"),
    ("labelled", "labeled"),
    ("labelling", "labeling"),
    ("licence", "license"),
    ("modelled", "modeled"),
    ("modelling", "modeling"),
    ("neighbour", "neighbor"),
    ("normalis", "normaliz"),
    ("organis", "organiz"),
    ("prioritis", "prioritiz"),
    ("programme", "program"),
    ("recognis", "recogniz"),
    ("sanitis", "sanitiz"),
    ("serialis", "serializ"),
    ("summaris", "summariz"),
    ("utilis", "utiliz"),
    ("whilst", "while"),
];

/// Every file whose prose is ours to write, as (path, contents).
///
/// `openapi/` and `custom-openapi/` are left out. Their descriptions do reach
/// the user as help text, but the words are the API teams', mirrored from
/// upstream specs, and an edit here would not survive the next regenerate.
/// Both happen to be clean today; neither is ours to hold to this rule.
fn prose_files() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut out = vec![];

    for name in [
        "AGENTS.md",
        "CLAUDE.md",
        "README.md",
        "CONTRIBUTING.md",
        "CHANGELOG.md",
        "SECURITY.md",
    ] {
        out.push((
            name.to_owned(),
            std::fs::read_to_string(root.join(name)).expect("read a root document"),
        ));
    }

    for dir in ["src", "tests", "docs", "scripts"] {
        for entry in std::fs::read_dir(root.join(dir)).expect("read a source directory") {
            let path = entry.expect("a directory entry").path();
            let ours = path
                .extension()
                .is_some_and(|ext| ["rs", "md", "sh", "ps1"].iter().any(|k| ext == *k));
            if ours {
                let name = format!(
                    "{dir}/{}",
                    path.file_name().expect("a file name").to_string_lossy()
                );
                out.push((name, std::fs::read_to_string(&path).expect("read a file")));
            }
        }
    }

    out.sort();
    // The trap this guard could fall into is reading nothing and passing for
    // it, so the count is asserted rather than assumed.
    assert!(out.len() > 30, "prose_files found only {}", out.len());
    out
}

/// A row of a two-column table of string literals, like `BRITISH`'s own.
///
/// Without this the guard fails on itself: the table of spellings to avoid is
/// a list of spellings to avoid. Narrow on purpose — a line has to be nothing
/// but two quoted strings and a comma — so it excuses a data row and not a
/// sentence.
fn is_a_table_row(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("(\"") && line.ends_with("\"),")
}

/// One spelling, everywhere, and American because Mapbox is.
///
/// Fenced code blocks in Markdown are skipped: sample output and captured API
/// responses are quoted from somewhere else, and `docs/commands.md` is full of
/// both. A British place name in a captured geocoding result is not a
/// spelling mistake, and rewriting it would make the document misquote the
/// API.
#[test]
fn prose_is_american_english() {
    let mut found = vec![];
    for (name, text) in prose_files() {
        let markdown = name.ends_with(".md");
        let mut fenced = false;
        for (index, line) in text.lines().enumerate() {
            if markdown && line.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced || is_a_table_row(line) {
                continue;
            }
            let lower = line.to_lowercase();
            for (british, american) in BRITISH {
                if lower.contains(british) {
                    found.push(format!("{name}:{}: {british} -> {american}", index + 1));
                }
            }
        }
    }

    assert!(
        found.is_empty(),
        "British spellings:\n  {}\n\n\
         This repository writes American English, in comments and documentation \
         as well as in anything the CLI prints. If one of these is a proper noun \
         rather than a spelling — a place name in a fixture, say — move it inside \
         a fenced block or drop the word from BRITISH with the reason.",
        found.join("\n  ")
    );
}
