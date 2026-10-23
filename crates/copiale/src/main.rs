//! Copiale: a fast, minimalist native desktop client for Signal.

mod cli;
mod instance;
#[cfg(unix)]
mod signals;

use std::process::ExitCode;
use std::sync::Arc;

use cli::{Cli, USAGE};
use copiale_core::config::ConfigFile;
use copiale_core::signal::PresageLinker;
use copiale_core::storage::{OsKeychain, Storage, StoragePaths};
use copiale_ui::Request;
use futures::channel::mpsc::unbounded;
use instance::Claim;

/// Held by tests while they spawn a process: until it execs, the child
/// shares every open file, so a lock dropped meanwhile stays held.
#[cfg(test)]
fn spawn_guard() -> std::sync::MutexGuard<'static, ()> {
    static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SPAWNING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn main() -> ExitCode {
    copiale_core::logging::init();
    copiale_core::hardening::init();

    let cli = match cli::parse(std::env::args_os().skip(1)) {
        Ok(cli) => cli,
        Err(arg) => {
            eprintln!("Unknown argument: {arg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match cli {
        Cli::Help => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Cli::CheckConfig(file) => return check_config(file),
        _ => {}
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
    #[cfg(unix)]
    {
        let requests = requests.clone();
        let quit = move || {
            requests.unbounded_send(Request::Quit).ok();
        };
        if let Err(error) = signals::quit_on_signals(quit) {
            tracing::warn!(%error, "can't handle termination signals");
        }
    }
    if let Some(instance) = instance {
        instance.serve(move |request| {
            requests.unbounded_send(request).ok();
        });
    }
    copiale_ui::run(core, events, received, cli == Cli::Hidden);
    ExitCode::SUCCESS
}

/// Prints the problems in `file`, or in the platform config and its
/// defaults; fails if there are any.
fn check_config(file: Option<std::path::PathBuf>) -> ExitCode {
    let Some(file) = file.map(ConfigFile::new).or_else(ConfigFile::platform) else {
        eprintln!("Can't determine the config directory: no home directory");
        return ExitCode::FAILURE;
    };
    let problems = copiale_ui::settings::problems(&file.load());
    for problem in &problems {
        eprintln!("{problem}");
    }
    if problems.is_empty() {
        println!("No problems found.");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Hands the request to the running instance.
fn forward(dir: &std::path::Path, cli: Cli) -> ExitCode {
    if cli == Cli::Hidden {
        return ExitCode::SUCCESS;
    }
    // Lets the running instance's window take focus on Wayland.
    let activation_token = std::env::var("XDG_ACTIVATION_TOKEN").ok();
    let request = match cli {
        Cli::Start => Request::Show { activation_token },
        Cli::Toggle => Request::Toggle { activation_token },
        Cli::Quit => Request::Quit,
        Cli::Hidden | Cli::Help | Cli::CheckConfig(_) => unreachable!("exits before"),
    };
    match instance::send(dir, &request) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Can't reach the running Copiale: {error}");
            ExitCode::FAILURE
        }
    }
}
