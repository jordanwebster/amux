//! Per-connection message loop and stream lifecycle.
//!
//! Each connection runs a [`connection_loop`] that receives messages from the
//! reader task and dispatches them via [`handle_message`](super::handlers::handle_message).
//! Reader and writer tasks ([`reader_loop`], [`writer_loop`]) bridge the transport
//! to channels. Subscription management ([`register_subscription`],
//! [`cleanup_subscription`], [`cancel_subscriptions_matching`]) tracks active
//! subscriptions owned by this server.

mod context;
mod driver;
mod heartbeat;
mod reauth;
mod subscription;

pub(super) use context::{ConnectionContext, ConnectionError, HeartbeatRole, Result};
pub(super) use driver::run_connection;
pub(super) use subscription::{
    cancel_subscriptions_matching, cleanup_subscription, extend_subscription,
    register_subscription, unsubscribe_subscription,
};

#[cfg(test)]
mod tests {
    use super::context::{Incoming, MessageMetadata};
    use super::driver::{
        connection_loop, connection_loop_with_heartbeat, reader_loop, writer_loop,
    };
    use super::heartbeat::{
        DialerHeartbeatState, HeartbeatConfig, HeartbeatState, heartbeat_deadlines,
        refresh_has_priority,
    };
    use super::*;
    use crate::protocol::message::{Command, DirectMessage, Message};
    use crate::server::test_helpers::{test_ctx, test_state};
    use crate::server::{LOCAL_USER_ID, ServerState, ServerUserState};
    use crate::transport::TransportError;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;
    use tokio::sync::{RwLock, mpsc};

    // --- Mock MessageReader for reader_loop tests ---

    /// A mock reader that yields a pre-configured sequence of results then EOF.
    struct MockReader {
        results: std::collections::VecDeque<crate::transport::Result<Message>>,
    }

    impl MockReader {
        fn new(results: Vec<crate::transport::Result<Message>>) -> Self {
            Self {
                results: results.into(),
            }
        }
    }

    impl crate::transport::MessageReader for MockReader {
        async fn read_message(&mut self) -> crate::transport::Result<Message> {
            match self.results.pop_front() {
                Some(result) => result,
                None => Err(TransportError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "mock reader exhausted",
                ))),
            }
        }
    }

    struct MockWriter {
        written: Arc<Mutex<Vec<String>>>,
    }

    impl crate::transport::MessageWriter for MockWriter {
        async fn write_message(&mut self, msg: &Message) -> crate::transport::Result<()> {
            self.written
                .lock()
                .unwrap()
                .push(msg.type_label().to_string());
            Ok(())
        }
    }

    /// Drain all messages from an Incoming receiver without blocking.
    async fn drain_incoming(rx: &mut mpsc::Receiver<Incoming>) -> Vec<String> {
        let mut labels = Vec::new();
        while let Ok(item) = rx.try_recv() {
            labels.push(match item {
                Incoming::Msg(_) => "Msg".to_string(),
                Incoming::Wrote(meta) => format!("Wrote(heartbeat={})", meta.is_heartbeat),
                Incoming::Eof => "Eof".to_string(),
                Incoming::TransportErr(e) => format!("TransportErr({e})"),
            });
        }
        labels
    }

    fn test_peer_ctx(
        state: Arc<RwLock<ServerState>>,
        user_state: Arc<RwLock<ServerUserState>>,
    ) -> ConnectionContext {
        let (event_tx, _event_rx) = mpsc::channel(16);
        ConnectionContext {
            state,
            user_state,
            user_id: LOCAL_USER_ID,
            event_tx,
            link_name: "test-peer".to_string(),
            is_local: false,
            heartbeat_role: HeartbeatRole::Dialer,
            next_request_id: Arc::new(AtomicU64::new(1)),
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }

    fn test_acceptor_ctx(
        state: Arc<RwLock<ServerState>>,
        user_state: Arc<RwLock<ServerUserState>>,
    ) -> ConnectionContext {
        let (event_tx, _event_rx) = mpsc::channel(16);
        ConnectionContext {
            state,
            user_state,
            user_id: LOCAL_USER_ID,
            event_tx,
            link_name: "accepted-peer".to_string(),
            is_local: false,
            heartbeat_role: HeartbeatRole::Acceptor,
            next_request_id: Arc::new(AtomicU64::new(1)),
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }

    #[tokio::test]
    async fn reader_loop_forwards_messages_then_eof() {
        let reader = MockReader::new(vec![
            Ok(Message::Command {
                command: Command::ListAgents,
            }),
            Ok(Message::Command {
                command: Command::Debug {
                    verbose: false,
                    format: crate::protocol::message::DebugFormat::Yaml,
                },
            }),
            // MockReader auto-sends EOF when exhausted
        ]);
        let (tx, mut rx) = mpsc::channel(16);

        reader_loop(reader, tx).await;

        let items = drain_incoming(&mut rx).await;
        assert_eq!(items, vec!["Msg", "Msg", "Eof"]);
    }

    #[tokio::test]
    async fn reader_loop_eof_sends_eof_variant() {
        let reader = MockReader::new(vec![Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "connection closed",
        )))]);
        let (tx, mut rx) = mpsc::channel(16);

        reader_loop(reader, tx).await;

        let items = drain_incoming(&mut rx).await;
        assert_eq!(items, vec!["Eof"]);
    }

    #[tokio::test]
    async fn reader_loop_skips_decode_errors_and_continues() {
        // Simulate: good message → undecodable frame → good message → EOF
        let decode_err = rmp_serde::decode::Error::Syntax("unknown variant".to_string());
        let reader = MockReader::new(vec![
            Ok(Message::Command {
                command: Command::ListAgents,
            }),
            Err(TransportError::SerializationDecode(decode_err)),
            Ok(Message::Command {
                command: Command::Debug {
                    verbose: false,
                    format: crate::protocol::message::DebugFormat::Yaml,
                },
            }),
        ]);
        let (tx, mut rx) = mpsc::channel(16);

        reader_loop(reader, tx).await;

        // Undecodable frame should be skipped — two messages plus EOF
        let items = drain_incoming(&mut rx).await;
        assert_eq!(
            items,
            vec!["Msg", "Msg", "Eof"],
            "decode error should be skipped, not forwarded"
        );
    }

    #[tokio::test]
    async fn reader_loop_fatal_io_error_sends_transport_err() {
        let reader = MockReader::new(vec![Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "peer reset",
        )))]);
        let (tx, mut rx) = mpsc::channel(16);

        reader_loop(reader, tx).await;

        let items = drain_incoming(&mut rx).await;
        assert_eq!(items.len(), 1);
        assert!(
            items[0].starts_with("TransportErr("),
            "fatal I/O error should produce TransportErr, got {:?}",
            items[0]
        );
    }

    #[tokio::test]
    async fn writer_loop_reports_successful_writes() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let writer = MockWriter {
            written: written.clone(),
        };
        let (outgoing_tx, outgoing_rx) = mpsc::channel(16);
        let (incoming_tx, mut incoming_rx) = mpsc::channel(16);

        let handle = tokio::spawn(writer_loop(writer, outgoing_rx, incoming_tx));

        outgoing_tx
            .send(Message::Command {
                command: Command::Debug {
                    verbose: false,
                    format: crate::protocol::message::DebugFormat::Yaml,
                },
            })
            .await
            .unwrap();
        outgoing_tx
            .send(Message::Direct {
                message: DirectMessage::Heartbeat,
            })
            .await
            .unwrap();
        drop(outgoing_tx);

        handle.await.unwrap();

        let items = drain_incoming(&mut incoming_rx).await;
        assert_eq!(
            items,
            vec!["Wrote(heartbeat=false)", "Wrote(heartbeat=true)"]
        );
        assert_eq!(
            &*written.lock().unwrap(),
            &vec![
                "Command::Debug".to_string(),
                "Direct::Heartbeat".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn reader_loop_stops_when_receiver_dropped() {
        // If the receiver is dropped, reader_loop should exit on next send
        let reader = MockReader::new(vec![
            Ok(Message::Command {
                command: Command::ListAgents,
            }),
            Ok(Message::Command {
                command: Command::ListAgents,
            }),
            Ok(Message::Command {
                command: Command::ListAgents,
            }),
        ]);
        let (tx, rx) = mpsc::channel(1);
        drop(rx);

        // Should not hang — exits because send fails
        reader_loop(reader, tx).await;
    }

    #[tokio::test]
    async fn connection_loop_eof_returns_ok() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, _response_rx) = mpsc::channel(16);

        incoming_tx.send(Incoming::Eof).await.unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_ok(), "EOF should return Ok, got {:?}", result);
    }

    #[tokio::test]
    async fn connection_loop_channel_close_returns_ok() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, _response_rx) = mpsc::channel(16);

        // Dropping the sender closes the channel — acts like EOF
        drop(incoming_tx);

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(
            result.is_ok(),
            "channel close should return Ok, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn connection_loop_read_error_propagates() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, _response_rx) = mpsc::channel(16);

        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "peer reset");
        incoming_tx
            .send(Incoming::TransportErr(TransportError::Io(io_err)))
            .await
            .unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_err(), "TransportErr should propagate as Err");
        assert!(
            matches!(
                result,
                Err(ConnectionError::Transport(TransportError::Io(_)))
            ),
            "should preserve Io error variant"
        );
    }

    #[tokio::test]
    async fn connection_loop_dispatches_command_and_returns_response() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        // Send a ListAgents command, then EOF to exit the loop
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Command {
                command: Command::ListAgents,
            })))
            .await
            .unwrap();
        incoming_tx.send(Incoming::Eof).await.unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_ok());

        // ListAgents should have produced a ListAgentsResult
        let msg = response_rx.try_recv().expect("should have a response");
        assert!(
            matches!(
                msg,
                Message::Command {
                    command: Command::ListAgentsResult { .. }
                }
            ),
            "expected ListAgentsResult, got {:?}",
            msg
        );
    }

    #[tokio::test]
    async fn connection_loop_skips_unexpected_reauth_result() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        // Send an unexpected ReauthResult (no refresh pending), then a real
        // command, then EOF. The ReauthResult should be skipped and the
        // command should still be dispatched.
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Direct {
                message: DirectMessage::ReauthResult { error: None },
            })))
            .await
            .unwrap();
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Command {
                command: Command::ListAgents,
            })))
            .await
            .unwrap();
        incoming_tx.send(Incoming::Eof).await.unwrap();

        let result = connection_loop(incoming_rx, response_tx, ctx, None).await;
        assert!(result.is_ok());

        // The ReauthResult should be skipped; only ListAgentsResult should appear
        let msg = response_rx.try_recv().expect("should have a response");
        assert!(
            matches!(
                msg,
                Message::Command {
                    command: Command::ListAgentsResult { .. }
                }
            ),
            "expected ListAgentsResult after skipped ReauthResult, got {:?}",
            msg
        );
    }

    #[tokio::test]
    async fn connection_loop_local_connections_do_not_send_heartbeats() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop(incoming_rx, response_tx, ctx, None));

        let recv_result = tokio::time::timeout(Duration::from_millis(40), response_rx.recv()).await;
        assert!(
            recv_result.is_err(),
            "local connections should not emit idle heartbeats"
        );

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "local connection should exit cleanly");
    }

    #[tokio::test]
    async fn connection_loop_sends_heartbeat_after_idle_period() {
        let (state, user_state) = test_state().await;
        let ctx = test_peer_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(20),
                ack_timeout: Duration::from_millis(100),
            }),
        ));

        let msg = tokio::time::timeout(Duration::from_millis(80), response_rx.recv())
            .await
            .expect("heartbeat should be sent before timeout")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "connection should exit cleanly after EOF");
    }

    #[tokio::test]
    async fn connection_loop_dialer_inbound_traffic_does_not_delay_heartbeat() {
        let (state, user_state) = test_state().await;
        let ctx = test_peer_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(25),
                ack_timeout: Duration::from_millis(100),
            }),
        ));

        tokio::time::sleep(Duration::from_millis(10)).await;
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Direct {
                message: DirectMessage::HeartbeatAck,
            })))
            .await
            .unwrap();

        let msg = tokio::time::timeout(Duration::from_millis(40), response_rx.recv())
            .await
            .expect("inbound-only traffic should not suppress a dialer heartbeat")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "connection should exit cleanly after EOF");
    }

    #[tokio::test]
    async fn connection_loop_dialer_outbound_write_resets_heartbeat_timer() {
        let (state, user_state) = test_state().await;
        let ctx = test_peer_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(40),
                ack_timeout: Duration::from_millis(100),
            }),
        ));

        tokio::time::sleep(Duration::from_millis(20)).await;
        incoming_tx
            .send(Incoming::Wrote(MessageMetadata {
                is_heartbeat: false,
            }))
            .await
            .unwrap();

        let recv_result = tokio::time::timeout(Duration::from_millis(15), response_rx.recv()).await;
        assert!(
            recv_result.is_err(),
            "outbound activity should reset the dialer heartbeat timer"
        );

        let msg = tokio::time::timeout(Duration::from_millis(60), response_rx.recv())
            .await
            .expect("heartbeat should fire after the reset idle interval")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(result.is_ok(), "connection should exit cleanly after EOF");
    }

    #[tokio::test]
    async fn connection_loop_times_out_when_dialer_heartbeat_unacked() {
        let (state, user_state) = test_state().await;
        let ctx = test_peer_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(15),
                ack_timeout: Duration::from_millis(15),
            }),
        ));

        let msg = tokio::time::timeout(Duration::from_millis(60), response_rx.recv())
            .await
            .expect("heartbeat should be queued before timeout")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        incoming_tx
            .send(Incoming::Wrote(MessageMetadata { is_heartbeat: true }))
            .await
            .unwrap();

        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(ConnectionError::HeartbeatTimeout)),
            "expected heartbeat timeout, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn connection_loop_times_out_when_heartbeat_write_callback_never_arrives() {
        let (state, user_state) = test_state().await;
        let ctx = test_peer_ctx(state, user_state);
        let (_incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(15),
                ack_timeout: Duration::from_millis(15),
            }),
        ));

        let msg = tokio::time::timeout(Duration::from_millis(60), response_rx.recv())
            .await
            .expect("heartbeat should be queued before timeout")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(ConnectionError::HeartbeatTimeout)),
            "expected heartbeat timeout without a write callback, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn connection_loop_acceptor_times_out_when_peer_heartbeat_is_overdue() {
        let (state, user_state) = test_state().await;
        let ctx = test_acceptor_ctx(state, user_state);
        let (_incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let result = connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(20),
                ack_timeout: Duration::from_millis(20),
            }),
        )
        .await;

        assert!(
            matches!(result, Err(ConnectionError::HeartbeatTimeout)),
            "expected acceptor heartbeat timeout, got {:?}",
            result
        );
        assert!(
            response_rx.try_recv().is_err(),
            "acceptors should not initiate heartbeats"
        );
    }

    #[tokio::test]
    async fn connection_loop_inbound_message_clears_pending_heartbeat_ack() {
        let (state, user_state) = test_state().await;
        let ctx = test_peer_ctx(state, user_state);
        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (response_tx, mut response_rx) = mpsc::channel(16);

        let handle = tokio::spawn(connection_loop_with_heartbeat(
            incoming_rx,
            response_tx,
            ctx,
            None,
            Some(HeartbeatConfig {
                idle_interval: Duration::from_millis(120),
                ack_timeout: Duration::from_millis(60),
            }),
        ));

        let msg = tokio::time::timeout(Duration::from_millis(200), response_rx.recv())
            .await
            .expect("heartbeat should be queued before timeout")
            .expect("response channel should remain open");
        assert!(matches!(
            msg,
            Message::Direct {
                message: DirectMessage::Heartbeat
            }
        ));

        // The ack arrives before the writer reports the heartbeat write. That
        // late write callback must not expose a stale idle deadline or queue a
        // second heartbeat immediately.
        incoming_tx
            .send(Incoming::Msg(Box::new(Message::Direct {
                message: DirectMessage::HeartbeatAck,
            })))
            .await
            .unwrap();
        incoming_tx
            .send(Incoming::Wrote(MessageMetadata { is_heartbeat: true }))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;

        incoming_tx.send(Incoming::Eof).await.unwrap();
        let result = handle.await.unwrap();
        assert!(
            result.is_ok(),
            "inbound activity should clear the pending heartbeat timeout"
        );
        assert!(
            response_rx.try_recv().is_err(),
            "no second heartbeat should be sent before the next idle interval"
        );
    }

    #[tokio::test]
    async fn heartbeat_deadlines_are_suppressed_while_refresh_response_is_pending() {
        let heartbeat = HeartbeatState::Dialer(DialerHeartbeatState {
            config: HeartbeatConfig {
                idle_interval: Duration::from_millis(50),
                ack_timeout: Duration::from_millis(20),
            },
            last_tx_at: tokio::time::Instant::now(),
            probe_deadline: Some(tokio::time::Instant::now() + Duration::from_millis(20)),
        });

        let (heartbeat_deadline, heartbeat_timeout) = heartbeat_deadlines(Some(&heartbeat), true);

        assert!(
            heartbeat_deadline.is_none(),
            "idle heartbeats should be paused while awaiting ReauthResult"
        );
        assert!(
            heartbeat_timeout.is_none(),
            "pending heartbeat acks should not time out while awaiting ReauthResult"
        );
    }

    #[tokio::test]
    async fn refresh_due_now_takes_priority_over_heartbeat_timeouts() {
        let refresh_deadline = Some(tokio::time::Instant::now() - Duration::from_millis(1));

        assert!(
            refresh_has_priority(refresh_deadline, false),
            "a due refresh should preempt heartbeat timeout handling"
        );
    }

    #[tokio::test]
    async fn heartbeat_pause_for_refresh_clears_pending_ack() {
        let mut heartbeat = HeartbeatState::Dialer(DialerHeartbeatState {
            config: HeartbeatConfig {
                idle_interval: Duration::from_millis(50),
                ack_timeout: Duration::from_millis(20),
            },
            last_tx_at: tokio::time::Instant::now(),
            probe_deadline: Some(tokio::time::Instant::now() + Duration::from_millis(20)),
        });
        let previous_last_tx_at = match &heartbeat {
            HeartbeatState::Dialer(state) => state.last_tx_at,
            HeartbeatState::Acceptor(_) => unreachable!(),
        };

        heartbeat.pause_for_refresh();

        assert!(
            !heartbeat.ack_pending(),
            "refresh start should clear any pending heartbeat ack timeout"
        );
        assert!(
            matches!(&heartbeat, HeartbeatState::Dialer(state) if state.last_tx_at == previous_last_tx_at),
            "refresh suppression should not invent outbound activity before the Reauth write succeeds"
        );
    }
}
