//! The core actor: owns the Signal backend on a dedicated thread.

use std::thread::JoinHandle;

use futures::StreamExt as _;
use futures::stream::LocalBoxStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::LocalSet;

use crate::backend::{BackendError, Incoming, SignalBackend};
use crate::model::{IncomingMessage, MessageBody, ServiceId, Timestamp};

/// Request from the UI to the core.
#[derive(Clone, Debug)]
pub enum Command {
    SendText {
        recipient: ServiceId,
        body: MessageBody,
    },
    /// Stops the core; [`Event::Stopped`] follows.
    Shutdown,
}

/// Notification from the core to the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Queued messages have been delivered; the core is live.
    Synced,
    MessageReceived(IncomingMessage),
    MessageSent {
        recipient: ServiceId,
        timestamp: Timestamp,
    },
    SendFailed {
        recipient: ServiceId,
        timestamp: Timestamp,
        error: BackendError,
    },
    /// The connection failed to open (`Some`) or closed (`None`).
    Disconnected {
        error: Option<BackendError>,
    },
    /// Last event: the core has stopped.
    Stopped,
}

/// The core is no longer running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("core has stopped")]
pub struct CoreStopped;

/// UI-side handle to a running core.
///
/// Dropping it stops the core without waiting for its thread.
pub struct CoreHandle {
    commands: UnboundedSender<Command>,
    thread: JoinHandle<()>,
}

impl CoreHandle {
    pub fn send(&self, command: Command) -> Result<(), CoreStopped> {
        self.commands.send(command).map_err(|_| CoreStopped)
    }

    /// Stops the core and waits for its thread to exit.
    pub fn shutdown(self) -> std::thread::Result<()> {
        self.commands.send(Command::Shutdown).ok();
        self.thread.join()
    }
}

/// Events from the core, in order.
pub struct Events(UnboundedReceiver<Event>);

impl Events {
    /// Next event; `None` once the core has stopped and all events are read.
    pub async fn recv(&mut self) -> Option<Event> {
        self.0.recv().await
    }
}

/// Starts the core on its own thread.
///
/// `backend` runs on that thread, so the backend need not be `Send`.
pub fn spawn<B, F>(backend: F) -> std::io::Result<(CoreHandle, Events)>
where
    F: FnOnce() -> B + Send + 'static,
    B: SignalBackend + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (command_tx, command_rx) = unbounded_channel();
    let (event_tx, event_rx) = unbounded_channel();
    let thread = std::thread::Builder::new()
        .name("copiale-core".into())
        .spawn(move || {
            LocalSet::new().block_on(&runtime, run(backend(), command_rx, event_tx));
        })?;
    let handle = CoreHandle {
        commands: command_tx,
        thread,
    };
    Ok((handle, Events(event_rx)))
}

async fn run<B: SignalBackend>(
    mut backend: B,
    mut commands: UnboundedReceiver<Command>,
    events: UnboundedSender<Event>,
) {
    // A closed receiver means the UI is gone; keep running until told to stop.
    let emit = |event| {
        events.send(event).ok();
    };

    let mut incoming = match backend.receive().await {
        Ok(stream) => Some(stream),
        Err(error) => {
            emit(Event::Disconnected { error: Some(error) });
            None
        }
    };

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::SendText { recipient, body }) => {
                    let timestamp = Timestamp::now();
                    let event = match backend.send_text(recipient, &body, timestamp).await {
                        Ok(()) => Event::MessageSent { recipient, timestamp },
                        Err(error) => Event::SendFailed { recipient, timestamp, error },
                    };
                    emit(event);
                }
                Some(Command::Shutdown) | None => break,
            },
            item = next_incoming(&mut incoming) => match item {
                Some(Incoming::QueueEmpty) => emit(Event::Synced),
                Some(Incoming::Message(message)) => emit(Event::MessageReceived(message)),
                None => {
                    incoming = None;
                    emit(Event::Disconnected { error: None });
                }
            },
        }
    }
    emit(Event::Stopped);
}

/// Next item from the stream; never resolves without one.
async fn next_incoming(stream: &mut Option<LocalBoxStream<'static, Incoming>>) -> Option<Incoming> {
    match stream {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}
