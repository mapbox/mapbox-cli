//! Where installed workflows live, and how they get there.
//!
//! Nothing is bundled into the binary: a workflow runs only once it has been
//! installed, from a local directory or from a GitHub repository laid out as
//! `workflow/<stage>/<name>/`, into `<config dir>/workflows/<name>/`. A copy
//! is taken rather than a link kept, so what runs is what was checked at
//! install time and nothing edited since.
//!
//! The same rules as `agent-skills` hold for the write, for the same
//! reasons: everything is read and checked in memory first, an existing
//! workflow stops the install unless `--force`, only regular files are
//! taken, and a path that would leave the workflow's directory stops the
//! read altogether. The new directory is staged and renamed into place.

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
pub const DEFAULT_REPO: &str = "mapbox/cli";
pub const DEFAULT_REF: &str = "main";

/// The directory inside a repository that holds workflows, by stage.
const REPO_PREFIX: &str = "workflow";

/// The stages a workflow may be published under. `beta` is the only one so
/// far; a `stable/` beside it needs nothing but an entry here.
pub const STAGES: &[&str] = &["beta"];

/// A ceiling on what is read, from a tarball or a local directory. Far above
/// any real workflow, and there so a hostile archive cannot make this
/// allocate without bound.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

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
    #[serde(default)]
    pub stage: Option<String>,
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
fn read_tree(dir: &Path, skip: &[&str]) -> Result<Files> {
    let mut files = Files::new();
    let mut total: u64 = 0;
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
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("Could not read {}", path.display()))?;
                total += bytes.len() as u64;
                if total > MAX_BYTES {
                    return Err(anyhow!(
                        "{} holds more than a workflow should",
                        dir.display()
                    ));
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
    let stage = dir
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|parent| STAGES.contains(&parent.as_str()));
    let files = read_tree(&dir, &[])?;
    package(
        &name,
        files,
        Meta {
            source: dir.display().to_string(),
            git_ref: None,
            stage,
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

    let url = format!("{base}/repos/{repo}/tarball/{git_ref}");
    if debug {
        eprintln!("[debug] GET {url}");
    }
    let token = github_token();
    let mut request = http::client()?
        .get(&url)
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = &token {
        request = request.bearer_auth(token);
    }
    let response = http::send(request)
        .map_err(|e| executor::transport_failure("Could not reach GitHub", e))?;

    let status = response.status();
    if !status.is_success() {
        // A private repository answers 404 to a request without a token, the
        // same as a ref that does not exist, so both are named.
        let message = match (status.as_u16(), &token) {
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
        return Err(CliError::http(status.as_u16(), &message)
            .with_remedy(remedy)
            .into());
    }

    let mut bytes = vec![];
    response
        .take(MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| anyhow!("Could not read the archive from GitHub: {e}"))?;
    let (stage, files) = extract(&bytes, name, repo)?;
    package(
        name,
        files,
        Meta {
            source: format!("github:{repo}"),
            git_ref: Some(git_ref.to_string()),
            stage: Some(stage),
            installed_at: now(),
        },
    )
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

/// The files of `workflow/<stage>/<name>/` in a repository tarball, and the
/// stage it was found under.
fn extract(archive: &[u8], name: &str, repo: &str) -> Result<(String, Files)> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder.take(MAX_BYTES));

    let mut found: std::collections::BTreeMap<(String, String), Files> = Default::default();
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
        let (Some(stage), Some(workflow)) = (parts.next(), parts.next()) else {
            continue;
        };
        let relative: PathBuf = parts.collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        let stage = stage.as_os_str().to_string_lossy().into_owned();
        let workflow = workflow.as_os_str().to_string_lossy().into_owned();
        if !STAGES.contains(&stage.as_str()) {
            continue;
        }
        let key = (stage, workflow);
        if key.1 != name {
            found.entry(key).or_default();
            continue;
        }
        let mut bytes = vec![];
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("Could not read {} out of the archive", path.display()))?;
        found.entry(key).or_default().insert(relative, bytes);
    }

    let mut matching: Vec<(String, Files)> = vec![];
    let mut available: Vec<String> = vec![];
    for ((stage, workflow), files) in found {
        if workflow == name {
            matching.push((stage, files));
        } else {
            available.push(format!("{workflow} ({stage})"));
        }
    }
    match matching.len() {
        1 => Ok(matching.pop().expect("one")),
        0 => {
            let listed = if available.is_empty() {
                format!("{repo} publishes no workflows under `{REPO_PREFIX}/`.")
            } else {
                format!("It publishes: {}.", available.join(", "))
            };
            Err(CliError::new(
                "workflow_not_found",
                format!("{repo} has no workflow called `{name}`. {listed}"),
            )
            .into())
        }
        _ => {
            let stages: Vec<&str> = matching.iter().map(|(stage, _)| stage.as_str()).collect();
            Err(CliError::new(
                "ambiguous_workflow",
                format!(
                    "{repo} publishes `{name}` under more than one stage ({}), which it \
                     should not.",
                    stages.join(", ")
                ),
            )
            .into())
        }
    }
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
    if target.exists() && !force {
        return Err(already_installed(name, &target));
    }

    let staging = root.join(format!(".{name}.staging"));
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

        if target.exists() {
            std::fs::remove_dir_all(&target)
                .with_context(|| format!("Could not replace {}", target.display()))?;
        }
        std::fs::rename(&staging, &target)
            .with_context(|| format!("Could not move the workflow into {}", target.display()))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result.map(|()| target)
}

pub fn already_installed(name: &str, target: &Path) -> anyhow::Error {
    CliError::new(
        "already_installed",
        format!("`{name}` is already installed at {}", target.display()),
    )
    .with_remedy(Remedy::default().with_fix("Pass --force to replace it."))
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
    let dir = root_path()?.join(name);
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
    let files = read_tree(&root, &[META_FILE])?;
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

    #[test]
    fn only_the_named_workflow_comes_out() {
        let bytes = archive(&[
            ("mapbox-cli-abc/README.md", b"x"),
            (
                "mapbox-cli-abc/workflow/beta/copy-style/workflow.yaml",
                b"a",
            ),
            ("mapbox-cli-abc/workflow/beta/copy-style/scripts/p.py", b"b"),
            ("mapbox-cli-abc/workflow/beta/other/workflow.yaml", b"c"),
        ]);
        let (stage, files) = extract(&bytes, "copy-style", "mapbox/cli").unwrap();
        assert_eq!(stage, "beta");
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
        let bytes = archive(&[("r-abc/workflow/beta/other/workflow.yaml", b"c")]);
        let err = extract(&bytes, "copy-style", "mapbox/cli").unwrap_err();
        assert!(err.to_string().contains("other (beta)"), "{err}");
    }

    #[test]
    fn an_unknown_stage_is_not_published() {
        let bytes = archive(&[("r-abc/workflow/experimental/copy-style/workflow.yaml", b"c")]);
        assert!(extract(&bytes, "copy-style", "mapbox/cli").is_err());
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
}
