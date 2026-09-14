//! `mapbox uninstall` — remove the binary this process is running from.
//!
//! Scope is deliberately narrow: this deletes the `mapbox` executable and
//! nothing else. It does not touch `~/.mapbox` (stored credentials, lock
//! files) or the separately-installed `tilesets` binary that `tilesets-cli`
//! proxies to — those survive a reinstall on purpose, and `auth logout`
//! already exists for the credential store.
//!
//! Unix lets a process unlink the very file it is executing from — the
//! kernel keeps the inode alive under the open fd until the process exits,
//! so `std::fs::remove_file` on `current_exe()` just works. Windows holds an
//! exclusive lock on a running executable's file and refuses to remove it
//! directly, so there [`remove_binary`] spawns a detached helper that waits
//! for this process to exit and deletes the file afterwards — the standard
//! self-deleting-installer trick, and the reason [`run`]'s report differs by
//! platform: on Windows the file is still there when the message is printed.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Command;
use serde_json::json;

use crate::confirm;
use crate::executor;
use crate::output::{self, Mode};

pub const COMMAND: &str = "uninstall";

const DRY_RUN_HELP: &str = "Describe what this would delete, then exit without deleting it";

pub fn command() -> Command {
    Command::new(COMMAND)
        .about("Remove the installed `mapbox` binary")
        .long_about(
            "Remove the `mapbox` binary this process is running from.\n\n\
             Deletes only the executable. Stored credentials, `auth logout`'s file, \
             and the separately-installed `tilesets` binary are left alone — run \
             `mapbox auth logout` first if you also want the credential store gone.",
        )
        .arg(executor::dry_run_arg(DRY_RUN_HELP))
}

/// Where `mapbox` is running from right now.
fn binary_path() -> Result<PathBuf> {
    std::env::current_exe().context("could not determine the path to the running `mapbox` binary")
}

/// Whether [`remove_binary`] finished before it returned, or handed the rest
/// off to a helper that finishes after this process exits.
enum Removal {
    // Only ever produced by the `unix` `remove_binary` below; a Windows
    // build never constructs it, which is fine and not dead code.
    #[cfg_attr(windows, allow(dead_code))]
    Done,
    // Only ever produced by the `windows` `remove_binary` below; macOS and
    // Linux builds never construct it, which is fine and not dead code.
    #[cfg_attr(not(windows), allow(dead_code))]
    Deferred,
}

#[cfg(unix)]
fn remove_binary(path: &Path) -> Result<Removal> {
    std::fs::remove_file(path).with_context(|| format!("could not remove {}", path.display()))?;
    Ok(Removal::Done)
}

#[cfg(windows)]
fn remove_binary(path: &Path) -> Result<Removal> {
    use std::os::windows::process::CommandExt;

    // DETACHED_PROCESS: outlives this process instead of dying with it.
    // CREATE_NO_WINDOW: no console flashes up for a helper nobody is meant
    // to see. `ping -n 2 127.0.0.1` is the traditional stand-in for "sleep
    // ~1s" in cmd — there is no built-in sleep, and timeout.exe refuses to
    // run with no console attached, which this deliberately has none of.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let quoted = path.display();
    std::process::Command::new("cmd")
        .args([
            "/C",
            &format!("ping -n 2 127.0.0.1>nul & del /f /q \"{quoted}\""),
        ])
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .context("could not start the helper that removes the binary after this process exits")?;
    Ok(Removal::Deferred)
}

pub fn describe_plan(mode: Mode) -> Result<()> {
    let shown = binary_path()?.display().to_string();
    output::emit(
        mode,
        &format!("Dry run — nothing was changed.\nWould delete {shown}."),
        json!({ "dry_run": true, "path": shown }),
    )
}

pub fn run(assume_yes: bool, mode: Mode) -> Result<()> {
    let path = binary_path()?;
    let shown = path.display().to_string();

    confirm::destructive_local_action(&format!("About to delete {shown}."), assume_yes)?;

    let removal = remove_binary(&path)?;
    let text = match removal {
        Removal::Done => format!("Removed {shown}."),
        Removal::Deferred => format!(
            "Removing {shown} once this process exits — Windows won't let a running \
             binary delete itself directly."
        ),
    };
    output::emit(
        mode,
        &text,
        json!({ "path": shown, "removed": matches!(removal, Removal::Done) }),
    )
}
