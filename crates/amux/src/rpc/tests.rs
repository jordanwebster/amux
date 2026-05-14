use uuid::Uuid;

use super::*;
use crate::protocol::{CallId, method};

fn call_id(n: u128) -> CallId {
    CallId::from(Uuid::from_u128(n))
}

fn dedup_key(value: &str) -> DedupKey {
    DedupKey::new("test", value)
}

#[test]
fn register_unary_tracks_active_inbound_call() {
    let mut state = RpcState::new();
    let id = call_id(1);

    let unary = state
        .register_inbound_unary(RpcInboundStart {
            call_id: id.clone(),
            method: method::AGENT_CREATE,
            dedup_key: None,
        })
        .unwrap();

    let call = state.inbound_for_call(&id).unwrap();
    assert_eq!(call.method, method::AGENT_CREATE);
    assert_eq!(call.state, InboundCallState::Active);
    assert_eq!(call.generation, unary.handle.generation);
    assert!(state.inbound_call_is_active_for_handle(&unary.handle));
}

#[test]
fn duplicate_call_id_is_rejected() {
    let mut state = RpcState::new();
    let id = call_id(1);

    state
        .register_inbound_unary(RpcInboundStart {
            call_id: id.clone(),
            method: method::AGENT_CREATE,
            dedup_key: None,
        })
        .unwrap();

    let error = state
        .register_outbound(RpcOutboundStart {
            call_id: id.clone(),
            method: method::AGENT_SEND_INPUT,
            state: OutboundCallState::AwaitingResponse,
        })
        .unwrap_err();

    assert_eq!(error, RegisterCallError::DuplicateCallId { call_id: id });
}

#[test]
fn server_stream_starts_before_it_accepts_followup_frames() {
    let mut state = RpcState::new();
    let id = call_id(1);
    let key = dedup_key("routing-peer");

    let stream = state
        .register_inbound_server_stream(RpcInboundStart {
            call_id: id.clone(),
            method: method::ROUTING_SUBSCRIBE_EVENTS,
            dedup_key: Some(key.clone()),
        })
        .unwrap();

    assert!(matches!(
        state.inbound_call_target_for_call(&id),
        Some(RpcInboundCallTarget::NotAccepting {
            method: method::ROUTING_SUBSCRIBE_EVENTS,
            state: InboundCallState::Starting,
        })
    ));

    assert!(state.activate_inbound_for_handle(&stream.handle));
    assert!(matches!(
        state.inbound_call_target_for_call(&id),
        Some(RpcInboundCallTarget::ActiveNoInput {
            method: method::ROUTING_SUBSCRIBE_EVENTS,
        })
    ));
}

#[test]
fn inbound_dedup_key_rejects_second_live_call() {
    let mut state = RpcState::new();
    let key = dedup_key("same-stream");

    state
        .register_inbound_server_stream(RpcInboundStart {
            call_id: call_id(1),
            method: method::ROUTING_SUBSCRIBE_EVENTS,
            dedup_key: Some(key.clone()),
        })
        .unwrap();

    let error = state
        .register_inbound_server_stream(RpcInboundStart {
            call_id: call_id(2),
            method: method::ROUTING_SUBSCRIBE_EVENTS,
            dedup_key: Some(key.clone()),
        })
        .unwrap_err();

    assert_eq!(
        error,
        RegisterCallError::DuplicateDedupKey {
            key,
            call_id: call_id(1),
        }
    );
}

#[test]
fn closing_inbound_call_cancels_and_finish_removes_it() {
    let mut state = RpcState::new();
    let id = call_id(1);
    let key = dedup_key("stream");

    let stream = state
        .register_inbound_server_stream(RpcInboundStart {
            call_id: id.clone(),
            method: method::AGENT_SUBSCRIBE_EVENTS,
            dedup_key: Some(key.clone()),
        })
        .unwrap();
    assert!(state.activate_inbound_for_handle(&stream.handle));

    let closing = state
        .begin_inbound_closing_for_handle_if(&stream.handle, |_| true)
        .unwrap();

    assert!(stream.cancellation.is_cancelled());
    assert!(matches!(
        state.inbound_call_target_for_call(&id),
        Some(RpcInboundCallTarget::NotAccepting {
            method: method::AGENT_SUBSCRIBE_EVENTS,
            state: InboundCallState::Closing,
        })
    ));
    assert!(state.dedup_call_id(&key).is_some());

    let removed = state.finish_inbound_closing(&closing).unwrap();
    assert_eq!(removed.call_id, id);
    assert!(state.inbound_for_call(&removed.call_id).is_none());
    assert!(state.dedup_call_id(&key).is_none());
}

#[test]
fn finish_inbound_closing_requires_current_generation() {
    let mut state = RpcState::new();
    let id = call_id(1);

    let unary = state
        .register_inbound_unary(RpcInboundStart {
            call_id: id.clone(),
            method: method::AGENT_CREATE,
            dedup_key: None,
        })
        .unwrap();
    let closing = state
        .begin_inbound_closing_for_handle_if(&unary.handle, |_| true)
        .unwrap();
    let mut stale = closing.clone();
    stale.handle.generation = Uuid::new_v4();

    assert!(state.finish_inbound_closing(&stale).is_none());
    assert!(state.inbound_for_call(&id).is_some());
    assert!(state.finish_inbound_closing(&closing).is_some());
    assert!(state.inbound_for_call(&id).is_none());
}

#[test]
fn outbound_calls_track_unary_and_output_stream_lifecycles() {
    let mut state = RpcState::new();
    let unary_id = call_id(1);
    let stream_id = call_id(2);

    let unary = state
        .register_outbound(RpcOutboundStart {
            call_id: unary_id.clone(),
            method: method::AGENT_SEND_INPUT,
            state: OutboundCallState::AwaitingResponse,
        })
        .unwrap();
    let stream = state
        .register_outbound_stream(RpcOutboundStart {
            call_id: stream_id.clone(),
            method: method::AGENT_SUBSCRIBE_SESSION,
            state: OutboundCallState::AwaitingResponse,
        })
        .unwrap();

    assert_eq!(state.outbound_len(), 2);
    assert!(state.set_outbound_state_for_handle_if(
        &stream,
        |state| state == OutboundCallState::AwaitingResponse,
        OutboundCallState::ActiveStream,
    ));
    assert_eq!(
        state.outbound_for_call(&stream_id).unwrap().state,
        OutboundCallState::ActiveStream
    );

    assert!(state.remove_outbound_for_handle(&unary).is_some());
    assert!(state.outbound_for_call(&unary_id).is_none());
    assert!(state.outbound_for_call(&stream_id).is_some());
}

#[test]
fn cancel_all_cancels_inbound_and_clears_indexes() {
    let mut state = RpcState::new();
    let key = dedup_key("cancelled");
    let stream = state
        .register_inbound_server_stream(RpcInboundStart {
            call_id: call_id(1),
            method: method::AGENT_SUBSCRIBE_EVENTS,
            dedup_key: Some(key),
        })
        .unwrap();
    state
        .register_outbound(RpcOutboundStart {
            call_id: call_id(2),
            method: method::AGENT_SEND_INPUT,
            state: OutboundCallState::AwaitingResponse,
        })
        .unwrap();

    state.cancel_all();

    assert!(stream.cancellation.is_cancelled());
    assert_eq!(state.inbound_len(), 0);
    assert_eq!(state.outbound_len(), 0);
    assert_eq!(state.dedup_len(), 0);
}

#[test]
fn debug_snapshot_reports_call_states_and_methods() {
    let mut state = RpcState::new();
    let unary = state
        .register_inbound_unary(RpcInboundStart {
            call_id: call_id(1),
            method: method::AGENT_CREATE,
            dedup_key: None,
        })
        .unwrap();
    state
        .begin_inbound_closing_for_handle_if(&unary.handle, |_| true)
        .unwrap();
    state
        .register_outbound_stream(RpcOutboundStart {
            call_id: call_id(2),
            method: method::AGENT_SUBSCRIBE_SESSION,
            state: OutboundCallState::ActiveStream,
        })
        .unwrap();

    let snapshot = state.debug_snapshot();

    assert_eq!(snapshot.inbound_calls.total, 1);
    assert_eq!(snapshot.inbound_calls.by_state.get("closing"), Some(&1));
    assert_eq!(
        snapshot
            .inbound_calls
            .by_method
            .get(method::AGENT_CREATE.name),
        Some(&1)
    );
    assert_eq!(snapshot.outbound_calls.total, 1);
    assert_eq!(
        snapshot.outbound_calls.by_state.get("active_stream"),
        Some(&1)
    );
    assert_eq!(
        snapshot
            .outbound_calls
            .by_method
            .get(method::AGENT_SUBSCRIBE_SESSION.name),
        Some(&1)
    );
}
