//! Platform directories for [`crate::APP_ID`].

use directories::ProjectDirs;

/// `None` if the home directory can't be determined.
pub(crate) fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("page.codeberg", "korba", "Copiale")
}
