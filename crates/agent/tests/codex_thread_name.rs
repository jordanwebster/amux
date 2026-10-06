//! A Codex thread carries the agent's name: Codex's own app shows the name
//! amux does, and a thread that has not run a turn can only be joined by a
//! second client once it is named.

mod support;

use agent::ExitCause;
use support::*;
use wire::{CodexInput, Input, RenameThread, StopMode, codex_input, input};

/// The names the agent gave the thread, in order, from what the fake read.
fn names(agent: &Agent, thread: &str) -> Vec<String> {
    agent
        .provider_input()
        .iter()
        .filter(|line| line["method"] == "thread/name/set")
        .inspect(|line| assert_eq!(line["params"]["threadId"], thread))
        .map(|line| line["params"]["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_thread_is_named_after_the_agent_at_start_and_on_rename() {
    let agent = Agent::start(Setup {
        kind: "codex",
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    let thread = agent.provider_session();
    agent
        .until("the thread is named", || names(&agent, &thread) == ["test"])
        .await;

    let rename = Input {
        input_id: b"rename-1".to_vec(),
        of: Some(input::Of::Codex(CodexInput {
            of: Some(codex_input::Of::Rename(RenameThread {
                name: "renamed".into(),
            })),
        })),
    };
    assert_eq!(daemon.input(rename).await, Verdict::Accepted);
    agent
        .until("the thread is renamed", || {
            names(&agent, &thread) == ["test", "renamed"]
        })
        .await;

    daemon.stop(StopMode::Graceful).await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
}
