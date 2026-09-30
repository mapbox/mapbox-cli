//! The directory one workflow run keeps its files in.
//!
//! A command step's `save` writes there, and a script finds it through
//! [`ENV`]. It is created under the system's temporary directory with a
//! random name, and removed when the run ends, however it ends — so a run
//! leaves nothing behind, and nothing it removes was ever named by a caller.

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
        // `create_dir`, not `create_dir_all`: a name that already exists is
        // somebody else's directory, and this one is about to be removed.
        std::fs::create_dir(&path)
            .with_context(|| format!("Could not create {}", path.display()))?;
        Ok(Workdir { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
