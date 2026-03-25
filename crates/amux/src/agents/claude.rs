//! Claude Code agent session.
//!
//! Two-phase init: [`ClaudeSession::new`] stores metadata,
//! [`ClaudeSession::start`] spawns the PTY process. Hook handling and structured
//! input translation are encapsulated here.

use super::{PtyHandle, spawn_pty_agent};
use crate::buffer::MultiplexStructuredReader;
use crate::claude::structured_log_source::StructuredLogSource;
use crate::claude::types::{
    AgentStructuredOutput, AskUserQuestionOption, AskUserQuestionResponse, ClaudeHook,
    ClaudeStructuredInput, ClaudeStructuredOutput, Hook, PermissionResponse,
};
use crate::error::Result;
use crate::message::{CreateAgentRequest, ProtocolError};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

const RIGHT_ARROW: &[u8] = b"\x1b[C";
const LEFT_ARROW: &[u8] = b"\x1b[D";
const UP_ARROW: &[u8] = b"\x1b[A";
const DOWN_ARROW: &[u8] = b"\x1b[B";
const DELAY: Duration = Duration::from_millis(20);

enum PtyAction {
    Send(Vec<u8>),
    Delay(Duration),
}

fn permission_response_keystrokes(response: &PermissionResponse) -> Vec<PtyAction> {
    let byte: &[u8] = match response {
        PermissionResponse::Yes => b"1",
        PermissionResponse::YesAll => b"2",
        PermissionResponse::No => b"3",
    };
    vec![PtyAction::Send(byte.to_vec())]
}

fn submit_message_keystrokes(data: &[u8]) -> Vec<PtyAction> {
    vec![
        PtyAction::Send(data.to_vec()),
        PtyAction::Delay(DELAY),
        PtyAction::Send(b"\r".to_vec()),
    ]
}

/// Find the 0-based index of the option whose label matches `answer`.
/// Returns `None` if no label matches (i.e. "Other" / custom text).
fn find_option_index(answer: &str, options: &[AskUserQuestionOption]) -> Option<usize> {
    options.iter().position(|opt| opt.label == answer)
}

/// Keystrokes for a standard single-select question (digit press).
fn select_keystrokes(answer: &str, options: &[AskUserQuestionOption]) -> Vec<PtyAction> {
    match find_option_index(answer, options) {
        Some(idx) => {
            let digit = idx + 1; // 1-based UI index
            vec![PtyAction::Send(digit.to_string().into_bytes())]
        }
        None => {
            // "Other" — digit for num_options+1, delay, type text, Enter
            let other_digit = options.len() + 1;
            vec![
                PtyAction::Send(other_digit.to_string().into_bytes()),
                PtyAction::Delay(DELAY),
                PtyAction::Send(answer.as_bytes().to_vec()),
                PtyAction::Delay(DELAY),
                PtyAction::Send(b"\r".to_vec()),
            ]
        }
    }
}

/// Keystrokes for a preview question (options with preview).
/// Arrow-nav to the target option, then Enter. No "Other" option exists.
/// Focus starts on the first option; delays between Down presses give
/// the preview UI time to process each navigation.
fn preview_keystrokes(answer: &str, options: &[AskUserQuestionOption]) -> Vec<PtyAction> {
    let idx = find_option_index(answer, options).unwrap_or(0);
    let mut actions = Vec::new();
    for _ in 0..idx {
        actions.push(PtyAction::Send(DOWN_ARROW.to_vec()));
        actions.push(PtyAction::Delay(DELAY));
    }
    actions.push(PtyAction::Send(b"\r".to_vec()));
    actions
}

/// Keystrokes for a multi-select question.
/// Navigates with arrows, Space to toggle each selected option.
fn multi_select_keystrokes(answer: &str, options: &[AskUserQuestionOption]) -> Vec<PtyAction> {
    let labels: Vec<&str> = answer.split(", ").collect();
    let mut actions = Vec::new();
    let mut cursor_pos: usize = 0; // 0-based

    // Resolve each label to an index, sort for efficient navigation
    let mut indices: Vec<(usize, Option<&str>)> = Vec::new();
    for label in &labels {
        match find_option_index(label, options) {
            Some(idx) => indices.push((idx, None)),
            None => {
                // Custom "Other" option is at options.len()
                indices.push((options.len(), Some(label)));
            }
        }
    }
    indices.sort_by_key(|(idx, _)| *idx);

    for (target, custom_text) in &indices {
        // Navigate to target
        for _ in cursor_pos..*target {
            actions.push(PtyAction::Send(DOWN_ARROW.to_vec()));
            actions.push(PtyAction::Delay(DELAY));
        }
        for _ in *target..cursor_pos {
            actions.push(PtyAction::Send(UP_ARROW.to_vec()));
            actions.push(PtyAction::Delay(DELAY));
        }
        cursor_pos = *target;

        if let Some(text) = custom_text {
            // Other: just type text (auto-selects the checkbox), then navigate away
            actions.push(PtyAction::Delay(DELAY));
            actions.push(PtyAction::Send(text.as_bytes().to_vec()));
            actions.push(PtyAction::Delay(DELAY));
            actions.push(PtyAction::Send(UP_ARROW.to_vec()));
            actions.push(PtyAction::Delay(DELAY));
        } else {
            // Predefined option: Space to toggle
            actions.push(PtyAction::Send(b" ".to_vec()));
        }
    }

    actions
}

/// Keystrokes to select "Chat about this" on a question.
/// For single-select: digit press (num_options + 2).
/// For multi-select: arrow-nav to the position, then Enter.
fn chat_about_this_keystrokes(
    options: &[AskUserQuestionOption],
    multi_select: bool,
) -> Vec<PtyAction> {
    // ChatAboutThis is after "Other" in the option list but isn't digit-selectable,
    // so we always use arrow navigation + Enter regardless of question type.
    // 0-based target: num_options + 1 (options… Other… ChatAboutThis)
    let target = options.len() + 1;
    let mut actions = Vec::new();
    // For multi-select cursor starts at 0; for single-select it also starts at 0
    for _ in 0..target {
        actions.push(PtyAction::Send(DOWN_ARROW.to_vec()));
        actions.push(PtyAction::Delay(DELAY));
    }
    actions.push(PtyAction::Send(b"\r".to_vec()));
    let _ = multi_select; // currently no difference, kept for future use
    actions
}

/// Build the PTY keystroke sequence for an AskUserQuestionResponse.
///
/// Derives question type and selection indices from the echoed questions:
/// - Any option has `preview` → preview (arrow nav + Enter)
/// - `multi_select: true` → multi-select (arrow nav + Space toggle)
/// - Otherwise → select (digit press)
///
/// In multi-question forms, single-select (digit press) and preview (Enter)
/// auto-advance to the next page, so no explicit Right arrow is needed
/// between them. Multi-select (Space toggle) does NOT auto-advance and
/// still requires Right arrow navigation. The last auto-advancing selection
/// advances to the submit page, where Enter is needed to confirm.
///
/// If `chat_about_this` is set, all answered questions are processed first,
/// then we navigate to the ChatAboutThis page and select it (no submit step).
fn ask_question_keystrokes(response: &AskUserQuestionResponse) -> Vec<PtyAction> {
    let mut actions = Vec::new();
    let num_questions = response.questions.len();
    let mut current_page = 0;
    let multi_question = num_questions > 1;

    // Find the ChatAboutThis page index, if any
    let chat_page = response.chat_about_this.as_ref().and_then(|q| {
        response
            .questions
            .iter()
            .position(|item| item.question == *q)
    });

    // Phase 1: process all answered questions (skip the ChatAboutThis page)
    for (i, question) in response.questions.iter().enumerate() {
        if Some(i) == chat_page {
            continue;
        }

        let Some(answer) = response.answers.get(&question.question) else {
            continue;
        };

        // Navigate forward to this page (only needed after multi-select
        // which doesn't auto-advance, or when a page was skipped)
        while current_page < i {
            actions.push(PtyAction::Delay(DELAY));
            actions.push(PtyAction::Send(RIGHT_ARROW.to_vec()));
            actions.push(PtyAction::Delay(DELAY));
            current_page += 1;
        }

        let is_preview = question.options.iter().any(|o| o.preview.is_some());

        if is_preview {
            actions.extend(preview_keystrokes(answer, &question.options));
            // Preview Enter auto-advances in multi-question forms
            if multi_question {
                actions.push(PtyAction::Delay(DELAY));
                current_page += 1;
            }
        } else if question.multi_select {
            actions.extend(multi_select_keystrokes(answer, &question.options));
        } else {
            actions.extend(select_keystrokes(answer, &question.options));
            // Digit press auto-advances in multi-question forms
            if multi_question {
                actions.push(PtyAction::Delay(DELAY));
                current_page += 1;
            }
        }
    }

    // Phase 2: ChatAboutThis — navigate to its page and select it (no submit)
    if let Some(chat_idx) = chat_page {
        while current_page < chat_idx {
            actions.push(PtyAction::Delay(DELAY));
            actions.push(PtyAction::Send(RIGHT_ARROW.to_vec()));
            actions.push(PtyAction::Delay(DELAY));
            current_page += 1;
        }
        while current_page > chat_idx {
            actions.push(PtyAction::Delay(DELAY));
            actions.push(PtyAction::Send(LEFT_ARROW.to_vec()));
            actions.push(PtyAction::Delay(DELAY));
            current_page -= 1;
        }

        let question = &response.questions[chat_idx];
        actions.extend(chat_about_this_keystrokes(
            &question.options,
            question.multi_select,
        ));
    }
    // Phase 3: no ChatAboutThis — submit when the form has a submit button
    // (multi-question forms always do; single multi-select questions do too,
    // since Space toggles don't auto-submit like digit presses)
    else if num_questions > 1 || response.questions.iter().any(|q| q.multi_select) {
        while current_page < num_questions {
            actions.push(PtyAction::Delay(DELAY));
            actions.push(PtyAction::Send(RIGHT_ARROW.to_vec()));
            actions.push(PtyAction::Delay(DELAY));
            current_page += 1;
        }
        actions.push(PtyAction::Send(b"\r".to_vec()));
    }

    actions
}

async fn execute_pty_actions(pty: &PtyHandle, actions: &[PtyAction]) -> Result<()> {
    for action in actions {
        match action {
            PtyAction::Send(bytes) => pty.send_input(bytes.clone()).await?,
            PtyAction::Delay(dur) => tokio::time::sleep(*dur).await,
        }
    }
    Ok(())
}

pub struct ClaudeSession {
    pub(super) agent_id: Uuid,
    pub(super) name: Option<String>,
    pub(super) command: String,
    pub(super) working_dir: PathBuf,
    pub(super) pty: Option<PtyHandle>,
    log_source: Option<StructuredLogSource>,

    // Stored for deferred start()
    pub(super) terminal_size: Option<crate::message::TerminalSize>,
    /// Claude session ID. Set from SessionStart hook during normal operation,
    /// or pre-set before `start()` for resume (triggers `--resume <id>`).
    pub(super) session_id: Option<Uuid>,
    /// True for externally-started sessions (no PTY, transcript-only)
    pub(super) readonly: bool,
    /// Extra arguments passed to the claude command
    pub(super) args: Vec<String>,
}

impl ClaudeSession {
    /// Create a new ClaudeSession from a CreateAgentRequest.
    /// Does not spawn the process — call [`start`] afterwards.
    pub fn new(req: &CreateAgentRequest) -> Self {
        Self {
            agent_id: req.agent_id,
            name: req.name.clone(),
            command: "claude".to_string(),
            working_dir: req.working_dir.clone(),
            pty: None,
            log_source: None,
            terminal_size: req.terminal_size,
            session_id: None,
            readonly: false,
            args: req.args.clone(),
        }
    }

    /// Create a readonly session for an externally-started Claude process.
    /// Has a StructuredLogSource (for transcript tailing) but no PTY.
    pub fn new_readonly(agent_id: Uuid, working_dir: PathBuf) -> Self {
        Self {
            agent_id,
            name: None,
            command: "claude".to_string(),
            working_dir,
            pty: None,
            log_source: Some(StructuredLogSource::new()),
            terminal_size: None,
            session_id: None,
            readonly: true,
            args: vec![],
        }
    }

    /// Link a transcript file for structured output tailing.
    pub async fn link_transcript(&self, path: PathBuf) {
        if let Some(log_source) = &self.log_source {
            log_source.link_transcript(path).await;
        }
    }

    /// Spawn the Claude Code process. Returns an exit handle that completes
    /// when the process exits. If `session_id` is set, passes `--resume <id>`.
    /// Extra args from creation are appended.
    pub fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        let env = [("AMUX_AGENT_ID", self.agent_id.to_string())];
        let mut args: Vec<String> = match self.session_id {
            Some(id) => vec!["--resume".to_string(), id.to_string()],
            None => vec![],
        };
        args.extend(self.args.iter().cloned());
        let (pty, log_source, exit_handle) = spawn_pty_agent(
            self.agent_id,
            &self.command,
            &args,
            &self.working_dir,
            &env,
            self.terminal_size,
        )?;
        self.pty = Some(pty);
        self.log_source = Some(log_source);
        Ok(exit_handle)
    }

    /// Send structured input to Claude Code.
    async fn send_input(&self, input: ClaudeStructuredInput) -> Result<()> {
        let Some(pty) = &self.pty else {
            return Ok(());
        };
        let actions = match &input {
            ClaudeStructuredInput::SubmitMessage { data } => submit_message_keystrokes(data),
            ClaudeStructuredInput::PermissionResponse(response) => {
                tracing::info!(agent_id = %self.agent_id, ?response, "sending permission response");
                permission_response_keystrokes(response)
            }
            ClaudeStructuredInput::AskUserQuestionResponse(response) => {
                tracing::info!(agent_id = %self.agent_id, num_questions = response.questions.len(), "sending AskUserQuestion response");
                ask_question_keystrokes(response)
            }
        };
        execute_pty_actions(pty, &actions).await
    }

    /// Validate seq and send structured input to Claude Code.
    pub async fn send_structured_input(
        &self,
        client_seq: u64,
        input: ClaudeStructuredInput,
    ) -> std::result::Result<(), ProtocolError> {
        if self.readonly {
            return Err(ProtocolError::ServerError(
                "session is readonly".to_string(),
            ));
        }
        let current_seq = self.current_seq().await;
        if client_seq != current_seq {
            return Err(ProtocolError::SequenceNumberMismatch {
                client_seq,
                current_seq,
            });
        }

        self.send_input(input)
            .await
            .map_err(|e| ProtocolError::ServerError(e.to_string()))
    }

    /// Handle a hook event.
    pub async fn handle_hook(&mut self, hook: Hook) -> Result<()> {
        let Some(log_source) = &self.log_source else {
            return Ok(());
        };
        match hook {
            Hook::Claude(ClaudeHook::SessionStart(session_start)) => {
                tracing::debug!(agent_id = %self.agent_id, session_id = %session_start.session_id, "linking transcript");
                self.session_id = Some(session_start.session_id);
                log_source
                    .link_transcript(PathBuf::from(&session_start.transcript_path))
                    .await;
            }
            Hook::Claude(ClaudeHook::PermissionRequest(perm_req)) => {
                tracing::debug!(agent_id = %self.agent_id, "permission request");
                log_source
                    .write(AgentStructuredOutput::Claude(
                        ClaudeStructuredOutput::PermissionRequest {
                            tool: perm_req.tool,
                        },
                    ))
                    .await;
            }
            Hook::Claude(ClaudeHook::PreToolUse(pre)) => {
                tracing::debug!(agent_id = %self.agent_id, tool = %pre.tool, "pre-tool-use");
                let timestamp = chrono::Utc::now().to_rfc3339();
                log_source
                    .write(AgentStructuredOutput::Claude(
                        ClaudeStructuredOutput::PreToolUseEvent {
                            tool_use_id: pre.tool_use_id,
                            tool: pre.tool,
                            timestamp,
                        },
                    ))
                    .await;
            }
            Hook::Claude(ClaudeHook::PostToolUse(post)) => {
                tracing::debug!(agent_id = %self.agent_id, tool = %post.tool, "post-tool-use");
                let timestamp = chrono::Utc::now().to_rfc3339();
                log_source
                    .write(AgentStructuredOutput::Claude(
                        ClaudeStructuredOutput::PostToolUseEvent {
                            tool_use_id: post.tool_use_id,
                            tool: post.tool,
                            timestamp,
                        },
                    ))
                    .await;
            }
            Hook::Claude(ClaudeHook::PostToolUseFailure(post)) => {
                tracing::debug!(agent_id = %self.agent_id, tool = %post.tool, "post-tool-use-failure");
                let timestamp = chrono::Utc::now().to_rfc3339();
                log_source
                    .write(AgentStructuredOutput::Claude(
                        ClaudeStructuredOutput::PostToolUseFailureEvent {
                            tool_use_id: post.tool_use_id,
                            tool: post.tool,
                            error: post.error,
                            is_interrupt: post.is_interrupt,
                            timestamp,
                        },
                    ))
                    .await;
            }
            Hook::Claude(ClaudeHook::Stop(_)) => {
                tracing::debug!(agent_id = %self.agent_id, "agent stopped");
                log_source
                    .write(AgentStructuredOutput::Claude(
                        ClaudeStructuredOutput::AgentStopped,
                    ))
                    .await;
            }
            Hook::Claude(ClaudeHook::SessionEnd(_)) => {
                tracing::debug!(agent_id = %self.agent_id, "session ended");
                log_source
                    .write(AgentStructuredOutput::Claude(
                        ClaudeStructuredOutput::AgentStopped,
                    ))
                    .await;
            }
            Hook::Claude(ClaudeHook::Unknown) => {}
        }
        Ok(())
    }

    /// Return the current structured output sequence number.
    pub async fn current_seq(&self) -> u64 {
        match &self.log_source {
            Some(log_source) => log_source.current_seq().await,
            None => 0,
        }
    }

    /// Subscribe to structured log output.
    pub async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        self.log_source.as_ref()?.subscribe().await
    }

    /// Subscribe to structured log output and return the matching seq.
    pub async fn subscribe_with_current_seq(&self) -> Option<(MultiplexStructuredReader, u64)> {
        self.log_source.as_ref()?.subscribe_with_current_seq().await
    }

    /// Shut down the session according to the given policy.
    pub async fn stop(&self, policy: super::StopPolicy) {
        tracing::info!(agent_id = %self.agent_id, "shutting down claude session");
        match policy {
            super::StopPolicy::Interrupt => {
                if let Some(pty) = &self.pty {
                    let _ = pty.send_input(vec![0x03]).await;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let _ = pty.send_input(vec![0x03]).await;
                }
            }
        }
        if let Some(pty) = &self.pty {
            pty.close().await;
        }
        if let Some(log_source) = &self.log_source {
            log_source.close().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::{AskUserQuestionItem, AskUserQuestionOption};
    use std::collections::HashMap;

    /// Extract just the Send bytes from a PtyAction sequence, ignoring delays.
    fn sends(actions: &[PtyAction]) -> Vec<Vec<u8>> {
        actions
            .iter()
            .filter_map(|a| match a {
                PtyAction::Send(bytes) => Some(bytes.clone()),
                PtyAction::Delay(_) => None,
            })
            .collect()
    }

    fn make_options(labels: &[&str]) -> Vec<AskUserQuestionOption> {
        labels
            .iter()
            .map(|l| AskUserQuestionOption {
                label: l.to_string(),
                description: String::new(),
                preview: None,
            })
            .collect()
    }

    fn make_preview_options(labels: &[&str]) -> Vec<AskUserQuestionOption> {
        labels
            .iter()
            .map(|l| AskUserQuestionOption {
                label: l.to_string(),
                description: String::new(),
                preview: Some("```preview```".to_string()),
            })
            .collect()
    }

    fn make_question(text: &str, options: Vec<AskUserQuestionOption>) -> AskUserQuestionItem {
        AskUserQuestionItem {
            question: text.to_string(),
            header: "H".to_string(),
            options,
            multi_select: false,
        }
    }

    fn make_multi_question(text: &str, options: Vec<AskUserQuestionOption>) -> AskUserQuestionItem {
        AskUserQuestionItem {
            question: text.to_string(),
            header: "H".to_string(),
            options,
            multi_select: true,
        }
    }

    #[test]
    fn test_permission_response_keystrokes() {
        assert_eq!(
            sends(&permission_response_keystrokes(&PermissionResponse::Yes)),
            vec![b"1".to_vec()]
        );
        assert_eq!(
            sends(&permission_response_keystrokes(&PermissionResponse::YesAll)),
            vec![b"2".to_vec()]
        );
        assert_eq!(
            sends(&permission_response_keystrokes(&PermissionResponse::No)),
            vec![b"3".to_vec()]
        );
    }

    #[test]
    fn test_submit_message_keystrokes() {
        let actions = submit_message_keystrokes(b"hello");
        assert_eq!(sends(&actions), vec![b"hello".to_vec(), b"\r".to_vec()]);
        assert!(matches!(actions[1], PtyAction::Delay(_)));
    }

    #[test]
    fn test_single_question_select_predefined() {
        let response = AskUserQuestionResponse {
            questions: vec![make_question("Q?", make_options(&["A", "B"]))],
            chat_about_this: None,
            answers: HashMap::from([("Q?".to_string(), "B".to_string())]),
        };
        let actions = ask_question_keystrokes(&response);
        assert_eq!(sends(&actions), vec![b"2".to_vec()]);
    }

    #[test]
    fn test_single_question_select_custom() {
        let response = AskUserQuestionResponse {
            questions: vec![make_question("Q?", make_options(&["A", "B"]))],
            chat_about_this: None,
            answers: HashMap::from([("Q?".to_string(), "hello".to_string())]),
        };
        let actions = ask_question_keystrokes(&response);
        let s = sends(&actions);
        assert_eq!(s[0], b"3"); // Other = num_options + 1
        assert_eq!(s[1], b"hello");
        assert_eq!(s[2], b"\r");
    }

    #[test]
    fn test_preview_question_first_option() {
        let response = AskUserQuestionResponse {
            questions: vec![make_question(
                "Q?",
                make_preview_options(&["Layout A", "Layout B"]),
            )],
            chat_about_this: None,
            answers: HashMap::from([("Q?".to_string(), "Layout A".to_string())]),
        };
        let actions = ask_question_keystrokes(&response);
        // First option: already focused, just Enter
        assert_eq!(sends(&actions), vec![b"\r".to_vec()]);
    }

    #[test]
    fn test_preview_question_second_option() {
        let response = AskUserQuestionResponse {
            questions: vec![make_question(
                "Q?",
                make_preview_options(&["Layout A", "Layout B"]),
            )],
            chat_about_this: None,
            answers: HashMap::from([("Q?".to_string(), "Layout B".to_string())]),
        };
        let actions = ask_question_keystrokes(&response);
        // Second option: one Down + Enter
        assert_eq!(sends(&actions), vec![DOWN_ARROW.to_vec(), b"\r".to_vec()]);
    }

    #[test]
    fn test_multi_question_right_arrows_and_submit() {
        let response = AskUserQuestionResponse {
            questions: vec![
                make_question("Q1?", make_options(&["A", "B"])),
                make_question("Q2?", make_options(&["C", "D"])),
            ],
            answers: HashMap::from([
                ("Q1?".to_string(), "A".to_string()),
                ("Q2?".to_string(), "D".to_string()),
            ]),
            chat_about_this: None,
        };
        let actions = ask_question_keystrokes(&response);
        // Digit presses auto-advance: "1"→page 1, "2"→submit page, Enter
        assert_eq!(
            sends(&actions),
            vec![b"1".to_vec(), b"2".to_vec(), b"\r".to_vec(),]
        );
    }

    #[test]
    fn test_multi_select_up_down_space() {
        let response = AskUserQuestionResponse {
            questions: vec![make_multi_question("Q?", make_options(&["A", "B", "C"]))],
            chat_about_this: None,
            answers: HashMap::from([("Q?".to_string(), "A, C".to_string())]),
        };
        let actions = ask_question_keystrokes(&response);
        assert_eq!(
            sends(&actions),
            vec![
                b" ".to_vec(),        // toggle A (index 0)
                DOWN_ARROW.to_vec(),  // 0→1
                DOWN_ARROW.to_vec(),  // 1→2
                b" ".to_vec(),        // toggle C (index 2)
                RIGHT_ARROW.to_vec(), // submit page
                b"\r".to_vec(),       // submit
            ]
        );
    }

    #[test]
    fn test_multi_select_custom_navigate_away() {
        let response = AskUserQuestionResponse {
            questions: vec![make_multi_question("Q?", make_options(&["A", "B", "C"]))],
            chat_about_this: None,
            answers: HashMap::from([("Q?".to_string(), "A, extra".to_string())]),
        };
        let actions = ask_question_keystrokes(&response);
        let s = sends(&actions);
        assert_eq!(s[0], b" "); // toggle A (index 0)
        assert_eq!(s[1], DOWN_ARROW); // 0→1
        assert_eq!(s[2], DOWN_ARROW); // 1→2
        assert_eq!(s[3], DOWN_ARROW); // 2→3 (Other)
        assert_eq!(s[4], b"extra"); // type custom text (auto-selects Other)
        assert_eq!(s[5], UP_ARROW); // navigate away from text input
        assert_eq!(s[6], RIGHT_ARROW); // submit page
        assert_eq!(s[7], b"\r"); // submit
        assert_eq!(s.len(), 8);
    }

    #[test]
    fn test_find_option_index_match() {
        let options = make_options(&["A", "B", "C"]);
        assert_eq!(find_option_index("B", &options), Some(1));
    }

    #[test]
    fn test_find_option_index_no_match() {
        let options = make_options(&["A", "B"]);
        assert_eq!(find_option_index("custom text", &options), None);
    }

    #[test]
    fn test_chat_about_this_single_question_single_select() {
        // Single question, "Chat about this" selected (no answers)
        let response = AskUserQuestionResponse {
            questions: vec![make_question("Q?", make_options(&["A", "B"]))],
            answers: HashMap::new(),
            chat_about_this: Some("Q?".to_string()),
        };
        let actions = ask_question_keystrokes(&response);
        // 2 options → ChatAboutThis at 0-based index 3, arrow-nav + Enter
        assert_eq!(
            sends(&actions),
            vec![
                DOWN_ARROW.to_vec(), // 0→1
                DOWN_ARROW.to_vec(), // 1→2
                DOWN_ARROW.to_vec(), // 2→3
                b"\r".to_vec(),
            ]
        );
    }

    #[test]
    fn test_chat_about_this_single_question_multi_select() {
        // Single multi-select question, "Chat about this" selected
        let response = AskUserQuestionResponse {
            questions: vec![make_multi_question("Q?", make_options(&["A", "B"]))],
            answers: HashMap::new(),
            chat_about_this: Some("Q?".to_string()),
        };
        let actions = ask_question_keystrokes(&response);
        // 2 options → ChatAboutThis at 0-based index 3, arrow-nav + Enter
        assert_eq!(
            sends(&actions),
            vec![
                DOWN_ARROW.to_vec(), // 0→1
                DOWN_ARROW.to_vec(), // 1→2
                DOWN_ARROW.to_vec(), // 2→3
                b"\r".to_vec(),
            ]
        );
    }

    #[test]
    fn test_chat_about_this_last_question_no_submit() {
        // Q1 answered, Q2 is ChatAboutThis — no submit step
        let response = AskUserQuestionResponse {
            questions: vec![
                make_question("Q1?", make_options(&["A", "B"])),
                make_question("Q2?", make_options(&["C", "D"])),
            ],
            answers: HashMap::from([("Q1?".to_string(), "A".to_string())]),
            chat_about_this: Some("Q2?".to_string()),
        };
        let actions = ask_question_keystrokes(&response);
        // Q1 digit auto-advances to page 1, arrow-nav to ChatAboutThis, no submit
        assert_eq!(
            sends(&actions),
            vec![
                b"1".to_vec(),
                DOWN_ARROW.to_vec(),
                DOWN_ARROW.to_vec(),
                DOWN_ARROW.to_vec(),
                b"\r".to_vec(),
            ]
        );
    }

    #[test]
    fn test_chat_about_this_navigate_back() {
        // Q1 is ChatAboutThis, Q2 answered — process Q2 first, navigate back
        let response = AskUserQuestionResponse {
            questions: vec![
                make_question("Q1?", make_options(&["A", "B"])),
                make_question("Q2?", make_options(&["C", "D"])),
            ],
            answers: HashMap::from([("Q2?".to_string(), "D".to_string())]),
            chat_about_this: Some("Q1?".to_string()),
        };
        let actions = ask_question_keystrokes(&response);
        // Phase 1: skip Q1, navigate to Q2 (right), select "2", auto-advance→page 2
        // Phase 2: navigate back to Q1: left, left, arrow-nav to ChatAboutThis
        assert_eq!(
            sends(&actions),
            vec![
                RIGHT_ARROW.to_vec(),
                b"2".to_vec(),
                LEFT_ARROW.to_vec(),
                LEFT_ARROW.to_vec(),
                DOWN_ARROW.to_vec(),
                DOWN_ARROW.to_vec(),
                DOWN_ARROW.to_vec(),
                b"\r".to_vec(),
            ]
        );
    }

    #[test]
    fn test_chat_about_this_middle_question_three_pages() {
        // Q1 answered, Q2 is ChatAboutThis, Q3 answered
        let response = AskUserQuestionResponse {
            questions: vec![
                make_question("Q1?", make_options(&["A", "B"])),
                make_question("Q2?", make_options(&["C", "D"])),
                make_question("Q3?", make_options(&["E", "F"])),
            ],
            answers: HashMap::from([
                ("Q1?".to_string(), "A".to_string()),
                ("Q3?".to_string(), "F".to_string()),
            ]),
            chat_about_this: Some("Q2?".to_string()),
        };
        let actions = ask_question_keystrokes(&response);
        // Phase 1: Q1 select "1" auto-advance→page 1 (Q2, skip),
        //          right→page 2 (Q3), select "2" auto-advance→page 3
        // Phase 2: navigate back from page 3 to page 1: left, left, arrow-nav to ChatAboutThis
        assert_eq!(
            sends(&actions),
            vec![
                b"1".to_vec(),
                RIGHT_ARROW.to_vec(),
                b"2".to_vec(),
                LEFT_ARROW.to_vec(),
                LEFT_ARROW.to_vec(),
                DOWN_ARROW.to_vec(),
                DOWN_ARROW.to_vec(),
                DOWN_ARROW.to_vec(),
                b"\r".to_vec(),
            ]
        );
    }
}
