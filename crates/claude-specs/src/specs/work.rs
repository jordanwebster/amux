//! Specifications for the work a turn does that a chat shows as rows: images
//! in and out, a command that fails, the task list, a shell left running in
//! the background, and a tool server that cannot start.

use serde_json::Value;

use super::{HAIKU, SQUARE_PNG, SessionSetup, SpecDef, SpecSession, Turn};
use crate::driver::sdk::{McpServerConfig, McpStdioServerConfig, PermissionMode};
use crate::expect;

pub(super) static IMAGE: SpecDef = SpecDef {
    name: "work/image",
    fixture: "image",
    setup: image_setup,
    run: |session| Box::pin(image(session)),
};

fn image_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Name the color of the attached image in one word. Then read square.png in the \
         working directory with the Read tool and say whether it is the same image.",
    );
    setup.prompt_image = Some(SQUARE_PNG);
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.allow_permissions();
    setup
}

/// An image travels both ways: attached to the person's message as a content
/// block, and back from the Read tool as an image in the tool result.
async fn image(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(turn.succeeded(), "the turn with an image finishes");
    let frames = raw(&turn);
    expect!(
        tool_calls(&turn)
            .iter()
            // Capture replaces the absolute path with a placeholder.
            .any(|(tool, _)| tool == "Read"),
        "Claude reads the image file: {:?}",
        tool_calls(&turn)
    );
    expect!(
        frames.iter().any(|frame| {
            frame["type"] == "user"
                && frame["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|block| {
                        block["type"] == "tool_result"
                            && block["content"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .any(|part| part["type"] == "image")
                    })
        }),
        "the Read result carries the image itself"
    );
    expect!(
        turn.text().to_lowercase().contains("red"),
        "the attached image reached the model: {:?}",
        turn.text()
    );
}

pub(super) static FAILING_COMMAND: SpecDef = SpecDef {
    name: "work/failing_command",
    fixture: "failing_command",
    setup: failing_command_setup,
    run: |session| Box::pin(failing_command(session)),
};

fn failing_command_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Run exactly this Bash command once: ls does-not-exist. Then tell me whether it failed.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.allow_permissions();
    setup
}

/// A command that exits non-zero comes back as an error tool result carrying
/// the exit code and what the command printed.
async fn failing_command(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(turn.succeeded(), "a failed command does not fail the turn");
    let failures = tool_results(&turn)
        .into_iter()
        .filter(|block| block["is_error"] == true)
        .collect::<Vec<_>>();
    expect!(
        failures.len() == 1
            && failures[0].to_string().contains("Exit code")
            && failures[0].to_string().contains("does-not-exist"),
        "the failure is one error result with the exit code and output: {failures:?}"
    );
}

pub(super) static TASK_LIST: SpecDef = SpecDef {
    name: "work/task_list",
    fixture: "task_list",
    setup: task_list_setup,
    run: |session| Box::pin(task_list(session)),
};

fn task_list_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Track this work with your task or todo tool: create three tasks named Draft, Review \
         and Publish. Then mark Draft in progress, then mark Draft completed. Do nothing else, \
         then stop.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.allow_permissions();
    setup
}

/// Headless Claude keeps its task list through a tool, so the list reaches an
/// SDK caller as tool calls and their results rather than as its own event.
async fn task_list(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(turn.succeeded(), "the turn that keeps a task list finishes");
    let tools = turn.tools_used();
    expect!(
        tools
            .iter()
            .any(|tool| ["TaskCreate", "TodoWrite"].contains(tool)),
        "the list is written through a task tool: {tools:?}"
    );
    let inputs = tool_calls(&turn)
        .into_iter()
        .map(|(_, input)| input)
        .collect::<String>();
    expect!(
        ["Draft", "Review", "Publish", "completed"]
            .iter()
            .all(|word| inputs.contains(word)),
        "the three tasks and Draft's completion are in the tool inputs: {inputs}"
    );
}

pub(super) static BACKGROUND_SHELL: SpecDef = SpecDef {
    name: "work/background_shell",
    fixture: "background_shell",
    setup: background_shell_setup,
    run: |session| Box::pin(background_shell(session)),
};

fn background_shell_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Use Bash with run_in_background set to true to run exactly: for i in 1 2 3; do echo \
         tick $i; sleep 2; done. Then wait about eight seconds by running Bash: sleep 8. Then \
         read the background shell's output with the tool for reading background output, and \
         report the last line it printed.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.allow_permissions();
    setup
}

/// A shell started in the background is a task: Claude announces it, the turn
/// goes on, and its output is read back later by id.
async fn background_shell(session: &mut SpecSession) {
    let mut turn = session.turn().await;
    turn.messages.extend(session.drain().await.messages);
    expect!(
        tool_calls(&turn)
            .iter()
            .any(|(tool, input)| tool == "Bash" && input.contains("\"run_in_background\":true")),
        "the command is started in the background: {:?}",
        tool_calls(&turn)
    );
    let frames = raw(&turn);
    expect!(
        frames.iter().any(|frame| {
            frame["type"] == "system"
                && (frame["subtype"] == "task_started"
                    || frame["subtype"] == "background_tasks_changed")
        }),
        "Claude announces the background shell as a task"
    );
    expect!(
        turn.text().contains("tick 3"),
        "the shell's later output is read back: {:?}",
        turn.text()
    );
}

pub(super) static FAILED_TOOL_SERVER: SpecDef = SpecDef {
    name: "control/failed_tool_server",
    fixture: "failed_tool_server",
    setup: failed_tool_server_setup,
    run: |session| Box::pin(failed_tool_server(session)),
};

fn failed_tool_server_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(HAIKU, "Reply with exactly PONG and nothing else.");
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.options.mcp_servers.insert(
        "broken".to_owned(),
        McpServerConfig::Stdio(McpStdioServerConfig {
            command: "spec-no-such-tool-server".to_owned(),
            args: Vec::new(),
            env: Default::default(),
            timeout: None,
            always_load: None,
        }),
    );
    setup
}

/// A tool server whose command cannot start is reported as failed, and the
/// session goes on without it.
async fn failed_tool_server(session: &mut SpecSession) {
    let statuses = session
        .mcp_server_status()
        .await
        .expect("the session reports its tool servers");
    let broken = statuses.iter().find(|status| status.name == "broken");
    expect!(
        broken.is_some_and(|status| status.status == "failed"),
        "the server that cannot start is failed: {broken:?}"
    );
    let turn = session.turn().await;
    expect!(
        turn.succeeded() && turn.text() == "PONG",
        "the session answers without the failed server: {:?}",
        turn.text()
    );
}

fn raw(turn: &Turn) -> Vec<Value> {
    turn.messages()
        .iter()
        .map(|message| serde_json::to_value(message).expect("a parsed frame serialises"))
        .collect()
}

/// Every tool call in the turn as its name and its input as JSON text.
fn tool_calls(turn: &Turn) -> Vec<(String, String)> {
    raw(turn)
        .into_iter()
        .filter(|frame| frame["type"] == "assistant")
        .flat_map(|frame| {
            frame["message"]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|block| block["type"] == "tool_use")
        .map(|block| {
            (
                block["name"].as_str().unwrap_or_default().to_owned(),
                block["input"].to_string(),
            )
        })
        .collect()
}

fn tool_results(turn: &Turn) -> Vec<Value> {
    raw(turn)
        .into_iter()
        .filter(|frame| frame["type"] == "user")
        .flat_map(|frame| {
            frame["message"]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|block| block["type"] == "tool_result")
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_pads_short_tails() {
        assert_eq!(super::super::base64(b"Man"), "TWFu");
        assert_eq!(super::super::base64(b"Ma"), "TWE=");
        assert_eq!(super::super::base64(b"M"), "TQ==");
    }
}
