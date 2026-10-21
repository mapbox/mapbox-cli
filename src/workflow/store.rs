//! Where installed workflows live, and how they get there.
//!
//! Nothing is bundled into the binary: a workflow runs only once it has been
//! installed, from a local directory or from a GitHub repository laid out as
//! `workflow/<name>/`, into `<config dir>/workflows/<name>/`. A copy
//! is taken rather than a link kept, so what runs is what was checked at
//! install time and nothing edited since.
//!
//! The same rules as `agent-skills` hold for the write, for the same
//! reasons: everything is read and checked in memory first, an existing
//! workflow stops the install unless `--force`, only regular files are
//! taken, and a path that would leave the workflow's directory stops the
//! read altogether. The new directory is staged and renamed into place, and
//! one it replaces is moved aside until then rather than removed first.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use super::definition::{self, is_contained, is_workflow_name, Files, Workflow};
use crate::auth;
use crate::executor;
use crate::http;
use crate::output::CliError;
use crate::remedy::Remedy;

const DIR_NAME: &str = "workflows";

/// Written beside an installed workflow's files, and what marks a directory
/// as one this command installed — `uninstall` removes nothing without it.
const META_FILE: &str = ".install.json";

/// Where the tarball comes from. The API rather than codeload directly,
/// because the API is what honors a token for a private repository; it
/// answers with a redirect to a signed codeload URL, and `reqwest` drops the
/// `Authorization` header when it follows a redirect to another host.
const GITHUB_API: &str = "https://api.github.com";
/// This repository. Not `REPO_URL`'s `mapbox/cli`: on GitHub that name
/// redirects to a different repository.
pub const DEFAULT_REPO: &str = "mapbox/mapbox-cli";
pub const DEFAULT_REF: &str = "main";

/// The directory inside a repository that holds its workflows, one
/// directory each.
pub const REPO_PREFIX: &str = "workflow";

/// A ceiling on what is read, from a tarball or a local directory. Far above
/// any real workflow, and there so a hostile archive cannot make this
/// allocate without bound.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

fn too_large(what: &str, limit: u64) -> anyhow::Error {
    anyhow!(
        "{what} is larger than {} MiB, more than a workflow should be",
        limit / (1024 * 1024)
    )
}

/// All of `reader`, or an error once it passes `limit`. One byte more than
/// the limit is read, so an oversized source is said to be one rather than
/// cut short into something that reads as corrupt.
fn read_capped(reader: impl Read, limit: u64, what: &str) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("Could not read {what}"))?;
    if bytes.len() as u64 > limit {
        return Err(too_large(what, limit));
    }
    Ok(bytes)
}

/// The files `read_local` passes over rather than refusing. Finder writes
/// `.DS_Store` into any directory it opens.
const IGNORED: &[&str] = &[".DS_Store"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    /// Where it came from, as a person would write it: a directory, or
    /// `github:owner/repo`.
    pub source: String,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    /// The commit `git_ref` named when it was installed, since a branch
    /// moves. Absent for a local install, and for one made before it was
    /// recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub installed_at: u64,
}

/// A workflow read and checked, not yet written anywhere.
pub struct Package {
    pub workflow: Workflow,
    pub files: Files,
    pub meta: Meta,
}

/// A workflow on disk.
pub struct Installed {
    pub root: PathBuf,
    pub workflow: Workflow,
    pub meta: Meta,
}

/// `<config dir>/workflows`, resolved and not created.
pub fn root_path() -> Result<PathBuf> {
    auth::config_dir_path()
        .map(|dir| dir.join(DIR_NAME))
        .ok_or_else(|| anyhow!("Could not determine home directory"))
}

pub fn invalid_workflow(name: &str, problems: &[String]) -> anyhow::Error {
    let listed = problems
        .iter()
        .map(|problem| format!("  - {problem}"))
        .collect::<Vec<_>>()
        .join("\n");
    CliError::new(
        "invalid_workflow",
        format!("`{name}` is not a valid workflow:\n{listed}"),
    )
    .into()
}

/// A workflow name from the command line, checked before it is ever joined
/// to a path.
pub fn check_name(name: &str) -> Result<()> {
    if is_workflow_name(name) {
        return Ok(());
    }
    Err(CliError::new(
        "invalid_name",
        format!("`{name}` is not a workflow name: lower-case letters, digits and dashes"),
    )
    .with_remedy(Remedy::default().with_action(Some("mapbox workflow list".to_string())))
    .into())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Every regular file under `dir`, by path relative to it.
///
/// A symlink is refused rather than followed: it is a way to install
/// something from outside the directory that was named.
fn read_tree(dir: &Path, skip: &[&str], limit: u64) -> Result<Files> {
    let mut files = Files::new();
    let mut total: u64 = 0;
    let what = dir.display().to_string();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = std::fs::read_dir(&current)
            .with_context(|| format!("Could not read {}", current.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("Could not read {}", current.display()))?;
            let path = entry.path();
            let relative = path.strip_prefix(dir).expect("under the directory walked");
            let name = entry.file_name();
            if current == dir && skip.iter().any(|s| name == *s) {
                continue;
            }
            if IGNORED.iter().any(|ignored| name == *ignored) {
                continue;
            }
            let kind = entry
                .file_type()
                .with_context(|| format!("Could not read {}", path.display()))?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                // Capped at what is left, so one huge file is refused
                // without being read whole first.
                let mut bytes = vec![];
                std::fs::File::open(&path)
                    .and_then(|file| file.take(limit - total + 1).read_to_end(&mut bytes))
                    .with_context(|| format!("Could not read {}", path.display()))?;
                total += bytes.len() as u64;
                if total > limit {
                    return Err(too_large(&what, limit));
                }
                files.insert(relative.to_path_buf(), bytes);
            } else {
                return Err(CliError::new(
                    "invalid_workflow",
                    format!(
                        "{} is not a regular file; a workflow holds only files and directories",
                        path.display()
                    ),
                )
                .into());
            }
        }
    }
    Ok(files)
}

fn package(name: &str, files: Files, meta: Meta) -> Result<Package> {
    let workflow = definition::parse(name, &files).map_err(|p| invalid_workflow(name, &p))?;
    Ok(Package {
        workflow,
        files,
        meta,
    })
}

/// A workflow from a directory on this machine. The directory's own name is
/// the workflow's, as it is in a repository.
pub fn read_local(dir: &Path) -> Result<Package> {
    let dir = dir
        .canonicalize()
        .with_context(|| format!("There is no directory {}", dir.display()))?;
    if !dir.is_dir() {
        return Err(anyhow!("{} is not a directory", dir.display()));
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let files = read_tree(&dir, &[], MAX_BYTES)?;
    package(
        &name,
        files,
        Meta {
            source: dir.display().to_string(),
            git_ref: None,
            commit: None,
            installed_at: now(),
        },
    )
}

/// `owner/repo`, and nothing that could move the request elsewhere.
pub fn check_repo(repo: &str) -> Result<()> {
    let part = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if part(owner) && part(name) => Ok(()),
        _ => Err(CliError::new(
            "invalid_repo",
            format!("`{repo}` is not a GitHub repository; write it as OWNER/REPO"),
        )
        .into()),
    }
}

/// A branch, tag or commit. `/` is allowed, since a branch may hold one;
/// URL syntax and `..` are not, since the ref goes into a URL path.
pub fn check_ref(git_ref: &str) -> Result<()> {
    let ok = !git_ref.is_empty()
        && !git_ref
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && git_ref
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    if ok {
        return Ok(());
    }
    Err(CliError::new(
        "invalid_ref",
        format!("`{git_ref}` is not a branch, tag or commit this can fetch"),
    )
    .into())
}

fn github_token() -> Option<String> {
    ["GH_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
}

/// A workflow out of a GitHub repository's tarball.
pub fn fetch_github(name: &str, repo: &str, git_ref: &str, debug: bool) -> Result<Package> {
    fetch_github_from(GITHUB_API, name, repo, git_ref, debug)
}

fn fetch_github_from(
    base: &str,
    name: &str,
    repo: &str,
    git_ref: &str,
    debug: bool,
) -> Result<Package> {
    check_name(name)?;
    check_repo(repo)?;
    check_ref(git_ref)?;

    // The commit first, then the tarball of that commit rather than of the
    // ref, so what is recorded is what was downloaded even if the branch
    // moves in between.
    let token = github_token();
    let commit = get(
        &format!("{base}/repos/{repo}/commits/{git_ref}"),
        "application/vnd.github.sha",
        token.as_deref(),
        (repo, git_ref),
        debug,
    )?
    .text()
    .map_err(|e| anyhow!("Could not read the commit from GitHub: {e}"))?;
    let commit = commit.trim().to_string();
    if !is_commit_id(&commit) {
        return Err(anyhow!(
            "GitHub answered `{git_ref}` in {repo} with something that is not a commit id"
        ));
    }
    let response = get(
        &format!("{base}/repos/{repo}/tarball/{commit}"),
        "application/vnd.github+json",
        token.as_deref(),
        (repo, git_ref),
        debug,
    )?;

    let bytes = read_capped(response, MAX_BYTES, "the archive from GitHub")?;
    let files = extract(&bytes, name, repo, MAX_BYTES)?;
    package(
        name,
        files,
        Meta {
            source: format!("github:{repo}"),
            git_ref: Some(git_ref.to_string()),
            commit: Some(commit),
            installed_at: now(),
        },
    )
}

/// A full SHA-1 or SHA-256 commit id, as `application/vnd.github.sha`
/// answers. Checked because it goes into the next URL and into
/// `.install.json`.
fn is_commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A GET to GitHub's API, with an error that says what `asked` — the
/// repository and the ref — when it fails.
fn get(
    url: &str,
    accept: &str,
    token: Option<&str>,
    asked: (&str, &str),
    debug: bool,
) -> Result<reqwest::blocking::Response> {
    let (repo, git_ref) = asked;
    if debug {
        eprintln!("[debug] GET {url}");
    }
    let mut request = http::client()?.get(url).header("Accept", accept);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = http::send(request)
        .map_err(|e| executor::transport_failure("Could not reach GitHub", e))?;

    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    // A private repository answers 404 to a request without a token, the
    // same as a ref that does not exist, so both are named.
    let message = match (status.as_u16(), token) {
        (404, None) => format!(
            "GitHub found no `{git_ref}` in {repo}. If the repository is private, set \
             GITHUB_TOKEN to a token that can read it."
        ),
        (404, Some(_)) => format!(
            "GitHub found no `{git_ref}` in {repo}, or the token in GH_TOKEN/GITHUB_TOKEN \
             cannot read it."
        ),
        _ => format!("GitHub answered {status} for {url}."),
    };
    let mut remedy = Remedy::default();
    if token.is_none() {
        remedy = remedy.with_fix("export GITHUB_TOKEN=\"$(gh auth token)\"");
    }
    Err(CliError::http(status.as_u16(), &message)
        .with_remedy(remedy)
        .into())
}

fn unsafe_entry(path: &Path) -> anyhow::Error {
    CliError::new(
        "unsafe_archive",
        format!(
            "Refusing to read `{}` out of the archive: it points outside the directory it \
             would be written to.",
            path.display()
        ),
    )
    .into()
}

/// The files of `workflow/<name>/` in a repository tarball.
fn extract(archive: &[u8], name: &str, repo: &str, limit: u64) -> Result<Files> {
    // Unpacked whole first: a small archive can inflate to far more than it
    // is, and the cap has to apply to what it inflates to.
    let unpacked = read_capped(
        flate2::read::GzDecoder::new(archive),
        limit,
        "the archive from GitHub, unpacked,",
    )?;
    let mut tar = tar::Archive::new(unpacked.as_slice());

    let mut files = Files::new();
    let mut available: std::collections::BTreeSet<String> = Default::default();
    for entry in tar
        .entries()
        .context("The archive from GitHub is not readable as a tarball")?
    {
        let mut entry = entry.context("The archive from GitHub ended unexpectedly")?;
        // Only regular files: a link is a way out of the destination no path
        // check would catch.
        if entry.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let path = entry
            .path()
            .context("The archive holds an entry whose path is not valid UTF-8")?
            .into_owned();
        // GitHub wraps every archive in one directory named after the
        // repository and the commit, so it is stripped by position.
        let mut components = path.components();
        let Some(Component::Normal(_)) = components.next() else {
            return Err(unsafe_entry(&path));
        };
        let inner: PathBuf = components.collect();
        if !is_contained(&inner) {
            return Err(unsafe_entry(&path));
        }
        let Ok(within) = inner.strip_prefix(REPO_PREFIX) else {
            continue;
        };
        let mut parts = within.components();
        let Some(workflow) = parts.next() else {
            continue;
        };
        let relative: PathBuf = parts.collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        if workflow.as_os_str() != name {
            available.insert(workflow.as_os_str().to_string_lossy().into_owned());
            continue;
        }
        let mut bytes = vec![];
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("Could not read {} out of the archive", path.display()))?;
        files.insert(relative, bytes);
    }

    if files.is_empty() {
        let listed = if available.is_empty() {
            format!("{repo} publishes no workflows under `{REPO_PREFIX}/`.")
        } else {
            format!(
                "It publishes: {}.",
                available.into_iter().collect::<Vec<_>>().join(", ")
            )
        };
        return Err(CliError::new(
            "workflow_not_found",
            format!("{repo} has no workflow called `{name}`. {listed}"),
        )
        .into());
    }
    Ok(files)
}

/// Writes `package` into place, replacing an existing install only when
/// `force` says so. Returns where it went.
pub fn install(package: &Package, force: bool) -> Result<PathBuf> {
    let name = &package.workflow.name;
    check_name(name)?;
    let root = root_path()?;
    std::fs::create_dir_all(&root)
        .with_context(|| format!("Could not create {}", root.display()))?;
    let target = root.join(name);
    let previous = previous_path(&root, name);
    recover(&root, name)?;
    if target.exists() && !force {
        return Err(already_installed(name, &target, None));
    }

    let staging = staging_path(&root, name);
    let _ = std::fs::remove_dir_all(&staging);
    let result = (|| -> Result<()> {
        for (relative, bytes) in &package.files {
            let path = staging.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Could not create {}", parent.display()))?;
            }
            std::fs::write(&path, bytes)
                .with_context(|| format!("Could not write {}", path.display()))?;
        }
        std::fs::write(
            staging.join(META_FILE),
            serde_json::to_string_pretty(&package.meta)?,
        )
        .context("Could not record where the workflow came from")?;

        swap_in(&staging, &target, &previous, |from, to| {
            std::fs::rename(from, to)
        })
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result.map(|()| target)
}

fn previous_path(root: &Path, name: &str) -> PathBuf {
    root.join(format!(".{name}.previous"))
}

fn staging_path(root: &Path, name: &str) -> PathBuf {
    root.join(format!(".{name}.staging"))
}

/// Puts back the copy an install moved aside, when it stopped before moving
/// the new one in: that copy is then the workflow. Called wherever an
/// installed workflow is looked for, so a crash in between never reads as
/// "not installed".
pub fn recover_name(name: &str) -> Result<()> {
    check_name(name)?;
    recover(&root_path()?, name)
}

fn recover(root: &Path, name: &str) -> Result<()> {
    let (target, previous) = (root.join(name), previous_path(root, name));
    if !target.exists() && previous.exists() {
        std::fs::rename(&previous, &target)
            .with_context(|| format!("Could not restore {}", target.display()))?;
    }
    Ok(())
}

/// Moves `staged` into `target`. An existing `target` is moved aside to
/// `previous` rather than removed first, and moved back if the new copy
/// cannot go in, so a failed `--force` leaves the old workflow installed.
/// `rename` is a parameter so a test can make the last move fail.
fn swap_in(
    staged: &Path,
    target: &Path,
    previous: &Path,
    rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    let _ = std::fs::remove_dir_all(previous);
    let replacing = target.exists();
    if replacing {
        rename(target, previous)
            .with_context(|| format!("Could not move {} aside", target.display()))?;
    }
    if let Err(e) = rename(staged, target) {
        if replacing {
            rename(previous, target).with_context(|| {
                format!(
                    "Could not move the workflow into {}, nor put the old one back; it is at {}",
                    target.display(),
                    previous.display()
                )
            })?;
        }
        return Err(anyhow::Error::new(e).context(format!(
            "Could not move the workflow into {}",
            target.display()
        )));
    }
    if replacing {
        let _ = std::fs::remove_dir_all(previous);
    }
    Ok(())
}

/// `retry` is the `install` line that would replace it, when the caller
/// knows what was typed.
pub fn already_installed(name: &str, target: &Path, retry: Option<String>) -> anyhow::Error {
    CliError::new(
        "already_installed",
        format!("`{name}` is already installed at {}.", super::tilde(target)),
    )
    .with_remedy(
        Remedy::default()
            .with_fix("Pass --force to replace it.")
            .with_action(retry),
    )
    .into()
}

fn not_installed(name: &str) -> anyhow::Error {
    CliError::new(
        "workflow_not_installed",
        format!("No workflow called `{name}` is installed."),
    )
    .with_remedy(
        Remedy::default()
            .with_action(Some(format!("mapbox workflow install {name}")))
            .with_action(Some("mapbox workflow list".to_string())),
    )
    .into()
}

/// An installed workflow's directory, if there is one. Only a directory
/// holding [`META_FILE`] counts.
pub fn installed_dir(name: &str) -> Result<Option<PathBuf>> {
    check_name(name)?;
    let root = root_path()?;
    // A failed restore leaves the copy where it was; `install` reports it.
    let _ = recover(&root, name);
    let dir = root.join(name);
    Ok(dir.join(META_FILE).is_file().then_some(dir))
}

/// Reads an installed workflow back and checks it again: a file edited by
/// hand since the install is refused here rather than failing mid-run.
pub fn load(name: &str) -> Result<Installed> {
    let root = installed_dir(name)?.ok_or_else(|| not_installed(name))?;
    load_dir(name, root)
}

fn load_dir(name: &str, root: PathBuf) -> Result<Installed> {
    let meta: Meta = serde_json::from_slice(
        &std::fs::read(root.join(META_FILE))
            .with_context(|| format!("Could not read {}", root.join(META_FILE).display()))?,
    )
    .with_context(|| format!("{} is not readable", root.join(META_FILE).display()))?;
    let files = read_tree(&root, &[META_FILE], MAX_BYTES)?;
    let workflow = definition::parse(name, &files).map_err(|p| invalid_workflow(name, &p))?;
    Ok(Installed {
        root,
        workflow,
        meta,
    })
}

/// Every installed workflow, by name, with the ones that no longer load
/// kept as their error.
pub fn list() -> Result<Vec<(String, Result<Installed>)>> {
    let root = root_path()?;
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(vec![]);
    };
    // First, so a workflow an interrupted install left aside is listed.
    let entries: Vec<_> = entries.flatten().map(|entry| entry.file_name()).collect();
    for name in &entries {
        let name = name.to_string_lossy();
        if let Some(moved) = name
            .strip_prefix('.')
            .and_then(|rest| rest.strip_suffix(".previous"))
            .filter(|moved| is_workflow_name(moved))
        {
            let _ = recover(&root, moved);
        }
    }
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(vec![]);
    };
    let mut out = vec![];
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_workflow_name(&name) || !entry.path().join(META_FILE).is_file() {
            continue;
        }
        let loaded = load_dir(&name, entry.path());
        out.push((name, loaded));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Removes an installed workflow. The name is checked and the directory
/// must carry [`META_FILE`], so nothing but a workflow this command put
/// there is ever removed.
pub fn uninstall(name: &str) -> Result<PathBuf> {
    let dir = installed_dir(name)?.ok_or_else(|| not_installed(name))?;
    std::fs::remove_dir_all(&dir).with_context(|| format!("Could not remove {}", dir.display()))?;
    // What an earlier install left behind, which would otherwise come back
    // as this workflow through `recover`.
    let root = root_path()?;
    for leftover in [previous_path(&root, name), staging_path(&root, name)] {
        let _ = std::fs::remove_dir_all(leftover);
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            vec![],
            flate2::Compression::fast(),
        ));
        for (path, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mapbox-store-{name}-{:016x}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn holding(dir: &Path, text: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("workflow.yaml"), text).unwrap();
        dir.to_path_buf()
    }

    fn content(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("workflow.yaml")).unwrap()
    }

    #[test]
    fn a_replacement_goes_in_and_the_old_copy_goes() {
        let root = scratch_dir("swap");
        let (staged, target, previous) = (
            holding(&root.join(".demo.staging"), "new"),
            holding(&root.join("demo"), "old"),
            root.join(".demo.previous"),
        );
        swap_in(&staged, &target, &previous, |a, b| std::fs::rename(a, b)).expect("swapped");
        assert_eq!(content(&target), "new");
        assert!(!previous.exists() && !staged.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    /// The old copy must survive a `--force` whose last step fails.
    #[test]
    fn a_failed_final_move_leaves_the_old_workflow_installed() {
        let root = scratch_dir("swap-fails");
        let (staged, target, previous) = (
            holding(&root.join(".demo.staging"), "new"),
            holding(&root.join("demo"), "old"),
            root.join(".demo.previous"),
        );
        let failing = |from: &Path, to: &Path| {
            if from == staged {
                Err(std::io::Error::other("disk full"))
            } else {
                std::fs::rename(from, to)
            }
        };
        let err = swap_in(&staged, &target, &previous, failing).expect_err("the move fails");
        assert!(format!("{err:#}").contains("disk full"), "{err:#}");
        assert_eq!(content(&target), "old");
        assert!(!previous.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    /// An install that stopped between its two renames left the old copy
    /// aside and none in place; it is the workflow, so it comes back.
    #[test]
    fn a_copy_left_aside_is_put_back() {
        let root = scratch_dir("recover");
        holding(&previous_path(&root, "demo"), "old");
        recover(&root, "demo").expect("recovered");
        assert_eq!(content(&root.join("demo")), "old");
        assert!(!previous_path(&root, "demo").exists());

        // With a copy in place, one aside is a leftover and stays put.
        holding(&previous_path(&root, "demo"), "older");
        recover(&root, "demo").expect("nothing to do");
        assert_eq!(content(&root.join("demo")), "old");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A loopback stand-in for GitHub's API that answers each request in
    /// turn, closing the connection after each so every one is its own, and
    /// returns the request lines it was sent.
    fn serve(
        responses: Vec<(&'static str, Vec<u8>)>,
    ) -> (std::thread::JoinHandle<Vec<String>>, String) {
        use std::io::Write as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut seen = vec![];
            for (content_type, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut head = vec![];
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match std::io::Read::read(&mut stream, &mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                seen.push(head.lines().next().unwrap_or_default().to_string());
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
            seen
        });
        (server, base)
    }

    /// A branch moves, so the install records the commit it resolved to,
    /// and downloads that commit rather than whatever the branch is by then.
    #[test]
    fn a_github_install_records_the_commit_it_downloaded() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let tarball = archive(&[(
            "mapbox-mapbox-cli-0123456/workflow/demo/workflow.yaml",
            b"schema: 1\nname: demo\nsummary: d\nsteps:\n  - id: a\n    command: config list\n",
        )]);
        let (server, base) = serve(vec![
            (
                "application/vnd.github.sha",
                format!("{commit}\n").into_bytes(),
            ),
            ("application/x-gzip", tarball),
        ]);

        let package =
            fetch_github_from(&base, "demo", "mapbox/mapbox-cli", "main", false).expect("fetched");
        assert_eq!(package.meta.git_ref.as_deref(), Some("main"));
        assert_eq!(package.meta.commit.as_deref(), Some(commit));
        assert_eq!(
            server.join().unwrap(),
            [
                "GET /repos/mapbox/mapbox-cli/commits/main HTTP/1.1".to_string(),
                format!("GET /repos/mapbox/mapbox-cli/tarball/{commit} HTTP/1.1"),
            ]
        );
    }

    #[test]
    fn only_a_full_commit_id_is_taken() {
        assert!(is_commit_id(&"a".repeat(40)));
        assert!(is_commit_id(&"0".repeat(64)));
        for bad in ["0123456", "main", "../../x", &"g".repeat(40)] {
            assert!(!is_commit_id(bad), "{bad}");
        }
    }

    /// An install from before the commit was recorded still loads.
    #[test]
    fn an_older_install_record_still_reads() {
        let meta: Meta = serde_json::from_str(
            r#"{"source":"github:mapbox/mapbox-cli","ref":"main","installed_at":1}"#,
        )
        .expect("parses");
        assert_eq!(meta.commit, None);
    }

    #[test]
    fn only_the_named_workflow_comes_out() {
        let bytes = archive(&[
            ("mapbox-cli-abc/README.md", b"x"),
            ("mapbox-cli-abc/workflow/copy-style/workflow.yaml", b"a"),
            ("mapbox-cli-abc/workflow/copy-style/scripts/p.py", b"b"),
            ("mapbox-cli-abc/workflow/other/workflow.yaml", b"c"),
        ]);
        let files = extract(&bytes, "copy-style", "mapbox/cli", MAX_BYTES).unwrap();
        assert_eq!(
            files.keys().cloned().collect::<Vec<_>>(),
            [
                PathBuf::from("scripts/p.py"),
                PathBuf::from("workflow.yaml")
            ]
        );
    }

    #[test]
    fn a_missing_workflow_lists_what_is_there() {
        let bytes = archive(&[("r-abc/workflow/other/workflow.yaml", b"c")]);
        let err = extract(&bytes, "copy-style", "mapbox/cli", MAX_BYTES).unwrap_err();
        assert!(err.to_string().contains("It publishes: other."), "{err}");
    }

    #[test]
    fn a_file_directly_under_workflow_is_not_a_workflow() {
        let bytes = archive(&[("r-abc/workflow/README.md", b"c")]);
        assert!(extract(&bytes, "copy-style", "mapbox/cli", MAX_BYTES).is_err());
    }

    #[test]
    fn repos_and_refs_cannot_carry_url_syntax() {
        assert!(check_repo("mapbox/cli").is_ok());
        for bad in [
            "mapbox",
            "mapbox/cli/x",
            "../x",
            "a/b?c",
            "a/b#c",
            "/cli",
            "a/..",
        ] {
            assert!(check_repo(bad).is_err(), "{bad}");
        }
        for good in ["main", "v0.3.0", "feat/copy-style", "063415e"] {
            assert!(check_ref(good).is_ok(), "{good}");
        }
        for bad in ["", "a?b", "a#b", "../main", "a//b", "a b", "a%2e"] {
            assert!(check_ref(bad).is_err(), "{bad}");
        }
    }

    /// An archive entry written as-is. `tar::Builder` refuses a `..` or an
    /// absolute path, which is exactly what a hostile archive holds.
    fn raw_entry(
        builder: &mut tar::Builder<flate2::write::GzEncoder<Vec<u8>>>,
        path: &str,
        kind: tar::EntryType,
        bytes: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        header.as_gnu_mut().unwrap().name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(kind);
        if kind == tar::EntryType::Symlink {
            header.set_link_name("/etc/passwd").unwrap();
        }
        header.set_cksum();
        builder.append(&header, bytes).unwrap();
    }

    fn raw_archive(entries: &[(&str, tar::EntryType)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            vec![],
            flate2::Compression::fast(),
        ));
        for (path, kind) in entries {
            let bytes: &[u8] = if *kind == tar::EntryType::Regular {
                b"x"
            } else {
                b""
            };
            raw_entry(&mut builder, path, *kind, bytes);
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn code(err: &anyhow::Error) -> &str {
        err.downcast_ref::<CliError>()
            .map_or("", |e| e.code.as_str())
    }

    #[test]
    fn an_entry_that_leaves_its_directory_stops_the_read() {
        use tar::EntryType::Regular;
        for bad in [
            "r-abc/../workflow/copy-style/workflow.yaml",
            "r-abc/workflow/copy-style/../../../x",
            "/workflow/copy-style/workflow.yaml",
        ] {
            let bytes = raw_archive(&[
                ("r-abc/workflow/copy-style/workflow.yaml", Regular),
                (bad, Regular),
            ]);
            let err = extract(&bytes, "copy-style", "mapbox/cli", MAX_BYTES).unwrap_err();
            assert_eq!(code(&err), "unsafe_archive", "{bad}: {err:#}");
        }
    }

    #[test]
    fn links_in_an_archive_are_passed_over() {
        let bytes = raw_archive(&[
            (
                "r-abc/workflow/copy-style/workflow.yaml",
                tar::EntryType::Regular,
            ),
            (
                "r-abc/workflow/copy-style/scripts/x.py",
                tar::EntryType::Symlink,
            ),
            (
                "r-abc/workflow/copy-style/scripts/y.py",
                tar::EntryType::Link,
            ),
        ]);
        let files = extract(&bytes, "copy-style", "mapbox/cli", MAX_BYTES).unwrap();
        assert_eq!(
            files.keys().cloned().collect::<Vec<_>>(),
            [PathBuf::from("workflow.yaml")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_a_local_directory_is_refused() {
        let dir = holding(&scratch_dir("symlink").join("demo"), "x");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.join("scripts/x.py")).unwrap();
        let err = read_tree(&dir, &[], MAX_BYTES).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err:#}");
    }

    /// Past the cap is an error saying so, not a read cut short into
    /// something that then fails as corrupt.
    #[test]
    fn past_the_size_cap_is_refused_as_too_large() {
        assert_eq!(read_capped(&[0u8; 8][..], 8, "x").unwrap().len(), 8);
        let err = read_capped(&[0u8; 9][..], 8, "x").unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err:#}");

        let bytes = archive(&[("r-abc/workflow/copy-style/workflow.yaml", &[b'x'; 4096])]);
        let err = extract(&bytes, "copy-style", "mapbox/cli", 2048).unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err:#}");

        let dir = holding(&scratch_dir("large").join("demo"), &"x".repeat(4096));
        let err = read_tree(&dir, &[], 2048).unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err:#}");
        assert!(read_tree(&dir, &[], MAX_BYTES).is_ok());
    }
}
