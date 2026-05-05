use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use super::*;
use crate::protocol::link::Link;
use crate::protocol::message::{
    FrameBody, Message, ProtocolError, ResponseFrame, RoutedFrameMessage,
};
use crate::protocol::{Route, RoutedCallId, method};
fn route(link: &str) -> Route {
    Route::from_link(Link::new(link).unwrap())
}

fn call_id(n: u128) -> RoutedCallId {
    RoutedCallId::from(Uuid::from_u128(n))
}

fn routed_sink(tx: mpsc::Sender<Message>) -> RpcRoutedSink {
    RpcRoutedSink::new(
        tx,
        route("server"),
        route("client"),
        call_id(42),
        Arc::new(Mutex::new(())),
    )
}

fn routed_payload(message: Message) -> Vec<u8> {
    let Message::Routed(frame) = message else {
        panic!("expected routed message");
    };
    assert_eq!(frame.src, route("server"));
    assert_eq!(frame.dst, route("client"));
    assert_eq!(frame.call_id, call_id(42));
    let RoutedFrameMessage::Payload(payload) = frame.message else {
        panic!("expected routed payload");
    };
    payload
}

fn inbound_resources() -> InboundCallResources {
    let (tx, _rx) = mpsc::channel(1);
    InboundCallResources {
        owner_link: Link::new("owner").unwrap(),
        output: routed_sink(tx),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TestStreamItem {
    Bytes(Vec<u8>),
    Cancel,
}

struct TestStreamCodec;

impl RpcStreamCodec for TestStreamCodec {
    type Item = TestStreamItem;

    fn decode_frame(frame: FrameBody) -> Result<Self::Item, ProtocolError> {
        match frame {
            FrameBody::StreamItem(payload) => Ok(TestStreamItem::Bytes(payload)),
            FrameBody::Cancel => Ok(TestStreamItem::Cancel),
            FrameBody::Request(_) | FrameBody::Response(_) => Err(ProtocolError::InvalidArgument {
                message: "test stream accepts only stream items or cancel frames".to_string(),
            }),
        }
    }
}

struct TestStreamEncoder;

impl RpcStreamEncoder for TestStreamEncoder {
    type Item = TestStreamItem;

    fn encode_item(item: &Self::Item) -> Vec<u8> {
        match item {
            TestStreamItem::Bytes(bytes) => bytes.clone(),
            TestStreamItem::Cancel => b"cancel".to_vec(),
        }
    }
}

#[tokio::test]
async fn rpc_stream_writer_delivers_frame_bodies() {
    let (writer, mut reader) = RpcStreamWriter::channel(2);

    writer
        .send_frame_body(FrameBody::StreamItem(b"hello".to_vec()))
        .await
        .unwrap();
    writer.send_frame_body(FrameBody::Cancel).await.unwrap();

    assert_eq!(
        reader.recv_frame().await,
        Some(FrameBody::StreamItem(b"hello".to_vec()))
    );
    assert_eq!(reader.recv_frame().await, Some(FrameBody::Cancel));
}

#[tokio::test]
async fn typed_rpc_stream_reader_maps_frames_through_codec() {
    let (writer, reader) = RpcStreamWriter::channel(2);
    let mut reader = reader.decode_with::<TestStreamCodec>();

    writer
        .send_frame_body(FrameBody::StreamItem(b"hello".to_vec()))
        .await
        .unwrap();
    writer.send_frame_body(FrameBody::Cancel).await.unwrap();

    assert_eq!(
        reader.recv().await,
        Some(Ok(TestStreamItem::Bytes(b"hello".to_vec())))
    );
    assert_eq!(reader.recv().await, Some(Ok(TestStreamItem::Cancel)));
}

#[tokio::test]
async fn routed_sink_sends_stream_item_when_current() {
    let (tx, mut rx) = mpsc::channel(1);
    let sink = routed_sink(tx);

    let sent = sink
        .send_stream_item_if_current(b"hello".to_vec(), || async { true })
        .await
        .unwrap();

    assert!(sent);
    let payload = routed_payload(rx.recv().await.unwrap());
    assert_eq!(
        crate::protocol::wire::decode_frame_body(&payload).unwrap(),
        FrameBody::StreamItem(b"hello".to_vec())
    );
}

#[tokio::test]
async fn typed_routed_sink_encodes_stream_items() {
    let (tx, mut rx) = mpsc::channel(1);
    let sink = routed_sink(tx).encode_with::<TestStreamEncoder>();

    let sent = sink
        .send_item_if_current(TestStreamItem::Bytes(b"hello".to_vec()), || async { true })
        .await
        .unwrap();

    assert!(sent);
    let payload = routed_payload(rx.recv().await.unwrap());
    assert_eq!(
        crate::protocol::wire::decode_frame_body(&payload).unwrap(),
        FrameBody::StreamItem(b"hello".to_vec())
    );
}

#[tokio::test]
async fn routed_sink_skips_stream_item_when_not_current() {
    let (tx, mut rx) = mpsc::channel(1);
    let sink = routed_sink(tx);

    let sent = sink
        .send_stream_item_if_current(b"hello".to_vec(), || async { false })
        .await
        .unwrap();

    assert!(!sent);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn routed_sink_sends_terminal_response() {
    let (tx, mut rx) = mpsc::channel(1);
    let sink = routed_sink(tx);

    sink.send_empty_response_result(Err(ProtocolError::Cancelled {
        message: "cancelled".to_string(),
    }))
    .await
    .unwrap();

    let payload = routed_payload(rx.recv().await.unwrap());
    assert_eq!(
        crate::protocol::wire::decode_frame_body(&payload).unwrap(),
        FrameBody::Response(ResponseFrame::Error(ProtocolError::Cancelled {
            message: "cancelled".to_string(),
        }))
    );
}

#[tokio::test]
async fn register_routed_bidi_owns_call_state_stream_and_sink() {
    let (tx, mut outbound_rx) = mpsc::channel(1);
    let mut state = RpcState::new();
    let counterparty = route("client");
    let call_id = call_id(42);

    let mut call = state
        .register_routed_bidi(RpcRoutedBidiStart {
            tx,
            owner_link: Link::new("owner").unwrap(),
            reply_src: route("server"),
            reply_dst: counterparty.clone(),
            counterparty_route: counterparty.clone(),
            call_id: call_id.clone(),
            method: method::AGENT_OPEN_SESSION,
            dedup_key: None,
            stream_capacity: 1,
        })
        .unwrap();

    let inbound = state.inbound_for_route(&counterparty, &call_id).unwrap();
    assert_eq!(inbound.method, method::AGENT_OPEN_SESSION);
    assert_eq!(inbound.state, InboundCallState::Active);
    assert_eq!(inbound.generation, call.handle.generation);
    assert!(!call.cancellation.is_cancelled());
    let stream_writer = inbound.stream_writer.clone().unwrap();

    stream_writer
        .send_frame_body(FrameBody::Cancel)
        .await
        .unwrap();
    assert_eq!(call.input.recv_frame().await, Some(FrameBody::Cancel));

    call.output
        .send_empty_response_result(Ok(()))
        .await
        .unwrap();
    let payload = routed_payload(outbound_rx.recv().await.unwrap());
    assert_eq!(
        crate::protocol::wire::decode_frame_body(&payload).unwrap(),
        FrameBody::Response(ResponseFrame::Payload(Vec::new()))
    );

    let closing = state
        .begin_inbound_closing_for_handle_if(&call.handle, |_, _| true)
        .expect("active bidi call should move to closing");
    assert!(call.cancellation.is_cancelled());
    assert!(state.finish_inbound_closing(&closing).is_some());
}

#[tokio::test]
async fn register_routed_unary_owns_call_state_and_terminal_sink() {
    let (tx, mut outbound_rx) = mpsc::channel(1);
    let mut state = RpcState::new();
    let counterparty = route("client");
    let call_id = call_id(42);

    let call = state
        .register_routed_unary(RpcRoutedUnaryStart {
            tx,
            owner_link: Link::new("owner").unwrap(),
            reply_src: route("server"),
            reply_dst: counterparty.clone(),
            counterparty_route: counterparty.clone(),
            call_id: call_id.clone(),
            method: method::AGENT_CREATE,
        })
        .unwrap();

    let inbound = state.inbound_for_route(&counterparty, &call_id).unwrap();
    assert_eq!(inbound.method, method::AGENT_CREATE);
    assert_eq!(inbound.state, InboundCallState::Active);
    assert!(inbound.stream_writer.is_none());
    assert_eq!(inbound.generation, call.handle.generation);
    assert!(!inbound.cancellation.is_cancelled());

    let closing = state
        .begin_inbound_closing_for_handle_if(&call.handle, |_, _| true)
        .expect("active unary call should move to closing");
    assert!(
        state
            .inbound_for_route(&counterparty, &call_id)
            .unwrap()
            .cancellation
            .is_cancelled()
    );
    closing
        .send_response(ResponseFrame::Payload(b"created".to_vec()))
        .await
        .unwrap();
    assert!(state.finish_inbound_closing(&closing).is_some());
    assert!(state.inbound_for_route(&counterparty, &call_id).is_none());

    let payload = routed_payload(outbound_rx.recv().await.unwrap());
    assert_eq!(
        crate::protocol::wire::decode_frame_body(&payload).unwrap(),
        FrameBody::Response(ResponseFrame::Payload(b"created".to_vec()))
    );
}

#[test]
fn register_server_stream_owns_no_input_call_state() {
    let (tx, _rx) = mpsc::channel(1);
    let mut state = RpcState::new();
    let counterparty = route("peer");
    let call_id = call_id(42);

    let stream = state
        .register_server_stream(RpcServerStreamStart {
            tx,
            counterparty_route: counterparty.clone(),
            call_id: call_id.clone(),
            method: method::ROUTING_SUBSCRIBE_EVENTS,
            dedup_key: None,
        })
        .unwrap();

    let inbound = state.inbound_for_route(&counterparty, &call_id).unwrap();
    assert_eq!(inbound.method, method::ROUTING_SUBSCRIBE_EVENTS);
    assert_eq!(inbound.state, InboundCallState::Starting);
    assert_eq!(inbound.generation, stream.handle.generation);
    assert!(inbound.stream_writer.is_none());
    assert!(inbound.resources.is_none());
    assert_eq!(
        state.active_inbound_call_id_for_route_and_method(
            &counterparty,
            method::ROUTING_SUBSCRIBE_EVENTS
        ),
        None
    );
    assert!(matches!(
        state.inbound_frame_target_for_route(&counterparty, &call_id),
        Some(RpcInboundFrameTarget::NotAccepting {
            state: InboundCallState::Starting
        })
    ));

    assert!(state.activate_inbound_for_handle(&stream.handle));
    let inbound = state.inbound_for_route(&counterparty, &call_id).unwrap();
    assert_eq!(inbound.state, InboundCallState::Active);
    assert_eq!(
        state.active_inbound_call_id_for_route_and_method(
            &counterparty,
            method::ROUTING_SUBSCRIBE_EVENTS
        ),
        Some(call_id.clone())
    );
    assert!(matches!(
        state.inbound_frame_target_for_route(&counterparty, &call_id),
        Some(RpcInboundFrameTarget::ActiveNoInput {
            method: method::ROUTING_SUBSCRIBE_EVENTS
        })
    ));
}

#[tokio::test]
async fn inbound_frame_target_identifies_active_stream_calls() {
    let (tx, _outbound_rx) = mpsc::channel(1);
    let mut state = RpcState::new();
    let counterparty = route("client");
    let call_id = call_id(42);

    let mut call = state
        .register_routed_bidi(RpcRoutedBidiStart {
            tx,
            owner_link: Link::new("owner").unwrap(),
            reply_src: route("server"),
            reply_dst: counterparty.clone(),
            counterparty_route: counterparty.clone(),
            call_id: call_id.clone(),
            method: method::AGENT_OPEN_SESSION,
            dedup_key: None,
            stream_capacity: 1,
        })
        .unwrap();

    let Some(RpcInboundFrameTarget::ActiveStream {
        method,
        stream_writer,
    }) = state.inbound_frame_target_for_route(&counterparty, &call_id)
    else {
        panic!("expected active stream target");
    };
    assert_eq!(method, method::AGENT_OPEN_SESSION);

    stream_writer
        .send_frame_body(FrameBody::Cancel)
        .await
        .unwrap();
    assert_eq!(call.input.recv_frame().await, Some(FrameBody::Cancel));
}

#[test]
fn inbound_frame_target_identifies_active_no_input_calls() {
    let (tx, _outbound_rx) = mpsc::channel(1);
    let mut state = RpcState::new();
    let counterparty = route("client");
    let call_id = call_id(42);

    state
        .register_routed_unary(RpcRoutedUnaryStart {
            tx,
            owner_link: Link::new("owner").unwrap(),
            reply_src: route("server"),
            reply_dst: counterparty.clone(),
            counterparty_route: counterparty.clone(),
            call_id: call_id.clone(),
            method: method::AGENT_CREATE,
        })
        .unwrap();

    assert!(matches!(
        state.inbound_frame_target_for_route(&counterparty, &call_id),
        Some(RpcInboundFrameTarget::ActiveNoInput {
            method: method::AGENT_CREATE
        })
    ));
}

#[test]
fn inbound_frame_target_reports_closing_calls_as_not_accepting() {
    let (tx, _outbound_rx) = mpsc::channel(1);
    let mut state = RpcState::new();
    let counterparty = route("client");
    let call_id = call_id(42);
    let call = state
        .register_routed_unary(RpcRoutedUnaryStart {
            tx,
            owner_link: Link::new("owner").unwrap(),
            reply_src: route("server"),
            reply_dst: counterparty.clone(),
            counterparty_route: counterparty.clone(),
            call_id: call_id.clone(),
            method: method::AGENT_CREATE,
        })
        .unwrap();

    state
        .begin_inbound_closing_for_handle_if(&call.handle, |_, _| true)
        .unwrap();

    assert!(matches!(
        state.inbound_frame_target_for_route(&counterparty, &call_id),
        Some(RpcInboundFrameTarget::NotAccepting {
            state: InboundCallState::Closing
        })
    ));
}

#[tokio::test]
async fn register_routed_bidi_rejects_duplicate_dedup_key() {
    let (tx, _outbound_rx) = mpsc::channel(1);
    let mut state = RpcState::new();
    let counterparty = route("client");
    let dedup_key = DedupKey::OpenSession {
        counterparty_route: counterparty.clone(),
        agent_id: Uuid::new_v4(),
    };

    state
        .register_routed_bidi(RpcRoutedBidiStart {
            tx: tx.clone(),
            owner_link: Link::new("owner").unwrap(),
            reply_src: route("server"),
            reply_dst: counterparty.clone(),
            counterparty_route: counterparty.clone(),
            call_id: call_id(42),
            method: method::AGENT_OPEN_SESSION,
            dedup_key: Some(dedup_key.clone()),
            stream_capacity: 1,
        })
        .unwrap();

    let error = state
        .register_routed_bidi(RpcRoutedBidiStart {
            tx,
            owner_link: Link::new("owner").unwrap(),
            reply_src: route("server"),
            reply_dst: counterparty.clone(),
            counterparty_route: counterparty.clone(),
            call_id: call_id(43),
            method: method::AGENT_OPEN_SESSION,
            dedup_key: Some(dedup_key.clone()),
            stream_capacity: 1,
        })
        .unwrap_err();

    assert_eq!(
        error,
        RegisterCallError::DuplicateDedupKey {
            key: dedup_key,
            counterparty_route: counterparty,
            call_id: call_id(42),
        }
    );
    assert_eq!(state.inbound_len(), 1);
    assert_eq!(state.dedup_len(), 1);
}

#[test]
fn begin_inbound_closing_for_route_moves_call_state_and_returns_close_token() {
    let mut state = RpcState::new();
    let counterparty = route("client");
    let call_id = call_id(42);
    let generation = Uuid::new_v4();
    state
        .register_inbound(InboundCall {
            call_id: call_id.clone(),
            counterparty_route: counterparty.clone(),
            method: method::AGENT_OPEN_SESSION,
            generation,
            state: InboundCallState::Active,
            dedup_key: None,
            stream_writer: None,
            resources: Some(inbound_resources()),
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap();

    let closing = state
        .begin_inbound_closing_for_route_if(&counterparty, &call_id, |call, resources| {
            call.method == method::AGENT_OPEN_SESSION
                && call.generation == generation
                && resources.owner_link == Link::new("owner").unwrap()
        })
        .expect("active call should move to closing");

    assert_eq!(closing.handle.counterparty_route, counterparty);
    assert_eq!(closing.handle.call_id, call_id);
    assert_eq!(closing.handle.method, method::AGENT_OPEN_SESSION);
    assert_eq!(closing.handle.generation, generation);
    assert!(matches!(
        state
            .inbound_for_route(&counterparty, &call_id)
            .map(|call| call.state),
        Some(InboundCallState::Closing)
    ));
    assert!(
        state
            .begin_inbound_closing_for_route_if(&counterparty, &call_id, |_, _| true)
            .is_none()
    );
}

#[test]
fn begin_inbound_closing_for_route_respects_predicate() {
    let mut state = RpcState::new();
    let counterparty = route("client");
    let call_id = call_id(42);
    state
        .register_inbound(InboundCall {
            call_id: call_id.clone(),
            counterparty_route: counterparty.clone(),
            method: method::AGENT_OPEN_SESSION,
            generation: Uuid::new_v4(),
            state: InboundCallState::Active,
            dedup_key: None,
            stream_writer: None,
            resources: Some(inbound_resources()),
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap();

    assert!(
        state
            .begin_inbound_closing_for_route_if(&counterparty, &call_id, |_, _| false)
            .is_none()
    );
    assert!(matches!(
        state
            .inbound_for_route(&counterparty, &call_id)
            .map(|call| call.state),
        Some(InboundCallState::Active)
    ));
}

#[test]
fn finish_inbound_closing_for_route_requires_generation_and_clears_dedup() {
    let mut state = RpcState::new();
    let counterparty = route("client");
    let call_id = call_id(42);
    let generation = Uuid::new_v4();
    let dedup_key = DedupKey::OpenSession {
        counterparty_route: counterparty.clone(),
        agent_id: Uuid::new_v4(),
    };
    state
        .register_inbound(InboundCall {
            call_id: call_id.clone(),
            counterparty_route: counterparty.clone(),
            method: method::AGENT_OPEN_SESSION,
            generation,
            state: InboundCallState::Active,
            dedup_key: Some(dedup_key.clone()),
            stream_writer: None,
            resources: Some(inbound_resources()),
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap();

    let closing = state
        .begin_inbound_closing_for_route_if(&counterparty, &call_id, |_, _| true)
        .expect("active call should move to closing");
    let mut wrong_generation = closing.clone();
    wrong_generation.handle.generation = Uuid::new_v4();

    assert!(state.finish_inbound_closing(&wrong_generation).is_none());
    assert_eq!(
        state.dedup_call_key(&dedup_key),
        Some((&counterparty, &call_id))
    );

    let removed = state
        .finish_inbound_closing(&closing)
        .expect("matching closing generation should remove call");

    assert_eq!(removed.call_id, call_id);
    assert!(state.inbound_for_route(&counterparty, &call_id).is_none());
    assert!(state.dedup_call_key(&dedup_key).is_none());
}

#[tokio::test]
async fn typed_rpc_stream_reader_returns_decode_errors() {
    let (writer, reader) = RpcStreamWriter::channel(1);
    let mut reader = reader.decode_with::<TestStreamCodec>();

    writer
        .send_frame_body(FrameBody::Response(ResponseFrame::Payload(Vec::new())))
        .await
        .unwrap();

    assert_eq!(
        reader.recv().await,
        Some(Err(ProtocolError::InvalidArgument {
            message: "test stream accepts only stream items or cancel frames".to_string(),
        }))
    );
}

#[test]
fn inbound_dedup_key_rejects_second_active_call() {
    let mut state = RpcState::new();
    let agent_id = Uuid::new_v4();
    let key = DedupKey::OpenSession {
        counterparty_route: route("client-a"),
        agent_id,
    };
    state
        .register_inbound(InboundCall {
            call_id: call_id(1),
            counterparty_route: route("client-a"),
            method: method::AGENT_OPEN_SESSION,
            generation: Uuid::new_v4(),
            state: InboundCallState::Active,
            dedup_key: Some(key.clone()),
            stream_writer: None,
            resources: None,
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap();

    let err = state
        .register_inbound(InboundCall {
            call_id: call_id(2),
            counterparty_route: route("client-a"),
            method: method::AGENT_OPEN_SESSION,
            generation: Uuid::new_v4(),
            state: InboundCallState::Active,
            dedup_key: Some(key.clone()),
            stream_writer: None,
            resources: None,
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap_err();

    assert_eq!(
        err,
        RegisterCallError::DuplicateDedupKey {
            key,
            counterparty_route: route("client-a"),
            call_id: call_id(1),
        }
    );
    assert_eq!(state.inbound_len(), 1);
    assert_eq!(state.dedup_len(), 1);

    let snapshot = state.debug_snapshot();
    assert_eq!(snapshot.inbound_calls.total, 1);
    assert_eq!(
        snapshot
            .inbound_calls
            .by_method
            .get(method::AGENT_OPEN_SESSION.name),
        Some(&1)
    );
    assert_eq!(
        snapshot.inbound_calls.by_counterparty.get("client-a"),
        Some(&1)
    );
}

#[test]
fn removing_inbound_call_clears_matching_dedup_key() {
    let mut state = RpcState::new();
    let agent_id = Uuid::new_v4();
    let key = DedupKey::OpenSession {
        counterparty_route: route("client-a"),
        agent_id,
    };
    state
        .register_inbound(InboundCall {
            call_id: call_id(1),
            counterparty_route: route("client-a"),
            method: method::AGENT_OPEN_SESSION,
            generation: Uuid::new_v4(),
            state: InboundCallState::Active,
            dedup_key: Some(key.clone()),
            stream_writer: None,
            resources: None,
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap();

    let removed = state
        .remove_inbound_for_route(&route("client-a"), &call_id(1))
        .unwrap();

    assert_eq!(removed.call_id, call_id(1));
    assert!(state.dedup_call_key(&key).is_none());
    assert_eq!(state.inbound_len(), 0);
    assert_eq!(state.dedup_len(), 0);
}

#[test]
fn outbound_calls_are_tracked_separately_from_inbound_calls() {
    let mut state = RpcState::new();
    state
        .register_inbound(InboundCall {
            call_id: call_id(1),
            counterparty_route: route("client-a"),
            method: method::AGENT_LIST,
            generation: Uuid::new_v4(),
            state: InboundCallState::Active,
            dedup_key: None,
            stream_writer: None,
            resources: None,
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap();
    state
        .register_outbound(OutboundCall {
            call_id: call_id(2),
            counterparty_route: route("server-a"),
            method: method::AGENT_LIST,
            state: OutboundCallState::AwaitingResponse,
            resources: None,
        })
        .unwrap();

    assert!(
        state
            .inbound_for_route(&route("client-a"), &call_id(1))
            .is_some()
    );
    assert!(
        state
            .outbound_for_route(&route("server-a"), &call_id(2))
            .is_some()
    );
    assert_eq!(state.inbound_len(), 1);
    assert_eq!(state.outbound_len(), 1);

    assert!(state.set_inbound_state_for_route_if(
        &route("client-a"),
        &call_id(1),
        |_| true,
        InboundCallState::Closing
    ));
    assert!(matches!(
        state
            .inbound_for_route(&route("client-a"), &call_id(1))
            .map(|call| call.state),
        Some(InboundCallState::Closing)
    ));
    assert!(state.set_outbound_state_for_route(
        &route("server-a"),
        &call_id(2),
        OutboundCallState::ActiveStream
    ));
    assert!(matches!(
        state
            .outbound_for_route(&route("server-a"), &call_id(2))
            .map(|call| call.state),
        Some(OutboundCallState::ActiveStream)
    ));
    assert!(
        state
            .remove_outbound_for_route_if(&route("server-a"), &call_id(2), |_| true)
            .is_some()
    );
    assert!(
        state
            .inbound_for_route(&route("client-a"), &call_id(1))
            .is_some()
    );
    assert_eq!(state.inbound_len(), 1);
    assert_eq!(state.outbound_len(), 0);
}

#[test]
fn duplicate_route_call_id_is_rejected_per_call_table() {
    let mut state = RpcState::new();
    let counterparty = route("client-a");
    let call_id = call_id(1);
    state
        .register_inbound(InboundCall {
            call_id: call_id.clone(),
            counterparty_route: counterparty.clone(),
            method: method::AGENT_LIST,
            generation: Uuid::new_v4(),
            state: InboundCallState::Active,
            dedup_key: None,
            stream_writer: None,
            resources: None,
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap();

    let inbound_error = state
        .register_inbound(InboundCall {
            call_id: call_id.clone(),
            counterparty_route: counterparty.clone(),
            method: method::AGENT_OPEN_SESSION,
            generation: Uuid::new_v4(),
            state: InboundCallState::Active,
            dedup_key: None,
            stream_writer: None,
            resources: None,
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap_err();
    assert_eq!(
        inbound_error,
        RegisterCallError::DuplicateCallId {
            counterparty_route: counterparty.clone(),
            call_id: call_id.clone(),
        }
    );

    state
        .register_outbound(OutboundCall {
            call_id: call_id.clone(),
            counterparty_route: counterparty.clone(),
            method: method::AGENT_LIST,
            state: OutboundCallState::AwaitingResponse,
            resources: None,
        })
        .unwrap();
    let outbound_error = state
        .register_outbound(OutboundCall {
            call_id: call_id.clone(),
            counterparty_route: counterparty.clone(),
            method: method::AGENT_OPEN_SESSION,
            state: OutboundCallState::AwaitingResponse,
            resources: None,
        })
        .unwrap_err();

    assert_eq!(
        outbound_error,
        RegisterCallError::DuplicateCallId {
            counterparty_route: counterparty,
            call_id,
        }
    );
    assert_eq!(state.inbound_len(), 1);
    assert_eq!(state.outbound_len(), 1);
}

#[test]
fn outbound_call_handle_guards_state_changes_by_route_call_and_method() {
    let mut state = RpcState::new();
    let handle = state
        .register_outbound_tracked(OutboundCall {
            call_id: call_id(2),
            counterparty_route: route("server-a"),
            method: method::AGENT_OPEN_SESSION,
            state: OutboundCallState::AwaitingResponse,
            resources: None,
        })
        .unwrap();
    let wrong_method = RpcOutboundCallHandle {
        method: method::AGENT_CREATE,
        ..handle.clone()
    };

    assert!(!state.set_outbound_state_for_handle(&wrong_method, OutboundCallState::ActiveStream));
    assert!(matches!(
        state
            .outbound_for_route(&route("server-a"), &call_id(2))
            .map(|call| call.state),
        Some(OutboundCallState::AwaitingResponse)
    ));

    assert!(state.set_outbound_state_for_handle(&handle, OutboundCallState::ActiveStream));
    assert!(matches!(
        state
            .outbound_for_route(&route("server-a"), &call_id(2))
            .map(|call| call.state),
        Some(OutboundCallState::ActiveStream)
    ));
    assert!(matches!(
        state.outbound_state_for_handle(&handle),
        Some(OutboundCallState::ActiveStream)
    ));
    assert!(!state.set_outbound_state_for_handle_if(
        &wrong_method,
        |_| true,
        OutboundCallState::Closing
    ));
    assert!(state.set_outbound_state_for_handle_if(
        &handle,
        |state| state == OutboundCallState::ActiveStream,
        OutboundCallState::Closing
    ));
    assert!(matches!(
        state.outbound_state_for_handle(&handle),
        Some(OutboundCallState::Closing)
    ));
    assert!(state.remove_outbound_for_handle(&wrong_method).is_none());
    assert!(state.remove_outbound_for_handle(&handle).is_some());
    assert!(state.outbound_state_for_handle(&handle).is_none());
    assert_eq!(state.outbound_len(), 0);
}

#[test]
fn same_call_id_is_allowed_for_different_counterparty_routes() {
    let mut state = RpcState::new();
    let call_id = call_id(1);
    state
        .register_inbound(InboundCall {
            call_id: call_id.clone(),
            counterparty_route: route("client-a"),
            method: method::AGENT_OPEN_SESSION,
            generation: Uuid::new_v4(),
            state: InboundCallState::Active,
            dedup_key: Some(DedupKey::OpenSession {
                counterparty_route: route("client-a"),
                agent_id: Uuid::new_v4(),
            }),
            stream_writer: None,
            resources: None,
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap();
    state
        .register_inbound(InboundCall {
            call_id: call_id.clone(),
            counterparty_route: route("client-b"),
            method: method::AGENT_OPEN_SESSION,
            generation: Uuid::new_v4(),
            state: InboundCallState::Active,
            dedup_key: Some(DedupKey::OpenSession {
                counterparty_route: route("client-b"),
                agent_id: Uuid::new_v4(),
            }),
            stream_writer: None,
            resources: None,
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap();

    assert_eq!(state.inbound_len(), 2);
    assert!(
        state
            .remove_inbound_for_route(&route("client-a"), &call_id)
            .is_some()
    );
    assert_eq!(state.inbound_len(), 1);
    assert!(
        state
            .remove_inbound_for_route(&route("client-b"), &call_id)
            .is_some()
    );
    assert_eq!(state.inbound_len(), 0);
}

#[test]
fn debug_snapshot_reports_all_call_states() {
    let mut state = RpcState::new();
    state
        .register_inbound(InboundCall {
            call_id: call_id(1),
            counterparty_route: route("client-a"),
            method: method::AGENT_LIST,
            generation: Uuid::new_v4(),
            state: InboundCallState::Closing,
            dedup_key: None,
            stream_writer: None,
            resources: None,
            cancellation: RpcCallCancellation::new(),
        })
        .unwrap();
    state
        .register_outbound(OutboundCall {
            call_id: call_id(2),
            counterparty_route: route("server-a"),
            method: method::ROUTING_SUBSCRIBE_EVENTS,
            state: OutboundCallState::ActiveStream,
            resources: None,
        })
        .unwrap();
    state
        .register_outbound(OutboundCall {
            call_id: call_id(3),
            counterparty_route: route("server-a"),
            method: method::AGENT_OPEN_SESSION,
            state: OutboundCallState::Closing,
            resources: None,
        })
        .unwrap();

    let snapshot = state.debug_snapshot();

    assert_eq!(snapshot.inbound_calls.by_state.get("starting"), Some(&0));
    assert_eq!(snapshot.inbound_calls.by_state.get("active"), Some(&0));
    assert_eq!(snapshot.inbound_calls.by_state.get("closing"), Some(&1));
    assert_eq!(
        snapshot.outbound_calls.by_state.get("awaiting_response"),
        Some(&0)
    );
    assert_eq!(
        snapshot.outbound_calls.by_state.get("active_stream"),
        Some(&1)
    );
    assert_eq!(snapshot.outbound_calls.by_state.get("closing"), Some(&1));
    assert_eq!(
        snapshot
            .outbound_calls
            .by_method
            .get(method::ROUTING_SUBSCRIBE_EVENTS.name),
        Some(&1)
    );
    assert_eq!(
        snapshot.outbound_calls.by_counterparty.get("server-a"),
        Some(&2)
    );
}
