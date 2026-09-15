//! `mapbox agent-skills` — install the published Mapbox Agent Skills.
//!
//! Not to be confused with its neighbor. [`crate::generate_skills`] writes a
//! skill describing *this CLI's own commands*, rendered from the specs
//! compiled into the binary. This command installs the *hand-written Mapbox
//! domain skills* — cartography, token security, iOS and Android patterns,
//! style quality — published at
//! <https://github.com/mapbox/mapbox-agent-skills>. Different content, same
//! destinations, so both go through [`crate::skill_dest`] and neither owns the
//! agent table.
//!
//! `/plugin marketplace add` and `npx skills add` already install these and
//! work well; this is not trying to replace them. It buys two things they do
//! not: **discovery**, for the many people who install the CLI and never learn
//! the skills repo exists, and **reachability**, in the environments where npm
//! is unavailable or unapproved but a signed `mapbox` binary is. Keeping that
//! second property is why this fetches and extracts a tarball itself rather
//! than shelling out to `git` or `tar`.
//!
//! # One request, not 158
//!
//! The skills are 20 directories of ~220 files. Fetching them individually
//! through the GitHub contents API would spend 158 of an unauthenticated
//! budget of 60 requests an hour, so a single install would fail part-way
//! through and leave the next one rate-limited. [`fetch`] takes the repository
//! tarball from `codeload.github.com` instead — one request, half a megabyte —
//! and [`extract`] keeps the `skills/` prefix out of it.
//!
//! `--ref` is what makes an install reproducible. The issue that asked for
//! this wanted the resolved commit SHA recorded in the output; that would cost
//! a second request against the same 60-an-hour budget, and it is not
//! necessary, because `--ref <sha>` is accepted and *is* the record. What gets
//! reported is the ref as given.
//!
//! # Writing into somebody's working directory
//!
//! This is the first command in this CLI that writes outside the config
//! directory, and the rules are stricter for it than for anything under
//! `~/.mapbox`:
//!
//! - **Nothing is written until everything is known.** The archive is fetched,
//!   extracted and checked in memory; the write is the last thing that
//!   happens.
//! - **An existing skill directory stops the install.** Not a merge, not an
//!   overwrite: `--force` is how you say you meant it. Silently replacing a
//!   directory somebody has edited is the failure mode this exists to avoid.
//! - **Only regular files come out of the archive.** A symlink, a hard link or
//!   a device node is skipped, and any entry whose path escapes its skill —
//!   `..`, an absolute path, a Windows drive letter — is a hard error rather
//!   than a skipped entry. A tarball that contains one is not a tarball with a
//!   bad file in it; it is a tarball to stop reading.
//! - **`evals/` never lands.** It is upstream's test tooling, and installing
//!   it would put JSON nobody reads into every skill an agent loads.
//! - **Each skill is staged and renamed into place**, so an interrupted
//!   install leaves either the old directory or the new one.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::json;

use crate::confirm;
use crate::executor;
use crate::http;
use crate::output::{self, CliError, Mode};
use crate::remedy::Remedy;
use crate::skill_dest::{self, AgentHomes, Destination};

pub const COMMAND: &str = "agent-skills";

const LIST: &str = "list";
const INSTALL: &str = "install";
const UPDATE: &str = "update";
const UNINSTALL: &str = "uninstall";

const NAME_ARG: &str = "name";
const REF_ARG: &str = "ref";
const FORCE_ARG: &str = "force";

/// The repository the skills are published from.
const REPO: &str = "mapbox/mapbox-agent-skills";

/// Where a repository tarball comes from. A separate host from `api.github.com`
/// on purpose — it serves the archive directly and is not part of the API rate
/// limit.
const CODELOAD_BASE: &str = "https://codeload.github.com";

const DEFAULT_REF: &str = "main";

/// The prefix inside the archive that holds the skills, and the directory
/// inside each skill that must not be installed.
const SKILLS_PREFIX: &str = "skills";
const EVALS_DIR: &str = "evals";

/// The file an agent reads first, and the one carrying the description.
const ENTRY_FILE: &str = "SKILL.md";

/// A ceiling on what will be read out of the archive, in bytes.
///
/// The published skills are half a megabyte; this is two orders of magnitude
/// above that and exists only so a malformed or hostile archive cannot make
/// this process allocate without bound. A gzip stream is a decompression bomb
/// waiting for a reader that trusts it.
const MAX_EXTRACTED: u64 = 64 * 1024 * 1024;

/// One skill, whole, in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Skill {
    name: String,
    /// The `description` from `SKILL.md`'s frontmatter, when it has one.
    description: Option<String>,
    /// Paths relative to the skill's own directory, and their bytes. Sorted,
    /// so a listing and a plan are stable.
    files: BTreeMap<PathBuf, Vec<u8>>,
}

pub fn command() -> Command {
    Command::new(COMMAND)
        .about("Install the published Mapbox Agent Skills into a project")
        .long_about(format!(
            "Install the Mapbox Agent Skills — hand-written domain guidance for coding \
             agents, covering cartography, token security, style quality and the mobile \
             and web SDKs — from https://github.com/{REPO}.\n\n\
             Not the same as `mapbox generate-skills`, which writes a skill describing \
             this CLI's own commands. These are about using Mapbox; that one is about \
             using this binary.\n\n\
             Needs no token: the repository is public."
        ))
        .subcommand_required(true)
        .subcommand(
            Command::new(LIST)
                .about("List the published skills, and which are already installed here")
                .args(skill_dest::args())
                .arg(ref_arg()),
        )
        .subcommand(
            Command::new(INSTALL)
                .about("Install skills. With no NAME, installs all of them")
                .arg(
                    Arg::new(NAME_ARG)
                        .value_name("NAME")
                        .action(ArgAction::Append)
                        .help("Skill to install, repeatable. Defaults to every published skill"),
                )
                .args(skill_dest::args())
                .arg(ref_arg())
                .arg(
                    Arg::new(FORCE_ARG)
                        .long(FORCE_ARG)
                        .action(ArgAction::SetTrue)
                        .help("Replace a skill directory that is already there"),
                )
                .arg(executor::dry_run_arg(
                    "List the files it would write, then exit without writing them",
                )),
        )
        .subcommand(
            Command::new(UPDATE)
                .about("Re-install the skills already here, and report what changed")
                .arg(
                    Arg::new(NAME_ARG)
                        .value_name("NAME")
                        .action(ArgAction::Append)
                        .help("Skill to update, repeatable. Defaults to every skill already here"),
                )
                .args(skill_dest::args())
                .arg(ref_arg())
                .arg(executor::dry_run_arg(
                    "Report what would change, then exit without changing it",
                )),
        )
        .subcommand(
            Command::new(UNINSTALL)
                .about("Remove installed skills")
                .arg(
                    Arg::new(NAME_ARG)
                        .value_name("NAME")
                        .action(ArgAction::Append)
                        .required(true)
                        .help("Skill to remove, repeatable"),
                )
                .args(skill_dest::args())
                .arg(
                    Arg::new(FORCE_ARG)
                        .long(FORCE_ARG)
                        .action(ArgAction::SetTrue)
                        .help(
                            "Remove a directory even though it does not look like an \
                             installed skill",
                        ),
                )
                .arg(executor::dry_run_arg(
                    "List what it would remove, then exit without removing it",
                )),
        )
}

fn ref_arg() -> Arg {
    Arg::new(REF_ARG)
        .long(REF_ARG)
        .value_name("REF")
        .default_value(DEFAULT_REF)
        .help("Branch, tag or commit to install from. A commit SHA pins the install")
}

/// The archive URL for one ref.
///
/// `refs/heads/<ref>` is deliberately not used: that spelling only resolves
/// branches, and this accepts a tag or a commit SHA as readily.
fn archive_url(base: &str, git_ref: &str) -> String {
    format!("{base}/{REPO}/tar.gz/{git_ref}")
}

/// The repository tarball, as bytes.
///
/// Goes through [`http::client`] like every other request this CLI makes, so
/// it carries the same user agent and the same timeout budget.
fn fetch(base: &str, git_ref: &str, debug: bool) -> Result<Vec<u8>> {
    let url = archive_url(base, git_ref);
    if debug {
        eprintln!("[debug] GET {url}");
    }

    let response = http::client()?
        .get(&url)
        .send()
        .map_err(|e| executor::transport_failure("Could not reach GitHub", e))?;

    let status = response.status();
    if !status.is_success() {
        // 404 is the one worth wording: it is what a ref that does not exist
        // looks like, and it is the only failure a caller can fix by typing
        // something different.
        let message = if status.as_u16() == 404 {
            format!("No `{git_ref}` in {REPO} — check the branch, tag or commit.")
        } else {
            format!("GitHub answered {status} for {url}.")
        };
        return Err(CliError::http(status.as_u16(), &message)
            .with_remedy(Remedy::default().with_doc(Some(&format!("https://github.com/{REPO}"))))
            .into());
    }

    let mut bytes = vec![];
    response
        .take(MAX_EXTRACTED)
        .read_to_end(&mut bytes)
        .map_err(|e| anyhow!("Could not read the archive from GitHub: {e}"))?;
    Ok(bytes)
}

/// Whether a path from the archive is one this may write.
///
/// Rejects `..`, absolute paths, and anything with a root or prefix component
/// — a Windows `C:` among them, which on Unix would otherwise pass as an
/// ordinary directory name and on Windows would escape the destination.
fn is_contained(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Whether a caller-supplied skill name is one this may turn into a path.
///
/// A skill name is one directory name, always. Anything else — `..`, an
/// absolute path, `a/b`, a Windows drive prefix, the empty string — is a way
/// to name a directory outside the destination, and `uninstall` hands what it
/// is given to `remove_dir_all`.
///
/// `install` and `update` are safe without this because `selected` matches
/// every name against the published set first. `uninstall` cannot: it works
/// from the disk, on purpose, so that a skill upstream has withdrawn can
/// still be removed. That is exactly why the check has to live here.
fn is_skill_name(name: &str) -> bool {
    let path = Path::new(name);
    is_contained(path) && path.components().count() == 1
}

/// The skills in a repository tarball, keyed by name.
///
/// Everything outside `skills/` is ignored, `evals/` is dropped, and a skill
/// with no [`ENTRY_FILE`] is not a skill — an agent would find nothing to
/// read, so installing it would be installing a directory.
fn extract(archive: &[u8]) -> Result<Vec<Skill>> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder.take(MAX_EXTRACTED));

    let mut files: BTreeMap<String, BTreeMap<PathBuf, Vec<u8>>> = BTreeMap::new();

    for entry in tar
        .entries()
        .context("The archive from GitHub is not readable as a tarball")?
    {
        let mut entry = entry.context("The archive from GitHub ended unexpectedly")?;

        // Only regular files. A symlink or a hard link is a way out of the
        // destination that no path check would catch, and nothing published
        // here needs one.
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }

        let path = entry
            .path()
            .context("The archive holds an entry whose path is not valid UTF-8")?
            .into_owned();

        // Every GitHub archive is wrapped in one directory named after the
        // repository and the ref, and the ref is not known here — so the
        // wrapper is stripped by position rather than by name.
        let mut components = path.components();
        let Some(Component::Normal(_)) = components.next() else {
            return Err(unsafe_entry(&path));
        };
        let inner: PathBuf = components.collect();
        if !is_contained(&inner) {
            return Err(unsafe_entry(&path));
        }

        let Ok(within) = inner.strip_prefix(SKILLS_PREFIX) else {
            continue;
        };
        // `skills/README.md` is the directory's own readme, not a skill.
        let mut parts = within.components();
        let Some(Component::Normal(name)) = parts.next() else {
            continue;
        };
        let relative: PathBuf = parts.collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        // Upstream's test tooling, which is not user-facing content.
        if relative
            .components()
            .any(|part| part.as_os_str() == EVALS_DIR)
        {
            continue;
        }

        let mut bytes = vec![];
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("Could not read {} out of the archive", path.display()))?;

        files
            .entry(name.to_string_lossy().into_owned())
            .or_default()
            .insert(relative, bytes);
    }

    let skills: Vec<Skill> = files
        .into_iter()
        .filter(|(_, files)| files.contains_key(Path::new(ENTRY_FILE)))
        .map(|(name, files)| Skill {
            description: files
                .get(Path::new(ENTRY_FILE))
                .and_then(|bytes| frontmatter_description(bytes)),
            name,
            files,
        })
        .collect();

    if skills.is_empty() {
        return Err(anyhow!(
            "The archive from GitHub holds no skills under `{SKILLS_PREFIX}/`. \
             The repository layout may have changed."
        ));
    }
    Ok(skills)
}

fn unsafe_entry(path: &Path) -> anyhow::Error {
    CliError::new(
        "unsafe_archive",
        format!(
            "Refusing to read `{}` out of the archive: it points outside the \
             directory it would be written to.",
            path.display()
        ),
    )
    .into()
}

/// The `description` from a `SKILL.md`'s YAML frontmatter.
///
/// Parsed with `serde_yaml`, which this crate already uses for the OpenAPI
/// specs, rather than by scanning for a `description:` line — a description
/// may be quoted, folded over several lines, or hold a colon, and all three
/// appear upstream.
fn frontmatter_description(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let rest = text.strip_prefix("---\n").or_else(|| {
        // A file written on Windows, or with a BOM.
        text.strip_prefix("\u{feff}---\n")
            .or_else(|| text.strip_prefix("---\r\n"))
    })?;
    let end = rest.find("\n---").or_else(|| rest.find("\r\n---"))?;
    let front: serde_yaml::Value = serde_yaml::from_str(&rest[..end]).ok()?;
    let description = front.get("description")?.as_str()?.trim();
    (!description.is_empty()).then(|| description.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The skills a caller named, or all of them.
///
/// An unknown name is an error naming the nearest published skill, because a
/// typo here would otherwise install nineteen of the twenty and say nothing.
fn selected<'a>(skills: &'a [Skill], wanted: &[String]) -> Result<Vec<&'a Skill>> {
    if wanted.is_empty() {
        return Ok(skills.iter().collect());
    }

    let mut out = vec![];
    for name in wanted {
        match skills.iter().find(|skill| &skill.name == name) {
            Some(skill) => out.push(skill),
            None => {
                let closest = skills
                    .iter()
                    .map(|skill| skill.name.as_str())
                    .find(|published| {
                        published.contains(name.as_str()) || name.contains(published)
                    });
                let hint = match closest {
                    Some(closest) => format!(" Did you mean `{closest}`?"),
                    None => String::new(),
                };
                return Err(CliError::new(
                    "not_found",
                    format!("No published skill is called `{name}`.{hint}"),
                )
                .with_remedy(
                    Remedy::default().with_action(Some(format!("mapbox {COMMAND} {LIST}"))),
                )
                .into());
            }
        }
    }
    // Naming one twice is one install — including when the two are not next
    // to each other. `Vec::dedup_by` only collapses *adjacent* equal items, so
    // it left `install foo bar foo` as three, and the second `foo` then found
    // its own directory already there and failed with "appeared while
    // installing" after the first copy had landed.
    let mut seen = BTreeSet::new();
    out.retain(|skill| seen.insert(skill.name.clone()));
    Ok(out)
}

/// Every file one install would write, as destination-relative paths.
fn plan(destination: &Destination, skills: &[&Skill]) -> Vec<PathBuf> {
    let mut out = vec![];
    for skill in skills {
        for path in skill.files.keys() {
            out.push(destination.root.join(&skill.name).join(path));
        }
    }
    out
}

/// The skill directories already present at a destination.
fn conflicts(destination: &Destination, skills: &[&Skill]) -> Vec<PathBuf> {
    skills
        .iter()
        .map(|skill| destination.root.join(&skill.name))
        .filter(|path| path.exists())
        .collect()
}

/// Writes the selected skills into one destination.
///
/// Staged: every file lands under a scratch directory inside the destination
/// and each skill is renamed into place afterwards, so an interrupted install
/// leaves either the directory that was there before or the whole new one —
/// never half of each. The scratch directory is inside the destination rather
/// than in the temp directory because a rename across filesystems is a copy,
/// and this one has to be atomic.
fn write_into(destination: &Destination, skills: &[&Skill], force: bool) -> Result<()> {
    let staging = destination.root.join(".mapbox-agent-skills.staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("Could not create {}", staging.display()))?;

    let result = (|| -> Result<()> {
        for skill in skills {
            for (relative, bytes) in &skill.files {
                let path = staging.join(&skill.name).join(relative);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("Could not create {}", parent.display()))?;
                }
                std::fs::write(&path, bytes)
                    .with_context(|| format!("Could not write {}", path.display()))?;
            }
        }

        for skill in skills {
            let target = destination.root.join(&skill.name);
            if target.exists() {
                if !force {
                    // `conflicts` is checked before anything is written, so
                    // reaching here means the directory appeared during the
                    // install.
                    return Err(anyhow!(
                        "{} appeared while installing; nothing was replaced.",
                        target.display()
                    ));
                }
                std::fs::remove_dir_all(&target)
                    .with_context(|| format!("Could not replace {}", target.display()))?;
            }
            std::fs::rename(staging.join(&skill.name), &target).with_context(|| {
                format!("Could not move the new skill into {}", target.display())
            })?;
        }
        Ok(())
    })();

    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// What the globals say, for the two subcommands that care.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunFlags {
    pub debug: bool,
    /// `--yes`. Only `uninstall` asks a question.
    pub assume_yes: bool,
}

pub fn run(matches: &ArgMatches, flags: RunFlags, mode: Mode) -> Result<()> {
    run_with(matches, flags, mode, CODELOAD_BASE)
}

/// The three inputs the subcommands have in common, read once.
struct Shared {
    git_ref: String,
    wanted: Vec<String>,
    dry_run: bool,
}

/// Reads them without assuming every subcommand declares every one.
///
/// **`try_get_*`, never `get_*`.** Asking clap for an argument the subcommand
/// being parsed never declared is a panic, not a `None`, and the four here do
/// not declare the same set: `uninstall` has no `--ref`, because it reads the
/// disk rather than the repository; `list` has neither `NAME` nor
/// `--dry-run`, because it changes nothing. Each of those was a panic on a
/// perfectly ordinary command line before this was one function.
/// `executor::wants_dry_run` is careful in the same way, for the same reason.
///
/// `every_subcommand_survives_the_shared_prologue` walks all four.
fn shared(matches: &ArgMatches) -> Shared {
    Shared {
        git_ref: matches
            .try_get_one::<String>(REF_ARG)
            .ok()
            .flatten()
            .cloned()
            .unwrap_or_else(|| DEFAULT_REF.to_string()),
        wanted: matches
            .try_get_many::<String>(NAME_ARG)
            .ok()
            .flatten()
            .into_iter()
            .flatten()
            .cloned()
            .collect(),
        dry_run: executor::wants_dry_run(matches),
    }
}

/// [`run`], against a named archive host.
///
/// The host is a parameter rather than a constant read inside so the tests can
/// point the whole command at a loopback server — the same seam
/// `account_usage::fetch` takes for the same reason.
fn run_with(matches: &ArgMatches, flags: RunFlags, mode: Mode, base: &str) -> Result<()> {
    let (action, action_matches) = matches
        .subcommand()
        .expect("`agent-skills` sets subcommand_required(true)");

    let Shared {
        git_ref,
        wanted,
        dry_run,
    } = shared(action_matches);
    let git_ref = git_ref.as_str();

    let homes = AgentHomes::from_env();
    // `require` is false for `list` alone, which reports rather than writes: a
    // machine with no agent installed can still ask what is published.
    let destinations = skill_dest::from_matches(action_matches, &homes, action != LIST)?;

    // `uninstall` is the one action that makes no request. It works from what
    // is on disk, so it needs neither the network nor a ref — which also means
    // it still works for a skill that has since been unpublished.
    if action == UNINSTALL {
        return uninstall(
            &wanted,
            &destinations,
            action_matches.get_flag(FORCE_ARG),
            dry_run,
            flags.assume_yes,
            mode,
        );
    }

    let archive = fetch(base, git_ref, flags.debug)?;
    let skills = extract(&archive)?;

    match action {
        LIST => list(&skills, &destinations, git_ref, mode),
        INSTALL => install(
            &selected(&skills, &wanted)?,
            &destinations,
            git_ref,
            action_matches.get_flag(FORCE_ARG),
            dry_run,
            mode,
        ),
        UPDATE => update(
            &skills,
            &wanted,
            &destinations,
            git_ref,
            dry_run,
            flags.assume_yes,
            mode,
        ),
        _ => unreachable!("`agent-skills` declares only four subcommands"),
    }
}

fn list(skills: &[Skill], destinations: &[Destination], git_ref: &str, mode: Mode) -> Result<()> {
    let installed_in = |skill: &Skill| -> Vec<String> {
        destinations
            .iter()
            .filter(|destination| destination.root.join(&skill.name).exists())
            .map(|destination| destination.root.display().to_string())
            .collect()
    };

    let mut lines = vec![format!(
        "{} skills published at {REPO}@{git_ref}:",
        skills.len()
    )];
    let width = skills
        .iter()
        .map(|skill| skill.name.chars().count())
        .max()
        .unwrap_or(0);
    for skill in skills {
        let installed = installed_in(skill);
        let mark = if installed.is_empty() {
            " "
        } else {
            // A leading marker rather than a trailing note: the column a
            // reader scans is the left one.
            "*"
        };
        let description = skill.description.as_deref().unwrap_or("");
        // Cut to the same line length the table renderer fits to, rather than
        // to a sentence: these descriptions are one long sentence each, so
        // `first_sentence` would return the whole of a 190-character line.
        // `-o json` carries every one of them whole.
        let room = output::LINE.saturating_sub(width + 4);
        lines.push(format!(
            "{mark} {:width$}  {}",
            skill.name,
            clip(description, room)
        ));
    }
    if skills.iter().any(|skill| !installed_in(skill).is_empty()) {
        lines.push(String::new());
        lines.push("* already installed here".to_string());
    }

    let json = json!({
        "ref": git_ref,
        "repository": REPO,
        "skills": skills
            .iter()
            .map(|skill| json!({
                "name": skill.name,
                "description": skill.description,
                "installed": installed_in(skill),
            }))
            .collect::<Vec<_>>(),
    });

    output::emit(mode, &lines.join("\n"), json)
}

fn install(
    skills: &[&Skill],
    destinations: &[Destination],
    git_ref: &str,
    force: bool,
    dry_run: bool,
    mode: Mode,
) -> Result<()> {
    // Every conflict, across every destination, before anything is written —
    // a partial install is exactly what the check exists to prevent.
    if !force {
        let blocked: Vec<PathBuf> = destinations
            .iter()
            .flat_map(|destination| conflicts(destination, skills))
            .collect();
        if !blocked.is_empty() {
            let listed = blocked
                .iter()
                .map(|path| format!("  {}", path.display()))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(CliError::new(
                "already_installed",
                format!("These skills are already installed:\n{listed}"),
            )
            .with_remedy(
                Remedy::default()
                    .with_fix("Pass --force to replace them, or name only the skills you want.")
                    .with_action(Some(format!("mapbox {COMMAND} {INSTALL} --force"))),
            )
            .into());
        }
    }

    let files: Vec<(&Destination, Vec<PathBuf>)> = destinations
        .iter()
        .map(|destination| (destination, plan(destination, skills)))
        .collect();
    let total: usize = files.iter().map(|(_, paths)| paths.len()).sum();

    if !dry_run {
        for destination in destinations {
            write_into(destination, skills, force)?;
        }
    }

    let names: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
    let opening = if dry_run {
        format!(
            "Dry run — nothing was written.\nWould install {} skill{} ({total} file{}) from {REPO}@{git_ref}:",
            names.len(),
            plural(names.len()),
            plural(total),
        )
    } else {
        format!(
            "Installed {} skill{} ({total} file{}) from {REPO}@{git_ref}:",
            names.len(),
            plural(names.len()),
            plural(total),
        )
    };

    let mut lines = vec![opening];
    for (destination, paths) in &files {
        lines.push(format!("  {destination}"));
        for name in &names {
            lines.push(format!("    {name}"));
        }
        let _ = paths;
    }

    let json = json!({
        "dry_run": dry_run,
        "ref": git_ref,
        "repository": REPO,
        "skills": names,
        "destinations": files
            .iter()
            .map(|(destination, paths)| json!({
                "root": destination.root.display().to_string(),
                "source": destination.source,
                "files": paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    });

    output::emit(mode, &lines.join("\n"), json)
}

/// `text`, cut to `room` characters with an ellipsis when it does not fit.
///
/// Characters rather than bytes: a description with an accent in it would
/// otherwise be cut mid-codepoint and panic.
fn clip(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    let kept: String = text.chars().take(room.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// How an installed skill compares with the published one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    /// Every file matches, byte for byte.
    Unchanged,
    /// Something differs: a file added upstream, one removed, or one edited —
    /// on either side, since a local edit is a difference too and `update`
    /// restores the published copy.
    Changed,
}

/// Compares an installed skill directory with the published skill.
///
/// This is what a manifest would otherwise be for, and it is better than one:
/// upstream's lock file records a tree SHA so that `update` can *avoid
/// downloading* an unchanged skill, but this command has already downloaded
/// every skill in one tarball before it could consult any record. So there is
/// nothing left to save, and comparing the bytes answers exactly rather than
/// approximately — including for a skill somebody edited by hand, which no
/// recorded SHA would notice.
fn compare(root: &Path, skill: &Skill) -> Change {
    let installed = root.join(&skill.name);

    for (relative, published) in &skill.files {
        match std::fs::read(installed.join(relative)) {
            Ok(current) if &current == published => {}
            _ => return Change::Changed,
        }
    }

    // A file that is on disk and no longer published counts too: leaving it
    // behind would make an updated skill a mixture of two versions.
    match installed_files(&installed) {
        Some(present) if present.len() != skill.files.len() => Change::Changed,
        Some(_) => Change::Unchanged,
        // Unreadable is not the same as unchanged.
        None => Change::Changed,
    }
}

/// Every file under an installed skill, relative to it. `None` if the
/// directory cannot be walked.
fn installed_files(root: &Path) -> Option<Vec<PathBuf>> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()? {
            let path = entry.ok()?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path.strip_prefix(root).ok()?.to_path_buf());
            }
        }
    }
    Some(out)
}

/// Re-installs what is already here, and says what moved.
///
/// Deliberately does **not** install a skill that is not there: that is what
/// `install` is for, and an `update` that quietly added twenty directories
/// because upstream published them would be a different command.
fn update(
    skills: &[Skill],
    wanted: &[String],
    destinations: &[Destination],
    git_ref: &str,
    dry_run: bool,
    assume_yes: bool,
    mode: Mode,
) -> Result<()> {
    let candidates = selected(skills, wanted)?;

    // Per destination, the skills that are there and what has changed about
    // them.
    let mut changed: Vec<(&Destination, Vec<&Skill>)> = vec![];
    let mut unchanged: Vec<String> = vec![];
    let mut missing: Vec<String> = vec![];

    for destination in destinations {
        let mut here = vec![];
        for skill in &candidates {
            if !destination.root.join(&skill.name).is_dir() {
                continue;
            }
            match compare(&destination.root, skill) {
                Change::Changed => here.push(*skill),
                Change::Unchanged => unchanged.push(skill.name.clone()),
            }
        }
        if !here.is_empty() {
            changed.push((destination, here));
        }
    }

    // A name asked for that is installed nowhere is a mistake worth naming,
    // rather than a silent no-op.
    for skill in &candidates {
        let anywhere = destinations
            .iter()
            .any(|destination| destination.root.join(&skill.name).is_dir());
        if !anywhere && !wanted.is_empty() {
            missing.push(skill.name.clone());
        }
    }
    if !missing.is_empty() {
        return Err(CliError::new(
            "not_installed",
            format!(
                "Not installed here, so there is nothing to update: {}.",
                missing.join(", ")
            ),
        )
        .with_remedy(Remedy::default().with_action(Some(format!(
            "mapbox {COMMAND} {INSTALL} {}",
            missing.join(" ")
        ))))
        .into());
    }

    if !dry_run && !changed.is_empty() {
        // Updating replaces a skill directory wholesale, which takes any file
        // somebody added to it and any edit they made — `compare` cannot tell
        // an upstream change from a local one, and both read as "changed".
        // That is the same destructive local action `uninstall` asks about,
        // so it asks here too rather than being the one command in this
        // module that overwrites a working directory in silence. Only when
        // there is something to overwrite, only at a terminal, and `--yes`
        // answers it in advance.
        let listed = changed
            .iter()
            .flat_map(|(_, skills)| skills.iter().map(|skill| skill.name.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        confirm::destructive_local_action(
            &format!(
                "About to replace {listed} with the published copy, losing any local \
                 edits."
            ),
            assume_yes,
        )?;

        for (destination, skills) in &changed {
            // `force`, because updating in place is the whole job — the
            // conflict this would otherwise report is the skill being updated.
            write_into(destination, skills, true)?;
        }
    }

    // By name, not by destination. A machine with two agents installed holds
    // the same skill twice, and reporting "Updated 2 skills" for one skill in
    // two directories — or listing every name twice in `unchanged` — counts
    // directories while calling them skills.
    let mut updated_names: Vec<String> = changed
        .iter()
        .flat_map(|(_, skills)| skills.iter().map(|skill| skill.name.clone()))
        .collect();
    updated_names.sort_unstable();
    updated_names.dedup();

    unchanged.sort_unstable();
    unchanged.dedup();
    // A skill that is stale in one destination and current in another is
    // being updated, so it does not also belong in the up-to-date count.
    unchanged.retain(|name| !updated_names.contains(name));

    let updated = updated_names.len();
    let opening = match (dry_run, updated) {
        (_, 0) => format!("Everything is up to date with {REPO}@{git_ref}."),
        (true, n) => format!(
            "Dry run — nothing was changed.\nWould update {n} skill{} from {REPO}@{git_ref}:",
            plural(n)
        ),
        (false, n) => format!("Updated {n} skill{} from {REPO}@{git_ref}:", plural(n)),
    };

    let mut lines = vec![opening];
    for (destination, skills) in &changed {
        lines.push(format!("  {destination}"));
        for skill in skills {
            lines.push(format!("    {}", skill.name));
        }
    }
    if !unchanged.is_empty() {
        lines.push(format!("{} already up to date.", unchanged.len()));
    }

    let json = json!({
        "dry_run": dry_run,
        "ref": git_ref,
        "repository": REPO,
        "updated": updated_names,
        "unchanged": unchanged,
        "destinations": changed
            .iter()
            .map(|(destination, skills)| json!({
                "root": destination.root.display().to_string(),
                "source": destination.source,
                "skills": skills.iter().map(|skill| skill.name.clone()).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
    });

    output::emit(mode, &lines.join("\n"), json)
}

/// Removes installed skill directories.
///
/// Makes no request: what is on disk is the whole input, which is also what
/// lets it remove a skill that has since been unpublished.
///
/// Without a manifest it cannot know *this* command installed a given
/// directory — see [`compare`] for why there is no manifest. Three things
/// stand in for one: a directory that does not look like a skill (no
/// [`ENTRY_FILE`]) is refused unless `--force`, a terminal is asked before
/// anything is deleted, and `--dry-run` lists the directories first.
fn uninstall(
    wanted: &[String],
    destinations: &[Destination],
    force: bool,
    dry_run: bool,
    assume_yes: bool,
    mode: Mode,
) -> Result<()> {
    // Before anything is turned into a path. `remove_dir_all` is on the other
    // end of this, and `Path::join` on an absolute argument *replaces* the
    // path rather than extending it — so `--dir ./skills` is no boundary at
    // all unless the name is checked.
    let escaping: Vec<&String> = wanted.iter().filter(|name| !is_skill_name(name)).collect();
    if !escaping.is_empty() {
        let listed = escaping
            .iter()
            .map(|name| format!("  {name}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CliError::new(
            "invalid_name",
            format!(
                "A skill name is one directory name. These are not, and this command \
                 removes directories:\n{listed}"
            ),
        )
        .with_remedy(Remedy::default().with_action(Some(format!("mapbox {COMMAND} {LIST}"))))
        .into());
    }

    // Deduped, so naming one twice is removing it once. Without this the
    // second `remove_dir_all` fails on a directory the first one removed, and
    // the command reports a failure after doing exactly what was asked.
    let mut wanted: Vec<&String> = wanted.iter().collect();
    wanted.sort_unstable();
    wanted.dedup();

    let mut targets: Vec<PathBuf> = vec![];
    let mut absent: Vec<&String> = vec![];

    for name in wanted {
        let found: Vec<PathBuf> = destinations
            .iter()
            .map(|destination| destination.root.join(name))
            .filter(|path| path.is_dir())
            .collect();
        if found.is_empty() {
            absent.push(name);
        }
        targets.extend(found);
    }

    if !absent.is_empty() {
        let looked = destinations
            .iter()
            .map(|destination| destination.root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CliError::new(
            "not_installed",
            format!(
                "Not installed in {looked}: {}.",
                absent
                    .iter()
                    .map(|name| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .with_remedy(Remedy::default().with_action(Some(format!("mapbox {COMMAND} {LIST}"))))
        .into());
    }

    // A directory with no SKILL.md in it is not something this command put
    // there, whatever its name.
    if !force {
        let strangers: Vec<&PathBuf> = targets
            .iter()
            .filter(|path| !path.join(ENTRY_FILE).is_file())
            .collect();
        if !strangers.is_empty() {
            let listed = strangers
                .iter()
                .map(|path| format!("  {}", path.display()))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(CliError::new(
                "not_a_skill",
                format!(
                    "These directories hold no {ENTRY_FILE}, so they are not skills this \
                     command installed:\n{listed}"
                ),
            )
            .with_remedy(
                Remedy::default()
                    .with_fix("Pass --force to remove them anyway, or delete them by hand."),
            )
            .into());
        }
    }

    let listed = targets
        .iter()
        .map(|path| format!("  {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");

    if dry_run {
        return output::emit(
            mode,
            &format!("Dry run — nothing was removed.\nWould remove:\n{listed}"),
            json!({
                "dry_run": true,
                "removed": targets
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>(),
            }),
        );
    }

    confirm::destructive_local_action(
        &format!(
            "About to remove {} skill directories:\n{listed}",
            targets.len()
        ),
        assume_yes,
    )?;

    for path in &targets {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("Could not remove {}", path.display()))?;
    }

    output::emit(
        mode,
        &format!(
            "Removed {} skill director{}:\n{listed}",
            targets.len(),
            if targets.len() == 1 { "y" } else { "ies" }
        ),
        json!({
            "dry_run": false,
            "removed": targets
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
        }),
    )
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// A gzipped tarball shaped like a GitHub repository archive: one wrapper
    /// directory, then the paths given.
    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        archive_with(entries, tar::EntryType::Regular)
    }

    fn archive_with(entries: &[(&str, &[u8])], kind: tar::EntryType) -> Vec<u8> {
        let mut builder = tar::Builder::new(vec![]);
        for (path, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_entry_type(kind);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(
                    &mut header,
                    format!("mapbox-agent-skills-main/{path}"),
                    *bytes,
                )
                .expect("append");
        }
        let tar = builder.into_inner().expect("finish tar");

        let mut encoder = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast());
        encoder.write_all(&tar).expect("gzip");
        encoder.finish().expect("finish gzip")
    }

    const SKILL_MD: &[u8] = b"---\nname: mapbox-cartography\ndescription: Map design, color and type. Use when styling.\n---\n\n# Cartography\n";

    fn one_skill() -> Vec<u8> {
        archive(&[
            ("README.md", b"the repo readme"),
            ("skills/README.md", b"the skills readme"),
            ("skills/mapbox-cartography/SKILL.md", SKILL_MD),
            ("skills/mapbox-cartography/AGENTS.md", b"# same, for Codex"),
            (
                "skills/mapbox-cartography/references/scenarios.md",
                b"# scenarios",
            ),
            (
                "skills/mapbox-cartography/evals/evals.json",
                b"{\"upstream\":\"tooling\"}",
            ),
        ])
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mapbox-cli-agent-skills-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn destination(root: &Path) -> Destination {
        Destination {
            root: root.to_path_buf(),
            source: None,
        }
    }

    #[test]
    fn the_skills_come_out_and_the_repository_scaffolding_does_not() {
        let skills = extract(&one_skill()).expect("a readable archive");
        assert_eq!(skills.len(), 1, "{skills:?}");

        let skill = &skills[0];
        assert_eq!(skill.name, "mapbox-cartography");
        assert_eq!(
            skill.description.as_deref(),
            Some("Map design, color and type. Use when styling.")
        );

        let paths: Vec<String> = skill
            .files
            .keys()
            .map(|path| path.display().to_string().replace('\\', "/"))
            .collect();
        assert_eq!(
            paths,
            ["AGENTS.md", "SKILL.md", "references/scenarios.md"],
            "the repo readme, the skills readme or evals/ came through"
        );
    }

    /// `evals/` is upstream's test tooling. It is 20 of the 220 files and none
    /// of them is content an agent should load.
    #[test]
    fn evals_never_lands() {
        let skills = extract(&one_skill()).expect("a readable archive");
        for skill in &skills {
            for path in skill.files.keys() {
                assert!(
                    !path.components().any(|part| part.as_os_str() == "evals"),
                    "{} came out of the archive",
                    path.display()
                );
            }
        }
    }

    /// A tarball holding a path the `tar` crate refuses to *write*.
    ///
    /// `Builder::append_data` rejects `..` outright, which is the right
    /// default for a writer and useless for a test whose subject is a reader
    /// meeting a hostile archive. So the name goes into the header's bytes
    /// directly, which is what an attacker's tar would do.
    fn archive_with_raw_path(path: &str, bytes: &[u8]) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        {
            let name = &mut header.as_old_mut().name;
            let written = path.as_bytes();
            assert!(written.len() < name.len(), "the test path is too long");
            name[..written.len()].copy_from_slice(written);
        }
        header.set_cksum();

        let mut builder = tar::Builder::new(vec![]);
        builder.append(&header, bytes).expect("append a raw entry");
        let tar = builder.into_inner().expect("finish tar");

        let mut encoder = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast());
        encoder.write_all(&tar).expect("gzip");
        encoder.finish().expect("finish gzip")
    }

    /// An entry that climbs out of its directory stops the extraction. Not
    /// "is skipped": an archive containing one is not to be trusted for the
    /// entries around it either.
    #[test]
    fn a_path_that_escapes_is_refused() {
        for escape in [
            "mapbox-agent-skills-main/skills/../../etc/passwd",
            "mapbox-agent-skills-main/skills/mapbox-cartography/../../outside.md",
            "mapbox-agent-skills-main/../outside.md",
        ] {
            let archive = archive_with_raw_path(escape, b"no");
            let err = extract(&archive).expect_err("an escaping path was accepted");
            assert!(
                err.to_string().contains("outside"),
                "{escape} was refused for the wrong reason: {err}"
            );
        }
    }

    /// An absolute path, which is the other classic tarball escape.
    #[test]
    fn an_absolute_path_is_refused() {
        let archive = archive_with_raw_path("/etc/passwd", b"no");
        let err = extract(&archive).expect_err("an absolute path was accepted");
        assert!(err.to_string().contains("outside"), "{err}");
    }

    #[test]
    fn is_contained_rejects_everything_that_is_not_a_plain_relative_path() {
        assert!(is_contained(Path::new("skills/a/SKILL.md")));
        assert!(!is_contained(Path::new("")));
        assert!(!is_contained(Path::new("/absolute")));
        assert!(!is_contained(Path::new("../up")));
        assert!(!is_contained(Path::new("a/../../b")));
        // A Windows prefix is a `Prefix` component there and would be a
        // `Normal` one here, which is why the check is on components rather
        // than on the string.
        assert!(is_contained(Path::new("a/b")));
    }

    /// A symlink is a way out of the destination that no path check catches,
    /// since the path itself is innocent.
    #[test]
    fn only_regular_files_are_taken() {
        let archive = archive_with(&[("skills/evil/SKILL.md", b"")], tar::EntryType::Symlink);
        let err = extract(&archive).expect_err("a symlink-only archive has no skills");
        assert!(err.to_string().contains("no skills"), "{err}");
    }

    /// A directory with no `SKILL.md` is not a skill: an agent would find
    /// nothing to read in it.
    #[test]
    fn a_directory_without_an_entry_file_is_not_a_skill() {
        let archive = archive(&[
            ("skills/not-a-skill/notes.md", b"# notes"),
            ("skills/real/SKILL.md", SKILL_MD),
        ]);
        let skills = extract(&archive).expect("one real skill");
        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["real"]
        );
    }

    #[test]
    fn an_archive_with_no_skills_at_all_says_so() {
        let archive = archive(&[("README.md", b"nothing else")]);
        let err = extract(&archive).expect_err("no skills");
        assert!(err.to_string().contains("no skills"), "{err}");
    }

    #[test]
    fn a_description_is_read_from_the_frontmatter_and_folded() {
        assert_eq!(
            frontmatter_description(b"---\nname: x\ndescription: One line.\n---\nbody"),
            Some("One line.".to_string())
        );
        // Folded over several lines, which upstream does.
        assert_eq!(
            frontmatter_description(
                b"---\ndescription: >-\n  Two lines\n  folded into one.\n---\nbody"
            ),
            Some("Two lines folded into one.".to_string())
        );
        // A colon in the value is why this is parsed rather than scanned.
        assert_eq!(
            frontmatter_description(b"---\ndescription: \"Tokens: keep them secret\"\n---\n"),
            Some("Tokens: keep them secret".to_string())
        );
        assert_eq!(frontmatter_description(b"no frontmatter here"), None);
        assert_eq!(frontmatter_description(b"---\nname: x\n---\n"), None);
    }

    #[test]
    fn the_archive_url_takes_a_branch_a_tag_or_a_sha() {
        assert_eq!(
            archive_url("https://codeload.github.com", "main"),
            "https://codeload.github.com/mapbox/mapbox-agent-skills/tar.gz/main"
        );
        assert_eq!(
            archive_url("https://codeload.github.com", "deadbeef"),
            "https://codeload.github.com/mapbox/mapbox-agent-skills/tar.gz/deadbeef"
        );
    }

    #[test]
    fn an_unknown_name_is_an_error_that_suggests_one() {
        let skills = extract(&one_skill()).expect("a readable archive");
        let err = selected(&skills, &["cartography".to_string()])
            .expect_err("`cartography` is not the published name");
        assert!(err.to_string().contains("mapbox-cartography"), "{err}");

        // And naming nothing is naming everything.
        assert_eq!(selected(&skills, &[]).unwrap().len(), skills.len());
    }

    /// Naming one skill twice is naming it once, wherever the repeats sit in
    /// the list. `Vec::dedup_by` only collapses adjacent duplicates, so
    /// `foo bar foo` used to survive as three and fail part-way through the
    /// install — after the first copy had already been written.
    #[test]
    fn naming_one_skill_twice_is_one_install_however_they_are_ordered() {
        let archive = archive(&[
            ("skills/aaa/SKILL.md", SKILL_MD),
            ("skills/bbb/SKILL.md", SKILL_MD),
        ]);
        let skills = extract(&archive).expect("two skills");

        for wanted in [
            vec!["aaa", "aaa"],
            vec!["aaa", "bbb", "aaa"],
            vec!["bbb", "aaa", "bbb", "aaa"],
        ] {
            let asked: Vec<String> = wanted.iter().map(|n| n.to_string()).collect();
            let chosen = selected(&skills, &asked).expect("all published");
            let mut names: Vec<&str> = chosen.iter().map(|s| s.name.as_str()).collect();
            let before = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(
                before,
                names.len(),
                "{wanted:?} produced a duplicate: {names:?}"
            );
        }

        // And the order the caller asked in is kept, first mention winning,
        // since it is the order the install reports back.
        let asked = ["bbb".to_string(), "aaa".to_string(), "bbb".to_string()];
        let chosen = selected(&skills, &asked).expect("all published");
        assert_eq!(
            chosen.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["bbb", "aaa"]
        );
    }

    /// The whole way through, not just past `selected`: a repeat used to reach
    /// `write_into` and fail on its own freshly-created directory.
    #[test]
    fn a_repeated_name_installs_once_and_succeeds() {
        let root = scratch("install-repeat");
        let archive = archive(&[
            ("skills/aaa/SKILL.md", SKILL_MD),
            ("skills/bbb/SKILL.md", SKILL_MD),
        ]);
        let (server, base) = serve("200 OK", archive);

        let matches = command()
            .try_get_matches_from([
                COMMAND,
                INSTALL,
                "aaa",
                "bbb",
                "aaa",
                "--dir",
                &root.display().to_string(),
            ])
            .expect("a valid command line");
        run_with(
            &matches,
            RunFlags::default(),
            Mode::Json { pretty: false },
            &base,
        )
        .expect("a repeated name is not an error");
        let _ = server.join();

        assert!(root.join("aaa/SKILL.md").is_file());
        assert!(root.join("bbb/SKILL.md").is_file());
    }

    #[test]
    fn installing_writes_the_files_and_nothing_else() {
        let root = scratch("install");
        let skills = extract(&one_skill()).expect("a readable archive");
        let chosen: Vec<&Skill> = skills.iter().collect();
        let dest = destination(&root);

        write_into(&dest, &chosen, false).expect("install");

        assert!(root.join("mapbox-cartography/SKILL.md").is_file());
        assert!(root.join("mapbox-cartography/AGENTS.md").is_file());
        assert!(root
            .join("mapbox-cartography/references/scenarios.md")
            .is_file());
        assert!(
            !root.join("mapbox-cartography/evals").exists(),
            "evals/ was installed"
        );
        assert!(
            !root.join(".mapbox-agent-skills.staging").exists(),
            "the staging directory was left behind"
        );
    }

    /// An existing directory stops the install, and `--force` is the way past
    /// it. The distinction matters because the directory may hold edits.
    #[test]
    fn an_existing_skill_is_a_conflict_until_force() {
        let root = scratch("conflict");
        let skills = extract(&one_skill()).expect("a readable archive");
        let chosen: Vec<&Skill> = skills.iter().collect();
        let dest = destination(&root);

        std::fs::create_dir_all(root.join("mapbox-cartography")).expect("create");
        std::fs::write(root.join("mapbox-cartography/mine.md"), b"my edit").expect("write");

        assert_eq!(conflicts(&dest, &chosen).len(), 1);

        write_into(&dest, &chosen, true).expect("--force replaces it");
        assert!(root.join("mapbox-cartography/SKILL.md").is_file());
        assert!(
            !root.join("mapbox-cartography/mine.md").exists(),
            "--force merged instead of replacing"
        );
    }

    /// The plan is every file, named where it would land.
    #[test]
    fn the_plan_names_every_file_under_the_destination() {
        let skills = extract(&one_skill()).expect("a readable archive");
        let chosen: Vec<&Skill> = skills.iter().collect();
        let dest = destination(Path::new(".claude/skills"));

        let planned: Vec<String> = plan(&dest, &chosen)
            .iter()
            .map(|path| path.display().to_string().replace('\\', "/"))
            .collect();
        assert_eq!(
            planned,
            [
                ".claude/skills/mapbox-cartography/AGENTS.md",
                ".claude/skills/mapbox-cartography/SKILL.md",
                ".claude/skills/mapbox-cartography/references/scenarios.md",
            ]
        );
    }

    /// The bug this file had three times before `shared` existed: the four
    /// subcommands declare different arguments, and asking clap for one a
    /// subcommand never declared panics rather than answering `None`. Every
    /// one of `list`, `install`, `update` and `uninstall` goes through the
    /// same prologue, so every one of them is walked here.
    #[test]
    fn every_subcommand_survives_the_shared_prologue() {
        let lines: &[(&str, Vec<&str>)] = &[
            (LIST, vec![COMMAND, LIST]),
            (INSTALL, vec![COMMAND, INSTALL]),
            (UPDATE, vec![COMMAND, UPDATE]),
            (UNINSTALL, vec![COMMAND, UNINSTALL, "mapbox-cartography"]),
        ];

        for (action, argv) in lines {
            let matches = command()
                .try_get_matches_from(argv)
                .unwrap_or_else(|e| panic!("`{action}` is not a valid command line: {e}"));
            let (parsed, action_matches) = matches.subcommand().expect("a subcommand");
            assert_eq!(&parsed, action);

            // The panic, if there is one, happens in here.
            let shared = shared(action_matches);
            assert_eq!(shared.git_ref, DEFAULT_REF);
            assert!(!shared.dry_run);
        }

        // And the arguments that are declared are read, rather than being
        // swallowed by the same leniency.
        let matches = command()
            .try_get_matches_from([COMMAND, UPDATE, "a", "b", "--ref", "v2", "--dry-run"])
            .expect("a valid command line");
        let shared = shared(matches.subcommand().expect("a subcommand").1);
        assert_eq!(shared.git_ref, "v2");
        assert_eq!(shared.wanted, ["a", "b"]);
        assert!(shared.dry_run);
    }

    /// `uninstall` names what it removes, so it cannot be called with nothing
    /// — `mapbox agent-skills uninstall` removing every skill would be a very
    /// bad reading of an empty list.
    #[test]
    fn uninstall_needs_a_name() {
        let err = command()
            .try_get_matches_from([COMMAND, UNINSTALL])
            .expect_err("a name is required");
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::MissingRequiredArgument,
            "{err}"
        );
    }

    /// The comparison that stands in for a manifest: byte-for-byte against
    /// what was published, in both directions.
    #[test]
    fn a_skill_compares_equal_only_when_every_byte_matches() {
        let root = scratch("compare");
        let skills = extract(&one_skill()).expect("a readable archive");
        let skill = &skills[0];
        let dest = destination(&root);

        write_into(&dest, &[skill], false).expect("install");
        assert_eq!(compare(&root, skill), Change::Unchanged);

        // An edit on our side.
        std::fs::write(root.join("mapbox-cartography/SKILL.md"), b"edited").expect("edit");
        assert_eq!(compare(&root, skill), Change::Changed);
        write_into(&dest, &[skill], true).expect("restore");
        assert_eq!(compare(&root, skill), Change::Unchanged);

        // A file that is no longer published but is still on disk: an update
        // that left it would be a mixture of two versions.
        std::fs::write(root.join("mapbox-cartography/stale.md"), b"left over").expect("write");
        assert_eq!(compare(&root, skill), Change::Changed);
        std::fs::remove_file(root.join("mapbox-cartography/stale.md")).expect("remove");
        assert_eq!(compare(&root, skill), Change::Unchanged);

        // Missing entirely.
        std::fs::remove_dir_all(root.join("mapbox-cartography")).expect("remove");
        assert_eq!(compare(&root, skill), Change::Changed);
    }

    /// Update touches what changed and leaves the rest alone — and never
    /// installs something that was not there, which is `install`'s job.
    #[test]
    fn update_rewrites_only_what_differs_and_adds_nothing() {
        let root = scratch("update");
        let (server, base) = serve("200 OK", one_skill());
        let skills = extract(&one_skill()).expect("a readable archive");
        let dest = destination(&root);
        write_into(&dest, &[&skills[0]], false).expect("install");
        std::fs::write(root.join("mapbox-cartography/SKILL.md"), b"edited").expect("edit");

        // A second skill is published but not installed, so update must not
        // bring it in.
        let matches = command()
            .try_get_matches_from([COMMAND, UPDATE, "--dir", &root.display().to_string()])
            .expect("a valid command line");
        run_with(
            &matches,
            RunFlags {
                debug: false,
                assume_yes: true,
            },
            Mode::Json { pretty: false },
            &base,
        )
        .expect("update");
        let _ = server.join();

        assert_eq!(
            std::fs::read(root.join("mapbox-cartography/SKILL.md")).expect("read"),
            SKILL_MD,
            "the edited file was not restored"
        );
        assert_eq!(compare(&root, &skills[0]), Change::Unchanged);
    }

    /// A dry-run update reports and changes nothing.
    #[test]
    fn a_dry_run_update_changes_nothing() {
        let root = scratch("update-dry-run");
        let (server, base) = serve("200 OK", one_skill());
        let skills = extract(&one_skill()).expect("a readable archive");
        write_into(&destination(&root), &[&skills[0]], false).expect("install");
        std::fs::write(root.join("mapbox-cartography/SKILL.md"), b"edited").expect("edit");

        let matches = command()
            .try_get_matches_from([
                COMMAND,
                UPDATE,
                "--dir",
                &root.display().to_string(),
                "--dry-run",
            ])
            .expect("a valid command line");
        run_with(
            &matches,
            RunFlags::default(),
            Mode::Json { pretty: false },
            &base,
        )
        .expect("dry run");
        let _ = server.join();

        assert_eq!(
            std::fs::read(root.join("mapbox-cartography/SKILL.md")).expect("read"),
            b"edited",
            "a dry run rewrote the file"
        );
    }

    /// Uninstall removes the directory, and refuses one that is not a skill
    /// until `--force` — the stand-in for the manifest it does not keep.
    #[test]
    fn uninstall_removes_a_skill_and_refuses_a_stranger() {
        let root = scratch("uninstall");
        let skills = extract(&one_skill()).expect("a readable archive");
        let dest = destination(&root);
        write_into(&dest, &[&skills[0]], false).expect("install");
        std::fs::create_dir_all(root.join("not-a-skill")).expect("create");

        // A directory with no SKILL.md is not something this installed.
        let err = uninstall(
            &["not-a-skill".to_string()],
            std::slice::from_ref(&dest),
            false,
            false,
            true,
            Mode::Json { pretty: false },
        )
        .expect_err("a stranger is refused");
        assert!(err.to_string().contains(ENTRY_FILE), "{err}");
        assert!(root.join("not-a-skill").exists(), "it was removed anyway");

        // A dry run removes nothing.
        uninstall(
            &["mapbox-cartography".to_string()],
            std::slice::from_ref(&dest),
            false,
            true,
            true,
            Mode::Json { pretty: false },
        )
        .expect("dry run");
        assert!(root.join("mapbox-cartography").is_dir());

        // And the real thing removes exactly that directory.
        uninstall(
            &["mapbox-cartography".to_string()],
            std::slice::from_ref(&dest),
            false,
            false,
            true,
            Mode::Json { pretty: false },
        )
        .expect("uninstall");
        assert!(!root.join("mapbox-cartography").exists());
        assert!(root.join("not-a-skill").exists(), "it removed too much");

        // `--force` reaches the stranger.
        uninstall(
            &["not-a-skill".to_string()],
            std::slice::from_ref(&dest),
            true,
            false,
            true,
            Mode::Json { pretty: false },
        )
        .expect("--force");
        assert!(!root.join("not-a-skill").exists());
    }

    /// The one that matters most in this module: `uninstall` takes a name
    /// straight from the command line and hands it to `remove_dir_all`, and
    /// `Path::join` on an absolute argument *replaces* the path rather than
    /// extending it. Before `is_skill_name`, `uninstall ../victim --dir ./out`
    /// deleted `../victim`, and an absolute path to a real skill elsewhere was
    /// removed without even needing `--force`, because the `SKILL.md` guard
    /// only asks what a directory looks like, not where it is.
    #[test]
    fn a_name_that_escapes_the_destination_is_refused() {
        let root = scratch("escape");
        let victim = root.join("victim");
        std::fs::create_dir_all(&victim).expect("create");
        std::fs::write(victim.join(ENTRY_FILE), b"---\n").expect("write");

        let dest = destination(&root.join("out"));
        std::fs::create_dir_all(&dest.root).expect("create");

        for name in [
            "../victim",
            "..",
            "a/b",
            "",
            &victim.display().to_string(),
            "./victim",
        ] {
            let err = uninstall(
                &[name.to_string()],
                std::slice::from_ref(&dest),
                // `--force` must not buy a way out of the destination either.
                true,
                false,
                true,
                Mode::Json { pretty: false },
            )
            .expect_err("a name outside the destination was accepted");
            assert!(
                err.to_string().contains("one directory name"),
                "`{name}` was refused for the wrong reason: {err}"
            );
            assert!(
                victim.is_dir(),
                "`{name}` removed a directory outside the destination"
            );
        }
    }

    #[test]
    fn is_skill_name_accepts_one_directory_name_and_nothing_else() {
        assert!(is_skill_name("mapbox-cartography"));
        assert!(is_skill_name("a"));
        assert!(!is_skill_name(""));
        assert!(!is_skill_name(".."));
        assert!(!is_skill_name("../up"));
        assert!(!is_skill_name("a/b"));
        assert!(!is_skill_name("/absolute"));
        assert!(!is_skill_name("./here"));
    }

    /// Naming one skill twice removes it once, and succeeds. It used to
    /// remove the directory and then fail on the second `remove_dir_all`,
    /// reporting an error after doing exactly what was asked.
    #[test]
    fn uninstalling_the_same_name_twice_removes_it_once() {
        let root = scratch("uninstall-twice");
        let skills = extract(&one_skill()).expect("a readable archive");
        let dest = destination(&root);
        write_into(&dest, &[&skills[0]], false).expect("install");

        uninstall(
            &[
                "mapbox-cartography".to_string(),
                "mapbox-cartography".to_string(),
            ],
            std::slice::from_ref(&dest),
            false,
            false,
            true,
            Mode::Json { pretty: false },
        )
        .expect("naming one twice is removing it once");
        assert!(!root.join("mapbox-cartography").exists());
    }

    /// A skill installed for two agents is one skill, not two. The counts and
    /// the JSON arrays are per skill; the destinations are where they went.
    #[test]
    fn update_counts_each_skill_once_across_destinations() {
        let root = scratch("update-two-destinations");
        let skills = extract(&one_skill()).expect("a readable archive");
        let first = destination(&root.join("one"));
        let second = destination(&root.join("two"));
        for dest in [&first, &second] {
            std::fs::create_dir_all(&dest.root).expect("create");
            write_into(dest, &[&skills[0]], false).expect("install");
            std::fs::write(dest.root.join("mapbox-cartography/SKILL.md"), b"edited").expect("edit");
        }

        // Rendered rather than returned, so the assertion is on what a caller
        // sees: `emit` writes to stdout, and the count is what matters here.
        update(
            &skills,
            &[],
            &[first.clone(), second.clone()],
            "main",
            false,
            true,
            Mode::Json { pretty: false },
        )
        .expect("update");

        // Both copies were rewritten...
        for dest in [&first, &second] {
            assert_eq!(compare(&dest.root, &skills[0]), Change::Unchanged);
        }
        // ...and a second pass finds nothing to do, which is the same
        // deduplication seen from the other side.
        update(
            &skills,
            &[],
            &[first, second],
            "main",
            false,
            true,
            Mode::Json { pretty: false },
        )
        .expect("second update");
    }

    /// Removing something that is not installed is an error naming where it
    /// looked, not a silent success.
    #[test]
    fn uninstalling_what_is_not_there_says_so() {
        let root = scratch("uninstall-absent");
        let err = uninstall(
            &["mapbox-cartography".to_string()],
            &[destination(&root)],
            false,
            false,
            true,
            Mode::Json { pretty: false },
        )
        .expect_err("nothing to remove");
        let message = err.to_string();
        assert!(message.contains("mapbox-cartography"), "{message}");
        assert!(message.contains(&root.display().to_string()), "{message}");
    }

    /// A loopback stand-in for codeload, so the whole command can be driven
    /// without the network.
    fn serve(status: &'static str, body: Vec<u8>) -> (std::thread::JoinHandle<String>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");

        let server = std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return String::new();
            };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match std::io::Read::read(&mut stream, &mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/gzip\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&body);
            String::from_utf8_lossy(&head).into_owned()
        });

        (server, format!("http://{addr}"))
    }

    #[test]
    fn a_fetch_asks_for_the_ref_it_was_given() {
        let (server, base) = serve("200 OK", one_skill());
        let bytes = fetch(&base, "v1.2.3", false).expect("the archive");
        assert!(!bytes.is_empty());

        let head = server.join().expect("the server thread");
        assert!(
            head.starts_with("GET /mapbox/mapbox-agent-skills/tar.gz/v1.2.3"),
            "{head}"
        );
        // The same user agent every other request carries.
        assert!(head.contains("mapbox-cli/"), "{head}");
    }

    #[test]
    fn a_missing_ref_is_reported_as_one() {
        let (server, base) = serve("404 Not Found", b"Not Found".to_vec());
        let err = fetch(&base, "no-such-branch", false).expect_err("404");
        let _ = server.join();
        let message = err.to_string();
        assert!(message.contains("no-such-branch"), "{message}");
    }

    /// `--dry-run` is the flag that has to be exactly true: it prints the same
    /// plan the install would carry out, and writes none of it.
    #[test]
    fn a_dry_run_writes_nothing() {
        let root = scratch("dry-run");
        let (server, base) = serve("200 OK", one_skill());

        let matches = crate::agent_skills::command()
            .try_get_matches_from([
                COMMAND,
                INSTALL,
                "--dir",
                &root.display().to_string(),
                "--dry-run",
            ])
            .expect("a valid command line");
        run_with(
            &matches,
            RunFlags::default(),
            Mode::Json { pretty: false },
            &base,
        )
        .expect("dry run");
        let _ = server.join();

        assert_eq!(
            std::fs::read_dir(&root)
                .expect("read the destination")
                .count(),
            0,
            "a dry run wrote something"
        );
    }

    #[test]
    fn an_install_through_the_command_line_lands_where_dir_says() {
        let root = scratch("through-the-command-line");
        let (server, base) = serve("200 OK", one_skill());

        let matches = crate::agent_skills::command()
            .try_get_matches_from([COMMAND, INSTALL, "--dir", &root.display().to_string()])
            .expect("a valid command line");
        run_with(
            &matches,
            RunFlags::default(),
            Mode::Json { pretty: false },
            &base,
        )
        .expect("install");
        let _ = server.join();

        assert!(root.join("mapbox-cartography/SKILL.md").is_file());
    }
}
