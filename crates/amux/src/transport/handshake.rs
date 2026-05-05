use std::time::Duration;

use thiserror::Error;

use crate::protocol::handshake::{Connect, ConnectResult, PROTOCOL_VERSION};
use crate::protocol::link::Link;
use crate::protocol::message::ProtocolError;
use crate::transport::{Transport, TransportError};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) type Result<T> = std::result::Result<T, HandshakeError>;

#[derive(Debug, Error)]
pub(crate) enum HandshakeError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("handshake timed out")]
    Timeout,
    #[error("invalid handshake message: {0}")]
    InvalidMessage(String),
    #[error("server rejected connection: {0}")]
    Protocol(ProtocolError),
}

/// Outcome of a successful client-initiated handshake.
pub(crate) struct HandshakeOutcome {
    pub(crate) link: Link,
    /// Negotiated idle timeout. `None` means heartbeats are disabled on this
    /// connection.
    pub(crate) idle_timeout_secs: Option<u32>,
}

pub(crate) async fn connect_handshake<T, F>(
    transport: &mut T,
    generate_link: F,
) -> Result<HandshakeOutcome>
where
    T: Transport,
    F: FnOnce() -> Link,
{
    let proposed_link = generate_link();

    let connect = Connect {
        link_name: proposed_link.as_str().to_string(),
        token: None,
        version: PROTOCOL_VERSION,
        supported_versions: vec![PROTOCOL_VERSION],
        client_name: Some("amux-cli".to_string()),
        client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    let payload = connect.encode().map_err(TransportError::from)?;
    transport.write_frame(&payload).await?;

    let payload = tokio::time::timeout(HANDSHAKE_TIMEOUT, transport.read_frame())
        .await
        .map_err(|_| HandshakeError::Timeout)??;
    let response = ConnectResult::decode(&payload).map_err(|e| {
        HandshakeError::InvalidMessage(format!("expected ConnectResult during handshake: {e}"))
    })?;

    match response.error {
        None => {
            let assigned_link = response.assigned_link_name.ok_or_else(|| {
                HandshakeError::InvalidMessage(
                    "accepted ConnectResponse omitted assigned_link_name".to_string(),
                )
            })?;
            let link = Link::new(assigned_link).map_err(|e| {
                HandshakeError::InvalidMessage(format!("server assigned invalid link name: {e}"))
            })?;
            Ok(HandshakeOutcome {
                link,
                idle_timeout_secs: response.idle_timeout_secs,
            })
        }
        Some(error) => Err(HandshakeError::Protocol(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use prost::Message as ProstMessage;

    use super::*;
    use crate::protocol::message::Message;
    use crate::protocol::wire;

    struct FakeTransport {
        reads: VecDeque<crate::transport::Result<Vec<u8>>>,
        writes: Vec<Vec<u8>>,
    }

    impl FakeTransport {
        fn new(reads: Vec<crate::transport::Result<Vec<u8>>>) -> Self {
            Self {
                reads: reads.into(),
                writes: Vec::new(),
            }
        }
    }

    impl Transport for FakeTransport {
        async fn read_frame(&mut self) -> crate::transport::Result<Vec<u8>> {
            self.reads.pop_front().unwrap_or_else(|| {
                Err(TransportError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "fake transport exhausted",
                )))
            })
        }

        async fn write_frame(&mut self, data: &[u8]) -> crate::transport::Result<()> {
            self.writes.push(data.to_vec());
            Ok(())
        }

        async fn read_message(&mut self) -> crate::transport::Result<Message> {
            unreachable!("handshake tests use raw frames")
        }

        async fn write_message(&mut self, _msg: &Message) -> crate::transport::Result<()> {
            unreachable!("handshake tests use raw frames")
        }
    }

    fn accepted_response(link: &str, idle_timeout_secs: Option<u32>) -> Vec<u8> {
        ConnectResult {
            error: None,
            idle_timeout_secs,
            assigned_link_name: Some(link.to_string()),
        }
        .encode()
        .unwrap()
    }

    fn raw_accepted_response(link: &str, idle_timeout_ms: Option<u32>) -> Vec<u8> {
        wire::ConnectResponse {
            outcome: Some(wire::connect_response::Outcome::Accepted(
                wire::ConnectAccepted {
                    protocol_version: PROTOCOL_VERSION,
                    assigned_link_name: link.to_string(),
                    heartbeat: idle_timeout_ms.map(|ms| wire::HeartbeatConfig {
                        role: 1,
                        idle_timeout_ms: ms,
                    }),
                    capabilities: None,
                },
            )),
        }
        .encode_to_vec()
    }

    #[tokio::test]
    async fn connect_handshake_writes_protobuf_request_and_uses_assigned_link() {
        let mut transport =
            FakeTransport::new(vec![Ok(accepted_response("server-link", Some(180)))]);

        let outcome = connect_handshake(&mut transport, || Link::new("client-link").unwrap())
            .await
            .unwrap();

        assert_eq!(outcome.link, Link::new("server-link").unwrap());
        assert_eq!(outcome.idle_timeout_secs, Some(180));
        assert_eq!(transport.writes.len(), 1);

        let request = wire::ConnectRequest::decode(transport.writes[0].as_slice()).unwrap();
        assert_eq!(request.proposed_link_name, "client-link");
        assert_eq!(request.supported_protocol_versions, vec![PROTOCOL_VERSION]);
        let client = request.client.expect("client info should be present");
        assert_eq!(client.name, "amux-cli");
        assert_eq!(client.version, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn connect_handshake_rejects_invalid_assigned_link() {
        let mut transport =
            FakeTransport::new(vec![Ok(raw_accepted_response("bad.link", Some(180_000)))]);

        let result = connect_handshake(&mut transport, || Link::new("client-link").unwrap()).await;

        assert!(matches!(result, Err(HandshakeError::InvalidMessage(_))));
    }
}
