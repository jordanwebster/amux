//! Claude Code agent session.
//!
//! Two-phase init: [`ClaudeSession::new`] stores metadata,
//! [`ClaudeSession::start`] spawns the PTY process. Hook handling and structured
//! input translation are encapsulated here.

use super::{PtyHandle, spawn_pty_agent};
use crate::buffer::MultiplexStructuredReader;
use crate::claude::structured_log_source::StructuredLogSource;
use crate::claude::types::{
    AskUserQuestionOption, AskUserQuestionResponse, ClaudeHook, ClaudeStructuredInput,
    ClaudeStructuredOutput, Hook, PermissionResponse, StructuredInput, StructuredOutput,
};
use crate::error::Result;
use crate::message::CreateAgentRequest;
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

/// Keystrokes for a preview question (options with markdown).
/// Arrow-nav to the target option, then Enter. No "Other" option exists.
fn preview_keystrokes(answer: &str, options: &[AskUserQuestionOption]) -> Vec<PtyAction> {
    let idx = find_option_index(answer, options).unwrap_or(0);
    let mut actions = Vec::new();
    for _ in 0..idx {
        actions.push(PtyAction::Send(DOWN_ARROW.to_vec()));
    }
    actions.push(PtyAction::Delay(DELAY));
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

    let mut last_was_custom = false;
    for (target, custom_text) in &indices {
        // Navigate to target
        for _ in cursor_pos..*target {
            actions.push(PtyAction::Send(DOWN_ARROW.to_vec()));
        }
        for _ in *target..cursor_pos {
            actions.push(PtyAction::Send(UP_ARROW.to_vec()));
        }
        cursor_pos = *target;

        // Toggle
        actions.push(PtyAction::Send(b" ".to_vec()));

        if let Some(text) = custom_text {
            actions.push(PtyAction::Delay(DELAY));
            actions.push(PtyAction::Send(text.as_bytes().to_vec()));
            last_was_custom = true;
        } else {
            last_was_custom = false;
        }
    }

    // If last was custom, navigate away to re-enable left/right
    if last_was_custom {
        actions.push(PtyAction::Send(UP_ARROW.to_vec()));
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
    // ChatAboutThis is always after "Other": num_options + 2 (1-based)
    let index = options.len() + 2;
    if multi_select {
        // Navigate from cursor position 0 to index-1 (0-based), then Enter
        let mut actions = Vec::new();
        for _ in 0..index - 1 {
            actions.push(PtyAction::Send(DOWN_ARROW.to_vec()));
        }
        actions.push(PtyAction::Send(b"\r".to_vec()));
        actions
    } else {
        vec![PtyAction::Send(index.to_string().into_bytes())]
    }
}

/// Build the PTY keystroke sequence for an AskUserQuestionResponse.
///
/// Derives question type and selection indices from the echoed questions:
/// - Any option has `markdown` → preview (arrow nav + Enter)
/// - `multi_select: true` → multi-select (arrow nav + Space toggle)
/// - Otherwise → select (digit press)
///
/// For multi-question forms, Right arrow navigates between pages and
/// a final Right + Enter submits. If `chat_about_this` is set, all
/// answered questions are processed first, then we navigate to the
/// ChatAboutThis page and select it (no submit step).
fn ask_question_keystrokes(response: &AskUserQuestionResponse) -> Vec<PtyAction> {
    let mut actions = Vec::new();
    let num_questions = response.questions.len();
    let mut current_page = 0;

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

        // Navigate forward to this page
        while current_page < i {
            actions.push(PtyAction::Send(RIGHT_ARROW.to_vec()));
            current_page += 1;
        }

        let is_preview = question.options.iter().any(|o| o.markdown.is_some());

        if is_preview {
            actions.extend(preview_keystrokes(answer, &question.options));
        } else if question.multi_select {
            actions.extend(multi_select_keystrokes(answer, &question.options));
        } else {
            actions.extend(select_keystrokes(answer, &question.options));
        }
    }

    // Phase 2: ChatAboutThis — navigate to its page and select it (no submit)
    if let Some(chat_idx) = chat_page {
        while current_page < chat_idx {
            actions.push(PtyAction::Send(RIGHT_ARROW.to_vec()));
            current_page += 1;
        }
        while current_page > chat_idx {
            actions.push(PtyAction::Send(LEFT_ARROW.to_vec()));
            current_page -= 1;
        }

        let question = &response.questions[chat_idx];
        actions.extend(chat_about_this_keystrokes(
            &question.options,
            question.multi_select,
        ));
    }
    // Phase 3: no ChatAboutThis — submit (if multiple questions)
    else if num_questions > 1 {
        while current_page < num_questions {
            actions.push(PtyAction::Send(RIGHT_ARROW.to_vec()));
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
        }
    }

    /// Spawn the Claude Code process. Returns an exit handle that completes
    /// when the process exits. If `session_id` is set, passes `--resume <id>`.
    pub fn start(&mut self) -> Result<tokio::task::JoinHandle<()>> {
        let env = [("AMUX_AGENT_ID", self.agent_id.to_string())];
        let args: Vec<String> = match self.session_id {
            Some(id) => vec!["--resume".to_string(), id.to_string()],
            None => vec![],
        };
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
    pub async fn send_input(&self, input: StructuredInput) -> Result<()> {
        let Some(pty) = &self.pty else {
            return Ok(());
        };
        let actions = match &input {
            StructuredInput::Claude(claude_input) => match claude_input {
                ClaudeStructuredInput::SubmitMessage { data } => submit_message_keystrokes(data),
                ClaudeStructuredInput::PermissionResponse(response) => {
                    tracing::info!(agent_id = %self.agent_id, ?response, "sending permission response");
                    permission_response_keystrokes(response)
                }
                ClaudeStructuredInput::AskUserQuestionResponse(response) => {
                    tracing::info!(agent_id = %self.agent_id, num_questions = response.questions.len(), "sending AskUserQuestion response");
                    ask_question_keystrokes(response)
                }
            },
        };
        execute_pty_actions(pty, &actions).await
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
                    .write(StructuredOutput::Claude(
                        ClaudeStructuredOutput::PermissionRequest {
                            tool: perm_req.tool,
                        },
                    ))
                    .await;
            }
            Hook::Claude(ClaudeHook::Stop(_)) => {
                tracing::debug!(agent_id = %self.agent_id, "agent stopped");
                log_source
                    .write(StructuredOutput::Claude(
                        ClaudeStructuredOutput::AgentStopped,
                    ))
                    .await;
            }
            Hook::Claude(ClaudeHook::Unknown) => {}
        }
        Ok(())
    }

    /// Subscribe to structured log output.
    pub async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        self.log_source.as_ref()?.subscribe().await
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
                markdown: None,
            })
            .collect()
    }

    fn make_preview_options(labels: &[&str]) -> Vec<AskUserQuestionOption> {
        labels
            .iter()
            .map(|l| AskUserQuestionOption {
                label: l.to_string(),
                description: String::new(),
                markdown: Some("```preview```".to_string()),
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
        // First option: no Down arrows, just delay + Enter
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
        // Page 0: "1", right→page 1, "2", right→submit, Enter
        assert_eq!(
            sends(&actions),
            vec![
                b"1".to_vec(),
                RIGHT_ARROW.to_vec(),
                b"2".to_vec(),
                RIGHT_ARROW.to_vec(),
                b"\r".to_vec(),
            ]
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
                b" ".to_vec(),       // toggle A (index 0)
                DOWN_ARROW.to_vec(), // 0→1
                DOWN_ARROW.to_vec(), // 1→2
                b" ".to_vec(),       // toggle C (index 2)
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
        assert_eq!(s[4], b" "); // toggle Other
        assert_eq!(s[5], b"extra"); // type custom text
        assert_eq!(s[6], UP_ARROW); // navigate away
        assert_eq!(s.len(), 7);
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
        // 2 options → ChatAboutThis index = 4 (num_options + 2), digit press
        assert_eq!(sends(&actions), vec![b"4".to_vec()]);
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
        // Page 0: select "1", right→page 1, digit "4" (ChatAboutThis), no submit
        assert_eq!(
            sends(&actions),
            vec![b"1".to_vec(), RIGHT_ARROW.to_vec(), b"4".to_vec(),]
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
        // Phase 1: skip Q1, navigate to Q2 (right), select "2"
        // Phase 2: navigate back to Q1 (left), digit "4" (ChatAboutThis)
        assert_eq!(
            sends(&actions),
            vec![
                RIGHT_ARROW.to_vec(),
                b"2".to_vec(),
                LEFT_ARROW.to_vec(),
                b"4".to_vec(),
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
        // Phase 1: Q1 select "1", right→Q2 (skip), right→Q3 select "2"
        // Phase 2: navigate back from page 2 to page 1 (left), digit "4"
        assert_eq!(
            sends(&actions),
            vec![
                b"1".to_vec(),
                RIGHT_ARROW.to_vec(),
                RIGHT_ARROW.to_vec(),
                b"2".to_vec(),
                LEFT_ARROW.to_vec(),
                b"4".to_vec(),
            ]
        );
    }
}
