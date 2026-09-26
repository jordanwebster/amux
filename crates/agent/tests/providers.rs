//! Each provider child as the agent hosts it, against the fake providers:
//! launch, handshake, a turn, and a later incarnation continuing the
//! provider's own session.

mod support;

use agent::ExitCause;
use provider_fakes::Step;
use support::*;
use wire::StopMode;

#[tokio::test(flavor = "multi_thread")]
async fn a_codex_agent_handshakes_runs_a_turn_and_resumes_its_thread() {
    let agent = Agent::start(Setup {
        kind: "codex",
        steps: vec![
            Step::Text {
                chunks: vec!["hello".into()],
            },
            Step::TurnEnd,
        ],
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "hi").await, Verdict::Accepted);
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    assert!(agent.log().has_text("hello"));
    let thread = agent.provider_session();
    assert!(!thread.is_empty(), "the thread the server made is kept");

    daemon.stop(StopMode::Graceful).await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);

    agent.resume();
    agent
        .wait("the resumed boundary", |log| {
            log.boundaries()
                .iter()
                .any(|boundary| boundary == "RESUMED")
        })
        .await;
    assert_eq!(agent.provider_session(), thread, "the same thread resumed");
    let mut daemon = agent.dial().await;
    daemon.stop(StopMode::Graceful).await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}
