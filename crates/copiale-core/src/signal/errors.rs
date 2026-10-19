//! presage errors as [`BackendError`]s.

use presage::libsignal_service::push_service::ServiceError;
use presage::libsignal_service::sender::MessageSenderError;

use crate::backend::BackendError;

pub(super) fn store_error(error: impl std::error::Error) -> BackendError {
    BackendError::new(error.to_string())
}

/// Marks rejected credentials and connection failures.
pub(super) fn presage_error<S: std::error::Error>(error: presage::Error<S>) -> BackendError {
    let service = match &error {
        presage::Error::ServiceError(error) => Some(error),
        presage::Error::MessageSenderError(error) => match &**error {
            MessageSenderError::ServiceError(error) => Some(error),
            _ => None,
        },
        _ => None,
    };
    let local = matches!(
        error,
        presage::Error::IoError(_) | presage::Error::Timeout(_)
    );
    if service.is_some_and(is_unauthorized) {
        BackendError::unauthorized(error.to_string())
    } else if local || service.is_some_and(is_network) {
        BackendError::network(error.to_string())
    } else {
        BackendError::new(error.to_string())
    }
}

/// The server rejected our credentials: on a request, or when opening the
/// connection.
fn is_unauthorized(error: &ServiceError) -> bool {
    match error {
        ServiceError::Unauthorized => true,
        ServiceError::WsError(error) => matches!(
            &**error,
            reqwest_websocket::Error::Handshake(
                reqwest_websocket::HandshakeError::UnexpectedStatusCode(status)
            ) if matches!(status.as_u16(), 401 | 403)
        ),
        _ => false,
    }
}

fn is_network(error: &ServiceError) -> bool {
    matches!(
        error,
        ServiceError::Timeout { .. }
            | ServiceError::SendError { .. }
            | ServiceError::IO(_)
            | ServiceError::WsError(_)
            | ServiceError::WsClosing { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_failures_are_network_errors() {
        type Error = presage::Error<std::io::Error>;
        let closing = || ServiceError::WsClosing { reason: "gone" };

        assert!(presage_error(Error::ServiceError(closing())).is_network());
        assert!(
            presage_error(Error::from(MessageSenderError::ServiceError(closing()))).is_network()
        );
        assert!(!presage_error(Error::ServiceError(ServiceError::Unauthorized)).is_network());
        assert!(presage_error(Error::ServiceError(ServiceError::Unauthorized)).is_unauthorized());
        let handshake = |status: u16| {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            let error = reqwest_websocket::HandshakeError::UnexpectedStatusCode(status);
            ServiceError::WsError(Box::new(error.into()))
        };
        assert!(presage_error(Error::ServiceError(handshake(401))).is_unauthorized());
        assert!(presage_error(Error::ServiceError(handshake(403))).is_unauthorized());
        assert!(!presage_error(Error::ServiceError(handshake(500))).is_unauthorized());
        assert!(!presage_error(Error::ServiceError(closing())).is_unauthorized());
        assert!(
            !presage_error(Error::from(MessageSenderError::MaximumRetriesLimitExceeded))
                .is_network()
        );
    }
}
