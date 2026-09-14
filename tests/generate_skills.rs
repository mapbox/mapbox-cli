//! End-to-end tests for `mapbox generate-skills`.
//!
//! The unit tests in `src/generate_skills.rs` check the four guarantees
//! against the rendered strings, in one process. What they cannot show is the
//! part a caller depends on: that running the real binary twice leaves
//! identical bytes on disk, that `--dry-run` leaves the disk alone, that a
//! directory holding someone else's work is refused rather than replaced, and
//! that none of it needs a token.
//!
//! Nothing here reaches the network, and that is the point rather than an
//! accident: an agent asking what this CLI can do has not logged in yet, so
//! every test runs with no credentials and an empty environment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A directory of this test's own, emptied first so a previous run cannot be
/// mistaken for this one's output.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("generate-skills-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Runs the real binary with the developer's own environment cleared.
///
/// A token in the running shell would let a mistake here reach the API, and
/// the developer's own `CLAUDE_CONFIG_DIR` or `CODEX_HOME` would make the
/// `--agent` tests depend on which agents happen to be installed on this
/// machine. See the same function in `schema_contract.rs` for why
/// `MAPBOX_CONFIG_DIR` is the isolation that holds and `HOME` is a backstop.
fn command(name: &str) -> Command {
    let home = scratch(&format!("{name}-home"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("MAPBOX_CONFIG_DIR", home.join(".mapbox"));
    cmd
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// Generates into `dir` and insists it worked, with nothing on stderr.
fn generate(name: &str, args: &[&str]) -> Output {
    let out = command(name).args(args).output().expect("run mapbox");
    assert!(out.status.success(), "{args:?} failed: {}", stderr(&out));
    assert_eq!(stderr(&out), "", "{args:?} wrote to stderr");
    out
}

/// Every file under `root`, keyed by its path relative to `root`.
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                out.insert(relative, std::fs::read(&path).expect("read a written file"));
            }
        }
    }
    out
}

fn skill_dir(root: &Path) -> PathBuf {
    root.join("mapbox-cli")
}

/// `path`, relative and rendered with `/`.
///
/// The report the CLI writes uses `/` on every platform (see `slashed` in
/// `src/generate_skills.rs`), but a path this test built by walking the real
/// filesystem carries whatever separator the OS used. Without normalizing
/// both sides the same way, comparing them would only ever pass on Unix.
fn slashed(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// **Guarantee 4.** Two runs of one binary leave identical bytes.
///
/// This is what lets a generated skill be committed: a diff after a
/// regenerate means the CLI changed, not that the clock moved or that a
/// `HashMap` iterated differently. Compared as bytes rather than as text, so
/// a stray `\r` on Windows fails here too.
#[test]
fn two_runs_write_the_same_bytes() {
    let dir = scratch("determinism");
    let out = dir.join("out");

    generate(
        "determinism-a",
        &["generate-skills", "--dir", out.to_str().unwrap()],
    );
    let first = tree(&skill_dir(&out));

    generate(
        "determinism-b",
        &["generate-skills", "--dir", out.to_str().unwrap()],
    );
    let second = tree(&skill_dir(&out));

    assert!(first.len() > 3, "only {} files written", first.len());
    assert_eq!(
        first.keys().collect::<Vec<&PathBuf>>(),
        second.keys().collect::<Vec<&PathBuf>>(),
        "the two runs wrote different files"
    );
    for (path, bytes) in &first {
        assert_eq!(
            bytes,
            second.get(path).expect("the same file"),
            "{} differs between two runs",
            path.display()
        );
    }
}

/// A dry run says what it would do and touches nothing.
///
/// The flag is worth having only if it is completely inert — a caller reaching
/// for it before a regenerate they are unsure about must not find the
/// regenerate already done.
#[test]
fn a_dry_run_writes_nothing_and_lists_what_it_would() {
    let dir = scratch("dry-run");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).expect("create the destination");

    let output = generate(
        "dry-run",
        &[
            "generate-skills",
            "--dry-run",
            "--dir",
            out.to_str().unwrap(),
            "-o",
            "text",
        ],
    );

    assert!(
        tree(&out).is_empty(),
        "--dry-run wrote {:?}",
        tree(&out).keys().collect::<Vec<&PathBuf>>()
    );
    assert!(!skill_dir(&out).exists(), "--dry-run created the skill dir");

    let text = stdout(&output);
    assert!(text.contains("Dry run"), "{text}");
    for expected in ["SKILL.md", "AGENTS.md", "references/styles.md"] {
        assert!(text.contains(expected), "{text} does not list {expected}");
    }
}

/// Every file has to say it was generated, because that is what lets the next
/// run replace the directory without asking.
#[test]
fn every_written_file_says_it_was_generated() {
    let dir = scratch("marker");
    let out = dir.join("out");
    generate(
        "marker",
        &["generate-skills", "--dir", out.to_str().unwrap()],
    );

    let files = tree(&skill_dir(&out));
    assert!(files.len() > 3, "only {} files written", files.len());
    for (path, bytes) in &files {
        let head = &bytes[..bytes.len().min(1024)];
        let marker = b"Generated by `mapbox generate-skills`";
        assert!(
            head.windows(marker.len()).any(|window| window == marker),
            "{} carries no generated marker in its first kibibyte",
            path.display()
        );
    }
}

/// SKILL.md's frontmatter has to be the first bytes of the file: the Agent
/// Skills format reads it there and nowhere else.
#[test]
fn the_skill_file_opens_with_its_frontmatter() {
    let dir = scratch("frontmatter");
    let out = dir.join("out");
    generate(
        "frontmatter",
        &["generate-skills", "--dir", out.to_str().unwrap()],
    );

    let contents =
        std::fs::read_to_string(skill_dir(&out).join("SKILL.md")).expect("read SKILL.md");
    assert!(
        contents.starts_with("---\nname: mapbox-cli\n"),
        "SKILL.md starts {:?}",
        &contents[..contents.len().min(60)]
    );
}

/// A directory holding a file this command did not write is refused, by name,
/// until `--force` says otherwise.
///
/// The stranger has to survive the refusal: an error that has already
/// deleted the thing it is complaining about is not a refusal.
#[test]
fn a_stranger_file_stops_the_write_until_force() {
    let dir = scratch("stranger");
    let out = dir.join("out");
    let target = out.to_str().unwrap();

    generate("stranger-first", &["generate-skills", "--dir", target]);

    let stranger = skill_dir(&out).join("references").join("my-notes.md");
    std::fs::write(&stranger, "notes I typed myself\n").expect("write a stranger");

    let refused = command("stranger-refused")
        .args(["generate-skills", "--dir", target])
        .output()
        .expect("run mapbox");
    assert!(!refused.status.success(), "the write was not refused");
    let message = stderr(&refused);
    assert!(message.contains("my-notes.md"), "{message}");
    assert!(message.contains("--force"), "{message}");
    assert!(
        stranger.exists(),
        "the refusal deleted the file it was protecting"
    );

    let forced = generate(
        "stranger-forced",
        &["generate-skills", "--dir", target, "--force"],
    );
    assert!(stdout(&forced).contains("SKILL.md"), "{}", stdout(&forced));
    assert!(
        !stranger.exists(),
        "--force is supposed to replace the whole directory"
    );
}

/// A file we stop generating has to go, or it outlives the command that made
/// it forever and an agent keeps reading it.
#[test]
fn a_regenerate_removes_a_file_it_no_longer_writes() {
    let dir = scratch("replace");
    let out = dir.join("out");
    let target = out.to_str().unwrap();

    generate("replace-first", &["generate-skills", "--dir", target]);
    assert!(skill_dir(&out).join("references/styles.md").exists());

    // Narrowed to one service, so every other reference page should be gone
    // rather than left behind as a stale description.
    generate(
        "replace-second",
        &["generate-skills", "--dir", target, "--service", "geocoder"],
    );
    let files = tree(&skill_dir(&out));
    let pages: Vec<&PathBuf> = files
        .keys()
        .filter(|path| path.starts_with("references"))
        .collect();
    assert_eq!(
        pages,
        vec![&PathBuf::from("references").join("geocoder.md")],
        "a page from the previous run survived"
    );
    // No working directory left beside it either.
    let leftovers: Vec<PathBuf> = std::fs::read_dir(&out)
        .expect("read the destination")
        .flatten()
        .map(|entry| entry.file_name().into())
        .filter(|name: &PathBuf| name != Path::new("mapbox-cli"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?} left beside the skill");
}

/// `--agent` writes where that agent reads, relative to the directory the
/// command ran in.
#[test]
fn an_agent_writes_into_the_directory_that_agent_reads() {
    let project = scratch("agent-project");

    let out = command("agent")
        .current_dir(&project)
        .args([
            "generate-skills",
            "--agent",
            "claude-code",
            "--agent",
            "codex",
        ])
        .output()
        .expect("run mapbox");
    assert!(out.status.success(), "{}", stderr(&out));

    for relative in [".claude/skills", ".agents/skills"] {
        let path = project.join(relative).join("mapbox-cli").join("SKILL.md");
        assert!(path.exists(), "{} was not written", path.display());
    }
}

/// `--global` writes under the agent's home rather than the project's, and
/// takes the home from the agent's own variable.
#[test]
fn global_writes_under_the_agents_home() {
    let dir = scratch("global");
    let config = dir.join("claude-config");
    std::fs::create_dir_all(&config).expect("create the agent home");

    let out = command("global")
        .current_dir(&dir)
        .env("CLAUDE_CONFIG_DIR", &config)
        .args(["generate-skills", "--global", "--agent", "claude-code"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success(), "{}", stderr(&out));

    assert!(config.join("skills/mapbox-cli/SKILL.md").exists());
    assert!(
        !dir.join(".claude").exists(),
        "--global wrote into the project as well"
    );
}

/// With nowhere to write to, the command says so rather than exiting zero
/// having done nothing.
#[test]
fn nowhere_to_write_is_an_error_naming_the_flags() {
    let dir = scratch("nowhere");

    let out = command("nowhere")
        .current_dir(&dir)
        .arg("generate-skills")
        .output()
        .expect("run mapbox");

    assert!(!out.status.success(), "{}", stdout(&out));
    let message = stderr(&out);
    for flag in ["--agent", "--global", "--dir"] {
        assert!(message.contains(flag), "{message} does not mention {flag}");
    }
    assert!(tree(&dir).is_empty(), "something was written anyway");
}

/// An unknown `--service` is refused with the real names, and writes nothing.
#[test]
fn an_unknown_service_is_refused_before_anything_is_written() {
    let dir = scratch("unknown-service");
    let out = dir.join("out");

    let refused = command("unknown-service")
        .args([
            "generate-skills",
            "--service",
            "style",
            "--dir",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("run mapbox");

    assert!(!refused.status.success(), "{}", stdout(&refused));
    let message = stderr(&refused);
    assert!(message.contains("styles"), "{message}");
    assert!(!out.exists(), "the destination was created anyway");
}

/// The report is a result, so it obeys the output contract: JSON on a pipe,
/// naming every file, with nothing on stderr.
#[test]
fn the_report_is_json_when_asked_and_names_every_file() {
    let dir = scratch("json");
    let out = dir.join("out");

    let output = generate(
        "json",
        &[
            "generate-skills",
            "--dir",
            out.to_str().unwrap(),
            "-o",
            "json",
        ],
    );

    let value: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("the report is JSON");
    assert_eq!(value["dry_run"], false);
    assert_eq!(value["skill"], "mapbox-cli");

    let destinations = value["destinations"]
        .as_array()
        .expect("destinations is an array");
    assert_eq!(destinations.len(), 1);

    let listed: Vec<String> = destinations[0]["files"]
        .as_array()
        .expect("files is an array")
        .iter()
        .map(|file| file.as_str().expect("a path").to_string())
        .collect();
    let written: Vec<String> = tree(&skill_dir(&out))
        .keys()
        .map(|path| slashed(path))
        .collect();

    let mut listed_sorted = listed.clone();
    listed_sorted.sort();
    let mut written_sorted = written.clone();
    written_sorted.sort();
    assert_eq!(listed_sorted, written_sorted);
}

/// Describing the CLI must not need a login. Nothing above supplies a token,
/// so a regression that made this command resolve credentials — or reach the
/// network — would fail every test here rather than only in an offline CI job.
#[test]
fn no_token_is_needed_and_the_help_says_what_it_does() {
    let out = command("help")
        .args(["generate-skills", "--help"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success(), "{}", stderr(&out));

    let help = stdout(&out);
    for flag in [
        "--agent",
        "--global",
        "--dir",
        "--service",
        "--force",
        "--dry-run",
    ] {
        assert!(help.contains(flag), "--help does not mention {flag}");
    }
}
