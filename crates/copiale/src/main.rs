//! Copiale: a fast, minimalist native desktop client for Signal.

mod cli;
mod instance;

use std::process::ExitCode;
use std::sync::Arc;

use cli::{Cli, USAGE};
use copiale_core::signal::PresageLinker;
use copiale_core::storage::{OsKeychain, Storage, StoragePaths};
use copiale_ui::Request;
use futures::channel::mpsc::unbounded;
use instance::Claim;

fn main() -> ExitCode {
    copiale_core::logging::init();

    let cli = match cli::parse(std::env::args_os().skip(1)) {
        Ok(cli) => cli,
        Err(arg) => {
            eprintln!("Unknown argument: {arg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if cli == Cli::Help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let Some(dir) = copiale_core::runtime_dir() else {
        tracing::error!("can't determine the runtime directory: no home directory");
        return ExitCode::FAILURE;
    };
    let instance = match instance::claim(&dir) {
        Ok(Claim::First(instance)) => Some(instance),
        Ok(Claim::Running) => return forward(&dir, cli),
        Err(error) => {
            tracing::warn!(%error, "can't check for a running instance");
            None
        }
    };
    if cli == Cli::Quit {
        eprintln!("Copiale isn't running.");
        return ExitCode::SUCCESS;
    }

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

    let (requests, received) = unbounded();
    if let Some(instance) = instance {
        instance.serve(move |request| {
            requests.unbounded_send(request).ok();
        });
    }
    copiale_ui::run(core, events, received);
    ExitCode::SUCCESS
}

/// Hands the request to the running instance.
fn forward(dir: &std::path::Path, cli: Cli) -> ExitCode {
    // Lets the running instance's window take focus on Wayland.
    let activation_token = std::env::var("XDG_ACTIVATION_TOKEN").ok();
    let request = match cli {
        Cli::Start => Request::Show { activation_token },
        Cli::Toggle => Request::Toggle { activation_token },
        Cli::Quit => Request::Quit,
        Cli::Help => unreachable!("help exits before"),
    };
    match instance::send(dir, &request) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Can't reach the running Copiale: {error}");
            ExitCode::FAILURE
        }
    }
}
