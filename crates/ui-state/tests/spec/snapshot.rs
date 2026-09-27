//! The thin per-kind snapshot decode and who the header believes.

use prost::Message;
use ui_state::{PhaseView, SessionState, decode_snapshot};
use wire::{Kind, Phase};

use crate::harness::*;

fn strip_facts() -> (
    wire::UsageLimits,
    wire::ToolServerHealth,
    wire::SignIn,
    wire::BackgroundProcesses,
) {
    (
        wire::UsageLimits {
            state: wire::UsageState::NearLimit as i32,
            windows: vec![wire::UsageWindow {
                name: "5h".into(),
                used_percent: 91.0,
                resets_at_ms: Some(9),
            }],
            credits: None,
        },
        wire::ToolServerHealth {
            state: wire::HealthState::Degraded as i32,
            servers: vec![wire::ToolServer {
                name: "github".into(),
                status: wire::ToolServerStatus::Failed as i32,
                error: "401".into(),
            }],
        },
        wire::SignIn {
            state: wire::SignInState::Expired as i32,
            account: "me".into(),
            message: "log in".into(),
        },
        wire::BackgroundProcesses {
            known: true,
            running: 2,
        },
    )
}

#[test]
fn every_kind_decodes_its_snapshot_including_the_four_strip_facts() {
    let (usage, servers, sign_in, background) = strip_facts();
    let tasks = wire::TaskList {
        known: true,
        entries: vec![wire::TaskListEntry {
            id: "1".into(),
            subject: "Update copy".into(),
            status: 2,
            active_form: "Updating".into(),
        }],
    };
    let context = wire::ContextMeter {
        known: true,
        used_tokens: 71_000,
        window_tokens: Some(100_000),
        breakdown: vec![],
    };
    let bodies = [
        (
            Kind::ClaudePty,
            wire::ClaudePtySnapshot {
                tasks: Some(tasks.clone()),
                context: Some(context.clone()),
                model: Some("opus".into()),
                permission_mode: Some("plan".into()),
                usage: Some(usage.clone()),
                servers: Some(servers.clone()),
                sign_in: Some(sign_in.clone()),
                background_processes: Some(background),
                ..Default::default()
            }
            .encode_to_vec(),
        ),
        (
            Kind::ClaudeSdk,
            wire::ClaudeSdkSnapshot {
                tasks: Some(tasks.clone()),
                context: Some(context.clone()),
                model: Some("opus".into()),
                effort: Some("high".into()),
                permission_mode: Some("plan".into()),
                usage: Some(usage.clone()),
                servers: Some(servers.clone()),
                sign_in: Some(sign_in.clone()),
                background_processes: Some(background),
                ..Default::default()
            }
            .encode_to_vec(),
        ),
        (
            Kind::Codex,
            wire::CodexSnapshot {
                plan: Some(tasks.clone()),
                context: Some(context.clone()),
                model: Some("opus".into()),
                effort: Some("high".into()),
                approval_policy: Some("plan".into()),
                sandbox: Some("workspace-write".into()),
                usage: Some(usage.clone()),
                servers: Some(servers.clone()),
                sign_in: Some(sign_in.clone()),
                background_processes: Some(background),
                ..Default::default()
            }
            .encode_to_vec(),
        ),
    ];
    for (kind, body) in bodies {
        let state = decode_snapshot(kind, &body);
        assert_eq!(state.tasks, tasks);
        assert_eq!(state.context, context);
        assert_eq!(state.model.as_deref(), Some("opus"));
        assert_eq!(state.mode.as_deref(), Some("plan"));
        assert_eq!(state.usage, usage);
        assert_eq!(state.servers, servers);
        assert_eq!(state.sign_in, sign_in);
        assert_eq!(state.background, background);
        if kind != Kind::ClaudePty {
            assert_eq!(state.effort.as_deref(), Some("high"));
        }
    }
}

#[test]
fn unknown_is_explicit_and_never_depends_on_which_fact_came_first() {
    for kind in KINDS {
        let state = decode_snapshot(kind, &snapshot_body(kind, &[]));
        assert_eq!(
            state,
            decode_snapshot(kind, &[]),
            "all unknown is the empty body"
        );
        assert_eq!(state.model, None);
        assert_eq!(state.usage.state(), wire::UsageState::Unknown);
        assert!(!state.background.known);
        assert!(!state.context.known);
        let garbage = decode_snapshot(kind, &[0xff, 0xff, 0xff]);
        assert_eq!(
            garbage,
            decode_snapshot(kind, &[]),
            "an unreadable body is nothing known"
        );
    }
}

#[test]
fn the_envelope_carries_phase_queue_and_working_on() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind));
        let mut snap = snapshot(kind, 4, Phase::Working, &[], &[(b"q1", false)]);
        snap.working_on = Some("fixing the build".into());
        apply_checked(&mut state, ev_snapshot(snap));
        let decoded = state.agent_state();
        assert_eq!(decoded.phase, Phase::Working);
        assert_eq!(decoded.working_on.as_deref(), Some("fixing the build"));
        assert_eq!(decoded.queue.len(), 1);
        assert_eq!(decoded.revision, 4);
    }
}

#[test]
fn the_chat_header_reads_the_snapshot_while_the_row_says_otherwise() {
    for kind in KINDS {
        // The inventory row lags one round trip behind the stream.
        let mut state = SessionState::new(with_phase(agent(kind), Phase::Idle));
        assert_eq!(
            state.phase(),
            PhaseView::Idle,
            "before any snapshot the row speaks"
        );
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 3, Phase::Working, &[], &[])),
        );
        assert_eq!(
            state.phase(),
            PhaseView::Working,
            "the snapshot wins on what the agent is doing"
        );
        apply_checked(
            &mut state,
            ui_state::Msg::Entry(with_phase(agent(kind), Phase::Idle)),
        );
        assert_eq!(state.phase(), PhaseView::Working);
        apply_checked(
            &mut state,
            ui_state::Msg::Entry(exited(agent(kind), "stopped")),
        );
        assert_eq!(
            state.phase(),
            PhaseView::Exited {
                cause: Some("stopped".into())
            },
            "the entry wins on whether the process exists"
        );
    }
}

#[test]
fn the_offered_models_and_commands_ride_on_the_session_state() {
    let models = vec![wire::OfferedModel {
        value: "sonnet".into(),
        display_name: "Sonnet".into(),
        description: "Efficient".into(),
        efforts: vec!["low".into(), "high".into()],
        default_effort: None,
        resolved_model: "claude-sonnet-5".into(),
    }];
    let commands = vec![wire::OfferedCommand {
        name: "stripe:test-cards".into(),
        description: "Test cards".into(),
        argument_hint: String::new(),
        source: "stripe".into(),
    }];
    let sdk = wire::ClaudeSdkSnapshot {
        models: models.clone(),
        commands: commands.clone(),
        ..Default::default()
    };
    let state = decode_snapshot(Kind::ClaudeSdk, &sdk.encode_to_vec());
    assert_eq!((&state.models, &state.commands), (&models, &commands));
    let codex = wire::CodexSnapshot {
        models: models.clone(),
        commands: commands.clone(),
        ..Default::default()
    };
    let state = decode_snapshot(Kind::Codex, &codex.encode_to_vec());
    assert_eq!((&state.models, &state.commands), (&models, &commands));
    let empty = decode_snapshot(Kind::Codex, &[]);
    assert!(empty.models.is_empty() && empty.commands.is_empty());
}
