use futures::{
    channel::{
        mpsc::{self, Sender},
        oneshot,
    },
    prelude::*,
};
use prost::Message;

pub use crate::{
    configuration::ServiceCredentials,
    proto::{
        web_socket_message, Envelope, WebSocketMessage,
        WebSocketRequestMessage, WebSocketResponseMessage,
    },
};

use crate::{
    push_service::ServiceError,
    websocket::{self, SignalWebSocket},
};

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Incoming {
    Envelope(Envelope),
    QueueEmpty,
}

/// Tells the server an [`Incoming`] item was handled, so it doesn't send
/// it again. Dropping it instead leaves the item to be sent again on the
/// next connection.
#[must_use = "the server sends unacknowledged envelopes again"]
pub struct Acknowledgement {
    responder: oneshot::Sender<WebSocketResponseMessage>,
    response: WebSocketResponseMessage,
}

impl Acknowledgement {
    /// Fails if the websocket closed.
    pub fn acknowledge(self) -> Result<(), ServiceError> {
        self.responder.send(self.response).map_err(|_| {
            ServiceError::WsClosing {
                reason: "could not respond to message pipe request",
            }
        })
    }
}

impl std::fmt::Debug for Acknowledgement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Acknowledgement").finish_non_exhaustive()
    }
}

pub struct MessagePipe {
    ws: SignalWebSocket<websocket::Identified>,
}

impl MessagePipe {
    pub fn from_socket(ws: SignalWebSocket<websocket::Identified>) -> Self {
        MessagePipe { ws }
    }

    /// Return a SignalWebSocket for sending messages and other purposes beyond receiving messages.
    pub fn ws(&self) -> SignalWebSocket<websocket::Identified> {
        self.ws.clone()
    }

    /// Worker task that processes the websocket into Envelopes
    async fn run(
        mut self,
        mut sink: Sender<Result<(Incoming, Acknowledgement), ServiceError>>,
    ) -> Result<(), mpsc::SendError> {
        let mut ws = self.ws.clone();
        let mut stream = ws
            .take_request_stream()
            .expect("web socket request handler not in use");

        while let Some((request, responder)) = stream.next().await {
            // WebsocketConnection::onMessage(ByteString)
            if let Some(env) =
                self.process_request(request, responder).await.transpose()
            {
                sink.send(env).await?;
            } else {
                tracing::trace!("got empty message in websocket");
            }
        }

        ws.return_request_stream(stream);

        Ok(())
    }

    async fn process_request(
        &mut self,
        request: WebSocketRequestMessage,
        responder: oneshot::Sender<WebSocketResponseMessage>,
    ) -> Result<Option<(Incoming, Acknowledgement)>, ServiceError> {
        // Java: MessagePipe::read
        let response = WebSocketResponseMessage::from_request(&request);

        let incoming = if request.is_signal_service_envelope() {
            let body = if let Some(body) = request.body.as_ref() {
                body
            } else {
                return Err(ServiceError::InvalidFrame {
                    reason: "request without body.",
                });
            };
            Some(Incoming::Envelope(Envelope::decode(body.as_slice())?))
        } else if request.is_queue_empty() {
            Some(Incoming::QueueEmpty)
        } else {
            None
        };

        let acknowledgement = Acknowledgement {
            responder,
            response,
        };
        match incoming {
            Some(incoming) => Ok(Some((incoming, acknowledgement))),
            None => acknowledgement.acknowledge().map(|()| None),
        }
    }

    /// Returns the stream of `Envelope`s
    ///
    /// Envelopes yielded are acknowledged.
    pub fn stream(self) -> impl Stream<Item = Result<Incoming, ServiceError>> {
        self.stream_unacknowledged().map(|item| {
            let (incoming, acknowledgement) = item?;
            acknowledgement.acknowledge()?;
            Ok(incoming)
        })
    }

    /// Returns the stream of `Envelope`s, each to acknowledge once handled.
    ///
    /// The server keeps an envelope until it is acknowledged, and sends
    /// it again on the next connection if it isn't.
    pub fn stream_unacknowledged(
        self,
    ) -> impl Stream<Item = Result<(Incoming, Acknowledgement), ServiceError>>
    {
        let (sink, stream) = mpsc::channel(1);

        let stream = stream.map(Some);
        let runner = self.run(sink).map(|e| {
            tracing::info!("sink was closed: {:?}", e);
            None
        });

        let combined = futures::stream::select(stream, runner.into_stream());
        combined.filter_map(|x| async { x })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acknowledgement(
    ) -> (Acknowledgement, oneshot::Receiver<WebSocketResponseMessage>) {
        let (responder, response) = oneshot::channel();
        let request = WebSocketRequestMessage {
            id: Some(7),
            verb: Some("PUT".into()),
            path: Some("/api/v1/queue/empty".into()),
            ..Default::default()
        };
        let acknowledgement = Acknowledgement {
            responder,
            response: WebSocketResponseMessage::from_request(&request),
        };
        (acknowledgement, response)
    }

    #[test]
    fn acknowledging_responds_to_the_request() {
        let (acknowledgement, mut response) = acknowledgement();

        acknowledgement.acknowledge().unwrap();

        let response = response.try_recv().unwrap().unwrap();
        assert_eq!(response.id, Some(7));
        assert_eq!(response.status, Some(200));
    }

    #[test]
    fn dropping_leaves_the_request_unanswered() {
        let (acknowledgement, mut response) = acknowledgement();

        drop(acknowledgement);

        assert!(response.try_recv().is_err());
    }

    #[test]
    fn acknowledging_fails_once_the_websocket_closed() {
        let (acknowledgement, response) = acknowledgement();
        drop(response);

        assert!(acknowledgement.acknowledge().is_err());
    }
}
