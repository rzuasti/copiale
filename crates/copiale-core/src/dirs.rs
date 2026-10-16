//! Platform directories for [`crate::APP_ID`].

use std::path::PathBuf;

use directories::ProjectDirs;

/// `None` if the home directory can't be determined.
pub(crate) fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("page.codeberg", "korba", "Copiale")
}

/// Where the running instance keeps its lock and socket: the per-user
/// runtime dir on Linux, else the local data dir.
pub fn runtime_dir() -> Option<PathBuf> {
    let dirs = project_dirs()?;
    Some(
        dirs.runtime_dir()
            .unwrap_or_else(|| dirs.data_local_dir())
            .to_path_buf(),
    )
}
