//! Specifications for the chat rows a Codex turn produces beyond a plain
//! answer: reasoning summaries, exploring commands, a failing command, file
//! changes, images, compaction, plan mode with its question,
//! a turn that errors, and a signed-out account.
//!
//! Live capture seeds the project with `config.txt` (`VALUE=1`), `old.txt`
//! and `square.png`, a 32×32 red PNG. Prompts name files relative to the
//! project so replay, which has no project, sends the same bytes.

use std::path::Path;

use codex::{
    ApprovalPolicy, Codex, CollaborationMode, CollaborationModeKind, CollaborationModeSettings,
    CommandAction, InputItem, PatchChangeKind, ReasoningEffort, SummaryMode, ThreadEventStream,
    ThreadItem, TurnConfig, TurnEvent, TurnInput, TurnStatus,
};

use super::{ScenarioReport, next_event, report, stringify, thread_config};

/// The seed image, attached to a prompt as a data URL.
pub(crate) const SQUARE_PNG: &[u8] = include_bytes!("../assets/square.png");

/// Every event of one turn, read until it completes.
async fn collect_turn(events: &mut ThreadEventStream) -> Result<Vec<TurnEvent>, String> {
    let mut seen = Vec::new();
    loop {
        let event = next_event(events, "turn completion").await?.event;
        let done = matches!(event, TurnEvent::TurnCompleted { .. });
        seen.push(event);
        if done {
            return Ok(seen);
        }
    }
}

fn completed_items(events: &[TurnEvent]) -> Vec<&ThreadItem> {
    events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::ItemCompleted(item) => Some(item),
            _ => None,
        })
        .collect()
}

fn answers(events: &[TurnEvent]) -> String {
    completed_items(events)
        .into_iter()
        .filter_map(|item| match item {
            ThreadItem::AgentMessage { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn final_status(events: &[TurnEvent]) -> Option<TurnStatus> {
    events.iter().rev().find_map(|event| match event {
        TurnEvent::TurnCompleted { turn } => Some(turn.status.clone()),
        _ => None,
    })
}

async fn full_auto(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<(codex::Thread, ThreadEventStream), String> {
    let mut config = thread_config(model, project);
    config.approval_policy = Some(ApprovalPolicy::Never);
    let thread = codex
        .start_thread(config)
        .await
        .map_err(|error| format!("model {model}: thread/start failed: {error}"))?;
    let events = thread.events().await.map_err(stringify)?;
    Ok((thread, events))
}

/// With a detailed summary requested, reasoning items carry summary text.
pub(super) async fn reasoning_summary(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn_with(
            "Think carefully about which is larger, 17 * 23 or 19 * 21, then reply with only the \
             larger product.",
            TurnConfig {
                effort: Some(ReasoningEffort::High),
                summary: Some(SummaryMode::Detailed),
                ..TurnConfig::default()
            },
        )
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    let summaries = completed_items(&seen)
        .into_iter()
        .filter_map(|item| match item {
            ThreadItem::Reasoning { summary, .. } => Some(summary.join("")),
            _ => None,
        })
        .filter(|summary| !summary.is_empty())
        .count();
    if summaries == 0 {
        return Err("no reasoning item carried summary text".to_string());
    }
    Ok(report(&thread))
}

/// Codex classifies the commands it runs: reading a file, listing files and
/// searching come back as typed command actions.
pub(super) async fn exploring(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn(
            "Run these three shell commands separately, one tool call each: `cat config.txt`, \
             `ls`, and `grep -n VALUE config.txt`. Then reply DONE.",
        )
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    let mut kinds = Vec::new();
    for item in completed_items(&seen) {
        if let ThreadItem::CommandExecution {
            command_actions, ..
        } = item
        {
            for action in command_actions {
                kinds.push(match action {
                    CommandAction::Read { .. } => "read",
                    CommandAction::ListFiles { .. } => "list",
                    CommandAction::Search { .. } => "search",
                    _ => "other",
                });
            }
        }
    }
    for kind in ["read", "list", "search"] {
        if !kinds.contains(&kind) {
            return Err(format!("no {kind} command action among {kinds:?}"));
        }
    }
    Ok(report(&thread))
}

/// A command that exits non-zero completes as a failed command execution
/// with its exit code and output.
pub(super) async fn failing_command(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn(
            "Run exactly this shell command once: ls does-not-exist. Then tell me whether it failed.",
        )
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    let failed = completed_items(&seen).into_iter().any(|item| {
        matches!(
            item,
            ThreadItem::CommandExecution {
                exit_code: Some(code),
                aggregated_output: Some(output),
                ..
            } if *code != 0 && output.contains("does-not-exist")
        )
    });
    if !failed {
        return Err("no command completed with a non-zero exit code".to_string());
    }
    Ok(report(&thread))
}

/// Edits made through apply_patch arrive as file-change items whose changes
/// say whether each file was added, modified or deleted.
pub(super) async fn file_changes(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn(
            "Use apply_patch for every change, one patch per step: first create notes.txt \
             containing the line one; then change that line to two; then delete old.txt. \
             Then reply DONE.",
        )
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    let mut kinds = Vec::new();
    for item in completed_items(&seen) {
        if let ThreadItem::FileChange { changes, .. } = item {
            kinds.extend(changes.iter().filter_map(|change| change.kind.clone()));
        }
    }
    for kind in [
        PatchChangeKind::Add,
        PatchChangeKind::Modify,
        PatchChangeKind::Delete,
    ] {
        if !kinds.contains(&kind) {
            return Err(format!("no {kind:?} change among {kinds:?}"));
        }
    }
    Ok(report(&thread))
}

/// An image goes in attached to the message, and Codex can look at one on
/// disk with its image viewer.
pub(super) async fn image(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn(TurnInput::Items(vec![
            InputItem::Image {
                url: format!("data:image/png;base64,{}", base64(SQUARE_PNG)),
            },
            InputItem::text(
                "Name the color of the attached image in one word. Then look at square.png in \
                 the working directory with your image viewing tool and say whether it is the \
                 same image.",
            ),
        ]))
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    if !completed_items(&seen)
        .into_iter()
        .any(|item| matches!(item, ThreadItem::ImageView { .. }))
    {
        return Err("Codex did not view the image on disk".to_string());
    }
    if !answers(&seen).to_lowercase().contains("red") {
        return Err(format!(
            "the attached image did not reach the model: {}",
            answers(&seen)
        ));
    }
    Ok(report(&thread))
}

/// Compacting a thread on request produces a context-compaction item.
pub(super) async fn compaction(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn("Reply with exactly CODEX_SPEC_BEFORE_COMPACT and nothing else.")
        .await
        .map_err(stringify)?;
    collect_turn(&mut events).await?;
    thread.compact().await.map_err(stringify)?;
    let mut compacted = false;
    loop {
        match next_event(&mut events, "compaction").await?.event {
            TurnEvent::ItemCompleted(ThreadItem::ContextCompaction { .. }) => compacted = true,
            TurnEvent::TurnCompleted { turn } => {
                if turn.status != TurnStatus::Completed {
                    return Err(format!("compaction ended with {:?}", turn.status));
                }
                break;
            }
            TurnEvent::Error { message, .. } => return Err(format!("compaction error: {message}")),
            _ => {}
        }
    }
    if !compacted {
        return Err("no context-compaction item completed".to_string());
    }
    Ok(report(&thread))
}

/// In plan mode Codex asks its questions through the request-user-input tool,
/// which reaches the client as a server request, and delivers its plan as a
/// plan item.
pub(super) async fn plan_mode(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn_with(
            "Before planning, ask me exactly one question with your tool for asking the user: \
             which color should color.txt hold, with the options Red and Blue. Then propose a \
             short plan to write my answer into color.txt.",
            TurnConfig {
                collaboration_mode: Some(CollaborationMode {
                    mode: CollaborationModeKind::Plan,
                    settings: CollaborationModeSettings {
                        model: model.to_string(),
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..TurnConfig::default()
            },
        )
        .await
        .map_err(stringify)?;
    let mut asked = false;
    let mut planned = false;
    loop {
        match next_event(&mut events, "plan mode turn").await?.event {
            TurnEvent::ServerRequest { id, method, params }
                if method == "item/tool/requestUserInput" =>
            {
                asked = true;
                let mut answers = serde_json::Map::new();
                for question in params["questions"].as_array().into_iter().flatten() {
                    let id = question["id"].as_str().unwrap_or_default().to_owned();
                    answers.insert(id, serde_json::json!({ "answers": ["Blue"] }));
                }
                thread
                    .respond_raw(id, serde_json::json!({ "answers": answers }))
                    .await
                    .map_err(stringify)?;
            }
            TurnEvent::ItemCompleted(ThreadItem::Plan { text, .. }) if !text.is_empty() => {
                planned = true;
            }
            TurnEvent::TurnCompleted { turn } => {
                if turn.status != TurnStatus::Completed {
                    return Err(format!("plan turn ended with {:?}", turn.status));
                }
                break;
            }
            TurnEvent::Error { message, .. } => return Err(format!("turn error: {message}")),
            _ => {}
        }
    }
    if !asked {
        return Err("Codex did not ask through request-user-input".to_string());
    }
    if !planned {
        return Err("no plan item completed".to_string());
    }
    Ok(report(&thread))
}

/// A turn against a model the account cannot use fails with an error event
/// and a failed turn.
pub(super) async fn turn_error(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn_with(
            "Reply with exactly CODEX_SPEC_UNREACHABLE.",
            TurnConfig {
                model: Some("codex-spec-no-such-model".to_string()),
                ..TurnConfig::default()
            },
        )
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    if !seen
        .iter()
        .any(|event| matches!(event, TurnEvent::Error { .. }))
    {
        return Err("no error event arrived".to_string());
    }
    if final_status(&seen) != Some(TurnStatus::Failed) {
        return Err(format!("the turn ended {:?}", final_status(&seen)));
    }
    Ok(report(&thread))
}

/// Without credentials the account reads as signed out and a turn fails.
pub(super) async fn signed_out(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let account = codex
        .read_account(codex::AccountReadParams {
            refresh_token: None,
        })
        .await
        .map_err(stringify)?;
    if account.account.is_some() {
        return Err(format!("an account was signed in: {account:?}"));
    }
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn("Reply with exactly CODEX_SPEC_SIGNED_OUT.")
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    if final_status(&seen) != Some(TurnStatus::Failed) {
        return Err(format!(
            "the signed-out turn ended {:?}",
            final_status(&seen)
        ));
    }
    Ok(report(&thread))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (index, byte)| {
            word | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(ALPHABET[(word >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_pads_short_tails() {
        assert_eq!(super::base64(b"Man"), "TWFu");
        assert_eq!(super::base64(b"Ma"), "TWE=");
        assert_eq!(super::base64(b"M"), "TQ==");
    }
}
