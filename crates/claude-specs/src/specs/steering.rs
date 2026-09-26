//! Specifications for a message sent to headless Claude while a turn is
//! running: which queue priority joins the running turn, which waits for it
//! to end, and which ends it early.

use serde_json::Value;

use super::channels::{lifecycle_states, raw};
use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::driver::sdk::{PermissionMode, UserMessage};
use crate::expect;

const OPENING: &str = "22222222-2222-4222-8222-222222222201";
const FOLDED_IN: &str = "22222222-2222-4222-8222-222222222202";
const LATER: &str = "22222222-2222-4222-8222-222222222203";
const NOW: &str = "22222222-2222-4222-8222-222222222204";

/// Two tool calls give the running turn a boundary after the first, where a
/// message sent during it can join.
fn two_commands(done: &str) -> String {
    format!(
        "Use Bash to run exactly: python3 -c 'import time; time.sleep(15)'; echo S1. Then use \
         Bash to run exactly: python3 -c 'import time; time.sleep(5)'; echo S2. Then reply \
         exactly {done} plus any extra words the user asked for."
    )
}

fn running_turn_setup(done: &str) -> SessionSetup {
    let mut setup = SessionSetup::conversation(HAIKU, two_commands(done));
    setup.prompt_uuid = Some(OPENING.to_owned());
    setup.options.permission_mode = Some(PermissionMode::Default);
    // Pre-allowed rather than answered: with --replay-user-messages Claude
    // echoes a permission answer back on stdout, which this driver rejects.
    setup.options.allowed_tools = vec!["Bash".to_owned()];
    setup
        .options
        .extra_args
        .insert("replay-user-messages".to_owned(), None);
    setup
}

pub(super) static FOLDED: SpecDef = SpecDef {
    name: "probes/steer_folded",
    fixture: "steer_folded",
    setup: || running_turn_setup("DONE-A"),
    run: |session| Box::pin(folded(session)),
};

/// A message sent with Claude's default priority while a tool runs joins the
/// running turn at the next tool boundary: it is reflected and started
/// before that turn's result, and the turn ends with one result for both.
/// A message sent with priority `later` waits for the turn to end and runs
/// as a turn of its own.
async fn folded(session: &mut SpecSession) {
    session.advance_to("system.task_started").await;
    session
        .send(UserMessage::text("Also append the word BANANA.").with_uuid(FOLDED_IN))
        .await
        .expect("the default-priority message is written");
    session
        .send(
            UserMessage::text("Reply with exactly LATER-C.")
                .with_uuid(LATER)
                .with_priority("later"),
        )
        .await
        .expect("the later-priority message is written");

    let running = session.turn().await;
    let mut after = session.turn().await;
    after.messages.extend(session.drain().await.messages);
    let running = raw(&running);
    let after = raw(&after);

    expect!(
        reflected(&running, FOLDED_IN) && !reflected(&running, LATER),
        "before the running turn's result, only the default-priority message is reflected"
    );
    expect!(
        lifecycle_states(&running, FOLDED_IN)
            .starts_with(&["queued".to_owned(), "started".to_owned()]),
        "the default-priority message is queued, then started inside the running turn: {:?}",
        lifecycle_states(&running, FOLDED_IN)
    );
    expect!(
        lifecycle_states(&running, LATER) == ["queued"],
        "the later-priority message stays queued through the running turn: {:?}",
        lifecycle_states(&running, LATER)
    );
    expect!(
        result_text(&running).contains("BANANA"),
        "the running turn's single result answers the message that joined it: {:?}",
        result_text(&running)
    );
    let joined = [
        lifecycle_states(&running, FOLDED_IN),
        lifecycle_states(&after, FOLDED_IN),
    ]
    .concat();
    let opening = [
        lifecycle_states(&running, OPENING),
        lifecycle_states(&after, OPENING),
    ]
    .concat();
    expect!(
        joined == ["queued", "started", "completed"]
            && opening.last().map(String::as_str) == Some("completed"),
        "the joined message completes with the turn it joined: {joined:?} {opening:?}"
    );
    expect!(
        reflected(&after, LATER)
            && lifecycle_states(&after, LATER)
                .ends_with(&["started".to_owned(), "completed".to_owned()])
            && result_text(&after).contains("LATER-C"),
        "the later-priority message then runs as its own turn: {:?}",
        lifecycle_states(&after, LATER)
    );
}

pub(super) static PREEMPTED: SpecDef = SpecDef {
    name: "probes/steer_preempted",
    fixture: "steer_preempted",
    setup: || running_turn_setup("DONE-N"),
    run: |session| Box::pin(preempted(session)),
};

/// A message sent with priority `now` while a tool runs lets that tool
/// finish, then ends the running turn at the boundary instead of joining it:
/// the turn's result says its remaining tools were abandoned, its prompt is
/// cancelled, and the message runs as a new turn.
async fn preempted(session: &mut SpecSession) {
    session.advance_to("system.task_started").await;
    session
        .send(
            UserMessage::text("Also append the word NUTMEG.")
                .with_uuid(NOW)
                .with_priority("now"),
        )
        .await
        .expect("the now-priority message is written");

    let running = session.turn().await;
    let mut after = session.turn().await;
    after.messages.extend(session.drain().await.messages);
    let running = raw(&running);
    let after = raw(&after);

    let finished_tool = running.iter().any(|frame| {
        frame["type"] == "user"
            && frame["tool_use_result"]["interrupted"] == false
            && frame["tool_use_result"]["stdout"] == "S1"
    });
    expect!(
        finished_tool,
        "the running command is not cut short; it finishes and prints S1"
    );
    let result = running
        .iter()
        .rev()
        .find(|frame| frame["type"] == "result")
        .cloned()
        .unwrap_or(Value::Null);
    expect!(
        result["terminal_reason"] == "aborted_tools" && !reflected(&running, NOW),
        "the running turn ends at the boundary, its remaining tools abandoned, before the \
         message is reflected: {:?}",
        result["terminal_reason"]
    );
    let opening = [
        lifecycle_states(&running, OPENING),
        lifecycle_states(&after, OPENING),
    ]
    .concat();
    expect!(
        opening.last().map(String::as_str) == Some("cancelled"),
        "the running turn's prompt is cancelled, not completed: {opening:?}"
    );
    expect!(
        reflected(&after, NOW)
            && lifecycle_states(&after, NOW)
                .ends_with(&["started".to_owned(), "completed".to_owned()])
            && result_text(&after).contains("NUTMEG"),
        "the message then runs as a turn of its own: {:?}",
        lifecycle_states(&after, NOW)
    );
}

fn reflected(frames: &[Value], id: &str) -> bool {
    frames
        .iter()
        .any(|frame| frame["type"] == "user" && frame["isReplay"] == true && frame["uuid"] == id)
}

fn result_text(frames: &[Value]) -> String {
    frames
        .iter()
        .rev()
        .find(|frame| frame["type"] == "result")
        .and_then(|frame| frame["result"].as_str())
        .unwrap_or_default()
        .to_owned()
}
