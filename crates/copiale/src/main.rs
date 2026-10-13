//! Copiale: a fast, minimalist native desktop client for Signal.

use std::process::ExitCode;
use std::sync::Arc;

use copiale_core::signal::PresageLinker;
use copiale_core::storage::{OsKeychain, Storage, StoragePaths};

fn main() -> ExitCode {
    copiale_core::logging::init();

    let Some(paths) = StoragePaths::platform() else {
        tracing::error!("can't determine the data directory: no home directory");
        return ExitCode::FAILURE;
    };
    let core = copiale_core::spawn(
        Storage::new(paths),
        Arc::new(OsKeychain::new()),
        PresageLinker::default,
    );
    let (core, events) = match core {
        Ok(core) => core,
        Err(error) => {
            tracing::error!(%error, "can't start the core");
            return ExitCode::FAILURE;
        }
    };

    copiale_ui::run(core, events);
    ExitCode::SUCCESS
}
