//! Subscription policy and attachment/session state at the client boundary.

use amux_ui::{
    Agent, AgentParent, ArtifactKind, Command, Effect, Msg, OpOutcome, StreamCloseReason,
    StreamMsg, StructuredProtocol, Why,
};
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "sdk-runtime";
fn sdk(name: &str, host: &str) -> Agent {
    Agent {
        kind: amux::AgentKind::Claude {
            driver: amux::ClaudeDriver::Sdk,
        },
        ..an_agent(name, host)
    }
}
fn ready() -> Value {
    json!({"type":"amux.claude_sdk.ready","session_id":"s","resumed":false})
}
fn subscriptions() -> Vec<Msg> {
    let mut readonly = sdk("observer", "nova");
    readonly.readonly = true;
    seq([
        vec![
            connected("nova"),
            host_up(&a_host("nova")),
            host_up(&a_host("hetzner")),
            agent_up(&sdk(AGENT, "nova")),
            agent_up(&sdk(AGENT, "nova")),
            agent_up(&sdk("remote", "hetzner")),
            agent_up(&readonly),
        ],
        synced(),
    ])
}
#[test]
fn claude_sdk_subscription_policy_opens_local_once_and_remote_or_readonly_on_request() {
    let (mut model, effects) = fold_with_effects(subscriptions());
    let expected = |name| Effect::OpenStream {
        agent: agent_id(name),
        protocol: StructuredProtocol::ClaudeSdk,
        tail: amux_ui::REPLAY_TAIL,
    };
    assert_eq!(effects, vec![expected(AGENT)]);
    for name in ["remote", "observer"] {
        assert_eq!(
            amux_ui::update(
                &mut model,
                Msg::UserAttached {
                    agent: agent_id(name)
                }
            ),
            vec![expected(name)]
        );
        assert!(
            amux_ui::update(
                &mut model,
                Msg::UserAttached {
                    agent: agent_id(name)
                }
            )
            .is_empty()
        );
    }
    amux_ui::update(
        &mut model,
        stream(
            AGENT,
            StreamMsg::Closed {
                reason: StreamCloseReason::HostUnreachable,
            },
        ),
    );
    assert_eq!(
        amux_ui::update(&mut model, agent_up(&sdk(AGENT, "nova"))),
        vec![expected(AGENT)]
    );
    amux_ui::update(
        &mut model,
        stream(
            AGENT,
            StreamMsg::Closed {
                reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
            },
        ),
    );
    assert!(amux_ui::update(&mut model, agent_up(&sdk(AGENT, "nova"))).is_empty());
}
fn attachments() -> Vec<Msg> {
    let draft = amux_ui::DraftAttachment::from_bytes(
        ArtifactKind::File,
        "facts.txt",
        "text/plain",
        b"facts".to_vec(),
    );
    seq([
        claude_sdk_base(AGENT),
        vec![
            batch(
                AGENT,
                1,
                vec![
                    ready(),
                    json!({"type":"amux.attachments","input_id":"p","refs":[{
                        "id":draft.id,"kind":"file","name":"facts.txt","mime":"text/plain","size":5
                    }]}),
                ],
            ),
            command(
                op(51),
                Command::SendPromptWithAttachments {
                    agent: agent_id(AGENT),
                    text: "Read facts".into(),
                    attachments: vec![draft.clone()],
                },
            ),
            command(
                op(52),
                Command::FetchDiff {
                    agent: agent_id(AGENT),
                    id: draft.id.clone(),
                },
            ),
            Msg::OpResult {
                op: op(52),
                outcome: OpOutcome::DiffFetched {
                    id: draft.id,
                    patch: "a patch".into(),
                },
            },
        ],
    ])
}
#[test]
fn claude_sdk_attachments_keep_pins_metadata_and_fetched_diffs() {
    let (model, effects) = fold_with_effects(attachments());
    let id = amux_artifacts::id_of(b"facts");
    let index = claude_sdk_layer(&model, AGENT).attachments();
    assert_eq!(index.artifact(&id).unwrap().name, "facts.txt");
    assert_eq!(index.diff(&id), Some("a patch"));
    assert!(effects.iter().any(|effect| matches!(effect,
        Effect::PutThenSend { input:amux_ui::InputPayload::ClaudeSdk { payload:amux_ui::claude_sdk::ClaudeSdkInput::Prompt { text } }, pin, puts, .. }
        if text == "Read facts" && pin == &vec![id.clone()] && puts[0].bytes.as_deref() == Some(b"facts"))));
}
fn family() -> Vec<Msg> {
    let mut child = sdk(AGENT, "nova");
    child.parent = Some(AgentParent {
        agent_id: agent_id("lead"),
        host_id: host_id("nova"),
    });
    seq([
        claude_sdk_base(AGENT),
        vec![
            agent_up(&an_agent("lead", "nova")),
            agent_up(&child),
            batch(
                AGENT,
                1,
                vec![
                    ready(),
                    json!({"type":"amux.claude_sdk.permission_required","request_id":"p","tool_name":"Write","input":{},"suggestions":[]}),
                ],
            ),
        ],
    ])
}
#[test]
fn claude_sdk_family_needs_tracks_asks_and_offline_hosts() {
    let mut model = fold(family());
    let needs = model.family_needs(agent_id("lead"));
    assert_eq!(needs.len(), 1);
    assert_eq!(needs[0].card.agent.id, agent_id(AGENT));
    assert_eq!(needs[0].why, Why::Permission);
    let mut host = a_host("nova");
    host.online = false;
    amux_ui::update(&mut model, host_up(&host));
    assert!(model.family_needs(agent_id("lead")).is_empty());
    host.online = true;
    amux_ui::update(&mut model, host_up(&host));
    amux_ui::update(
        &mut model,
        batch(
            AGENT,
            2,
            vec![
                json!({"type":"amux.claude_sdk.permission_resolved","request_id":"p","decision":"allow"}),
            ],
        ),
    );
    assert!(model.family_needs(agent_id("lead")).is_empty());
}
fn facts() -> Vec<Msg> {
    let rows =
        include_str!("../../../amux/tests/fixtures/rows/claude-sdk/introspection.rows.jsonl");
    seq([
        claude_sdk_base(AGENT),
        rows.lines()
            .enumerate()
            .map(|(i, row)| batch(AGENT, i as i64, vec![serde_json::from_str(row).unwrap()]))
            .collect(),
    ])
}
#[test]
fn claude_sdk_session_facts_survive_feed_rows_and_clear_stale_context() {
    let mut model = fold(facts());
    let layer = claude_sdk_layer(&model, AGENT);
    assert!(layer.session().model.is_some());
    assert!(!layer.session().slash_commands.is_empty());
    assert!(layer.context_breakdown().is_some());
    let before = layer.session().clone();
    let slash_commands = before.slash_commands.clone();
    amux_ui::update(
        &mut model,
        batch(
            AGENT,
            99,
            vec![
                json!({"type":"system","subtype":"init","parent_tool_use_id":"child","model":"child-model","permissionMode":"default"}),
            ],
        ),
    );
    assert_eq!(claude_sdk_layer(&model, AGENT).session(), &before);
    amux_ui::update(
        &mut model,
        batch(
            AGENT,
            100,
            vec![
                json!({"type":"amux.claude_sdk.session_facts","model":"new-model","permission_mode":"plan","context":{"used_tokens":23,"window_tokens":200000,"source":"assistant_usage"},"mcp_servers":[{"name":"amux","status":"connected"}]}),
            ],
        ),
    );
    let session = claude_sdk_layer(&model, AGENT).session();
    assert_eq!(session.model.as_deref(), Some("new-model"));
    assert_eq!(session.permission_mode.as_deref(), Some("plan"));
    assert_eq!(session.context.as_ref().unwrap().used_tokens, 23);
    assert_eq!(session.mcp_servers[0].name, "amux");
    assert_eq!(session.slash_commands, slash_commands);
    amux_ui::update(
        &mut model,
        batch(AGENT, 101, vec![json!({"type":"conversation_reset"})]),
    );
    let layer = claude_sdk_layer(&model, AGENT);
    assert!(layer.session().context.is_none());
    assert!(layer.context_breakdown().is_none());
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("claude_sdk_runtime::subscriptions", subscriptions()),
        ("claude_sdk_runtime::attachments", attachments()),
        ("claude_sdk_runtime::family", family()),
        ("claude_sdk_runtime::session", facts()),
    ]
}

#[test]
fn claude_sdk_runtime_sequences_replay_and_capture_public_state() {
    for (name, msgs) in sequences() {
        let mut live = amux_ui::Model::default();
        let mut recorder = amux_ui::Recorder::new(4, &live);
        for msg in msgs.clone() {
            recorder.record(&msg);
            amux_ui::update(&mut live, msg);
            assert!(live.check_invariants().is_empty(), "{name}");
        }
        let snapshot = recorder.snapshot();
        let mut replayed =
            serde_json::from_value(serde_json::to_value(snapshot.checkpoint).unwrap()).unwrap();
        for row in snapshot.msgs {
            amux_ui::update(&mut replayed, serde_json::from_str(&row).unwrap());
        }
        assert_eq!(replayed, live, "{name}");
        if let Some(path) = std::env::var_os("CLAUDE_SDK_RUNTIME_EVIDENCE") {
            let path = std::path::PathBuf::from(path);
            std::fs::create_dir_all(&path).unwrap();
            let (model, effects) = fold_with_effects(msgs);
            let capture = json!({"session":model.claude_sdk(agent_id(AGENT)).map(|l|l.session()),
                "context_breakdown":model.claude_sdk(agent_id(AGENT)).and_then(|l|l.context_breakdown()),
                "attachments":model.claude_sdk(agent_id(AGENT)).map(|l|l.attachments()),
                "effects":effects,"family_needs":model.family_needs(agent_id("lead")).iter().map(|n|json!({"agent":n.card.agent.name,"why":n.why})).collect::<Vec<_>>()});
            std::fs::write(
                path.join(format!("{}.json", name.rsplit("::").next().unwrap())),
                serde_json::to_string_pretty(&capture).unwrap(),
            )
            .unwrap();
        }
    }
}
