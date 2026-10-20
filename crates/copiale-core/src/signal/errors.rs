//! presage errors as [`BackendError`]s.

use presage::libsignal_service::StorageServiceError;
use presage::libsignal_service::protocol::SignalProtocolError;
use presage::libsignal_service::push_service::ServiceError;
use presage::libsignal_service::sender::MessageSenderError;

use crate::backend::BackendError;

pub(super) fn store_error(error: impl std::error::Error) -> BackendError {
    BackendError::new(error.to_string())
}

/// Marks rejected credentials, connection failures, rate limits and sends
/// refused to a changed verified identity.
pub(super) fn presage_error<S: std::error::Error>(error: presage::Error<S>) -> BackendError {
    let service = match &error {
        presage::Error::ServiceError(error) => Some(error),
        presage::Error::MessageSenderError(error) => match &**error {
            MessageSenderError::ServiceError(error) => Some(error),
            _ => None,
        },
        _ => None,
    };
    if let presage::Error::MessageSenderError(error) = &error
        && matches!(
            **error,
            MessageSenderError::UntrustedIdentity { .. }
                | MessageSenderError::ProtocolError(SignalProtocolError::UntrustedIdentity(_))
        )
    {
        return BackendError::untrusted_identity();
    }
    let local = matches!(
        error,
        presage::Error::IoError(_) | presage::Error::Timeout(_)
    );
    if let Some(ServiceError::RateLimitExceeded { retry_after }) = service {
        let retry_after = retry_after.and_then(|wait| wait.to_std().ok());
        BackendError::rate_limited(error.to_string(), retry_after)
    } else if service.is_some_and(is_unauthorized) {
        BackendError::unauthorized(error.to_string())
    } else if local || service.is_some_and(is_network) {
        BackendError::network(error.to_string())
    } else {
        BackendError::new(error.to_string())
    }
}

/// Marks connection failures.
pub(super) fn storage_service_error(error: StorageServiceError) -> BackendError {
    match &error {
        StorageServiceError::Service(service) if is_network(service) => {
            BackendError::network(error.to_string())
        }
        _ => BackendError::new(error.to_string()),
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

    #[test]
    fn rate_limits_say_how_long_to_wait() {
        type Error = presage::Error<std::io::Error>;
        let limited = |retry_after| {
            presage_error(Error::from(MessageSenderError::ServiceError(
                ServiceError::RateLimitExceeded { retry_after },
            )))
        };

        let said = limited(Some(chrono::Duration::seconds(600)));
        assert!(said.is_rate_limited());
        assert_eq!(
            said.retry_after(),
            Some(std::time::Duration::from_secs(600))
        );
        let unsaid = limited(None);
        assert!(unsaid.is_rate_limited());
        assert_eq!(unsaid.retry_after(), None);
        assert!(!presage_error(Error::ServiceError(ServiceError::Unauthorized)).is_rate_limited());
    }

    #[test]
    fn refused_identities_are_untrusted_identity_errors() {
        use presage::libsignal_service::protocol::{Aci, DeviceId, ProtocolAddress};

        type Error = presage::Error<std::io::Error>;
        let aci = Aci::from(uuid::Uuid::from_u128(7));
        let address =
            ProtocolAddress::new(aci.service_id_string(), DeviceId::try_from(1u32).unwrap());
        let bundle = MessageSenderError::UntrustedIdentity {
            address: aci.into(),
        };
        let session =
            MessageSenderError::ProtocolError(SignalProtocolError::UntrustedIdentity(address));

        for error in [bundle, session] {
            assert_eq!(
                presage_error(Error::from(error)),
                BackendError::untrusted_identity()
            );
        }
        assert!(
            !presage_error(Error::from(MessageSenderError::NoMessagesToSend))
                .is_untrusted_identity()
        );
    }
}
