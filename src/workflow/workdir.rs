//! The directory one workflow run keeps its files in.
//!
//! A command step's `save` writes there, and a script finds it through
//! [`ENV`]. It is created under the system's temporary directory with a
//! random name, readable by its owner only, and removed when the run ends —
//! including by Ctrl-C, which the runner turns into an ordinary failure for
//! this reason. A run that is killed outright still leaves it behind.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// The run's directory, in a script step's environment.
pub const ENV: &str = "MAPBOX_WORKFLOW_WORKDIR";

pub struct Workdir {
    path: PathBuf,
}

impl Workdir {
    pub fn create() -> Result<Self> {
        let name = format!("mapbox-workflow-{:016x}", rand::random::<u64>());
        let path = std::env::temp_dir().join(name);
        // Not `recursive`: a name that already exists is somebody else's
        // directory, and this one is about to be removed.
        builder()
            .create(&path)
            .with_context(|| format!("Could not create {}", path.display()))?;
        Ok(Workdir { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Owner-only on Unix, where `/tmp` is shared and a saved style or a
/// script's output is nobody else's business. Windows' temporary directory
/// is already under the user's own profile.
fn builder() -> std::fs::DirBuilder {
    #[allow(unused_mut)]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn only_its_owner_can_read_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let workdir = Workdir::create().expect("create");
        let mode = std::fs::metadata(workdir.path())
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn it_is_removed_when_dropped() {
        let workdir = Workdir::create().expect("create");
        let path = workdir.path().to_path_buf();
        std::fs::write(path.join("x"), "x").expect("write");
        drop(workdir);
        assert!(!path.exists());
    }
}
