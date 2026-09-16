use model::{AgentPhase, Attention, Summary, SummaryEnvelope, SummaryField, Why};
use ui_state::{Effect, Model, Msg, ServerMsg, StreamEntry, StreamMsg, update};

use crate::harness::{a_host, agent_id, agent_up, an_agent, connected, fold, host_up, t0_plus};

fn summary(attention: Attention) -> Summary {
    Summary {
        attention,
        phase: AgentPhase::Running,
        last_activity: Some(t0_plus(4)),
        todo: None,
        context: None,
        model: None,
        unknown: vec![
            SummaryField::Todo,
            SummaryField::Context,
            SummaryField::Model,
            SummaryField::Outstanding,
        ],
    }
}

fn envelope(through: u64, version: u32, stale: bool, attention: Attention) -> SummaryEnvelope {
    SummaryEnvelope {
        through,
        producer_version: version,
        observed_at: t0_plus(5),
        stale,
        revision: 9,
        summary: summary(attention),
    }
}

#[test]
fn a_host_summary_wins_a_tie_then_the_open_chat_wins_when_ahead() {
    let agent = an_agent("summary", "nova");
    let mut model = fold([
        connected("nova"),
        host_up(&a_host("nova")),
        agent_up(&agent),
    ]);
    update(
        &mut model,
        Msg::Stream {
            agent: agent.id,
            event: StreamMsg::Opened { truncated: false },
        },
    );
    update(
        &mut model,
        Msg::Stream {
            agent: agent.id,
            event: StreamMsg::Batch {
                at: t0_plus(1),
                entries: vec![StreamEntry::observed(
                    1,
                    t0_plus(1),
                    serde_json::json!({"type":"amux.transcript_ready"}),
                )],
            },
        },
    );
    update(
        &mut model,
        Msg::Server(ServerMsg::AgentSummary {
            agent: agent.id,
            envelope: envelope(1, 1, false, Attention::NeedsYou { why: Why::Finished }),
        }),
    );
    let card = model.agent(agent.id).expect("agent card");
    assert_eq!(
        model.fleet_attention(card),
        Attention::NeedsYou { why: Why::Finished },
        "the daemon wins at the same fold position"
    );

    update(
        &mut model,
        Msg::Stream {
            agent: agent.id,
            event: StreamMsg::Batch {
                at: t0_plus(2),
                entries: vec![StreamEntry::observed(
                    2,
                    t0_plus(2),
                    serde_json::json!({
                        "type":"user",
                        "uuid":"11111111-1111-4111-8111-111111111111",
                        "sessionId":"22222222-2222-4222-8222-222222222222",
                        "timestamp":"2026-08-09T00:00:02Z",
                        "message":{"role":"user","content":"continue"},
                        "origin":{"kind":"human"},
                        "promptSource":"typed"
                    }),
                )],
            },
        },
    );
    let card = model.agent(agent.id).expect("agent card");
    assert_ne!(
        model.fleet_attention(card),
        Attention::NeedsYou { why: Why::Finished },
        "a farther-through local fold replaces the host summary as a whole"
    );
}

#[test]
fn stale_and_foreign_host_summaries_keep_their_age_visible() {
    let mut agent = an_agent("summary", "nova");
    agent.summary = Some(envelope(4, 1, true, Attention::Working));
    let model = fold([
        connected("nova"),
        host_up(&a_host("nova")),
        agent_up(&agent),
    ]);
    let card = model.agent(agent.id).expect("agent card");
    assert_eq!(model.status_label_for(card), "stale");
    assert_eq!(model.effective_summary_age(card), t0_plus(4));

    agent.summary = Some(envelope(4, 99, false, Attention::Working));
    let model = fold([
        connected("nova"),
        host_up(&a_host("nova")),
        agent_up(&agent),
    ]);
    let card = model.agent(agent_id("summary")).expect("agent card");
    assert_eq!(model.status_label_for(card), "unknown");
    assert_eq!(model.effective_summary_age(card), t0_plus(5));
}

#[test]
fn fleet_inventory_never_subscribes_until_a_conversation_opens() {
    let agent = an_agent("summary", "nova");
    let mut model = Model::default();
    update(&mut model, connected("nova"));
    update(&mut model, host_up(&a_host("nova")));

    assert!(
        update(&mut model, agent_up(&agent)).is_empty(),
        "fleet standing comes from the daemon summary, not a chat stream"
    );
    assert!(model.stream(agent.id).is_none());

    assert!(matches!(
        update(&mut model, Msg::UserAttached { agent: agent.id }).as_slice(),
        [Effect::OpenStream { agent: opened, .. }] if *opened == agent.id
    ));
    assert!(
        model.stream(agent.id).is_some(),
        "an explicit chat still owns a subscription"
    );
}
