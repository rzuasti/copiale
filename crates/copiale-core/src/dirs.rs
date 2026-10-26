//! Platform directories for [`crate::APP_ID`].

use std::path::{Path, PathBuf};

use directories::{ProjectDirs, UserDirs};

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

/// Where saved attachments go by default: the user's downloads folder,
/// else their home.
pub fn downloads_dir() -> Option<PathBuf> {
    let dirs = UserDirs::new()?;
    Some(
        dirs.download_dir()
            .unwrap_or_else(|| dirs.home_dir())
            .to_path_buf(),
    )
}

/// Creates `dir` if needed and restricts it to the current user.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
