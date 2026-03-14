//! Claude Code agent session.
//!
//! Two-phase init: [`ClaudeSession::new`] stores metadata,
//! [`ClaudeSession::start`] spawns the PTY process. Hook handling and structured
//! input translation are encapsulated here.

use super::{PtyHandle, spawn_pty_agent};
use crate::buffer::MultiplexStructuredReader;
use crate::claude::structured_log_source::StructuredLogSource;
use crate::claude::types::{
    AskUserQuestionAnswer, AskUserQuestionResponse, ClaudeHook, ClaudeStructuredInput,
    ClaudeStructuredOutput, Hook, MultiSelectAnswer, PermissionResponse, SelectedOption,
    SingleSelectAnswer, StructuredInput, StructuredOutput,
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

fn single_select_keystrokes(answer: &SingleSelectAnswer) -> Vec<PtyAction> {
    match &answer.selected {
        SelectedOption::Predefined { index } => {
            vec![PtyAction::Send(index.to_string().into_bytes())]
        }
        SelectedOption::Custom { index, text } => {
            vec![
                PtyAction::Send(index.to_string().into_bytes()),
                PtyAction::Delay(DELAY),
                PtyAction::Send(text.as_bytes().to_vec()),
                PtyAction::Delay(DELAY),
                PtyAction::Send(b"\r".to_vec()),
            ]
        }
    }
}

fn multi_select_keystrokes(answer: &MultiSelectAnswer) -> Vec<PtyAction> {
    let mut actions = Vec::new();
    let mut cursor_pos: usize = 1;

    // Sort selections by index for efficient navigation
    let mut sorted: Vec<&SelectedOption> = answer.selected.iter().collect();
    sorted.sort_by_key(|opt| match opt {
        SelectedOption::Predefined { index } | SelectedOption::Custom { index, .. } => *index,
    });

    let mut last_was_custom = false;
    for opt in &sorted {
        let target = match opt {
            SelectedOption::Predefined { index } | SelectedOption::Custom { index, .. } => *index,
        };

        // Navigate to target
        for _ in cursor_pos..target {
            actions.push(PtyAction::Send(DOWN_ARROW.to_vec()));
        }
        for _ in target..cursor_pos {
            actions.push(PtyAction::Send(UP_ARROW.to_vec()));
        }
        cursor_pos = target;

        // Toggle
        actions.push(PtyAction::Send(b" ".to_vec()));

        // Custom text
        if let SelectedOption::Custom { text, .. } = opt {
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

fn chat_about_this_keystrokes(index: usize, multi_select: bool) -> Vec<PtyAction> {
    if multi_select {
        // Navigate from cursor position 1 to index, then press Enter
        let mut actions = Vec::new();
        for _ in 1..index {
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
/// `num_questions` is the total number of answered questions (= `answers.len()`).
/// For multi-question forms, right/left arrows navigate between pages and Enter
/// submits. ChatAboutThis ends the tool immediately (no submit).
fn ask_question_keystrokes(
    response: &AskUserQuestionResponse,
    num_questions: usize,
) -> Vec<PtyAction> {
    let mut actions = Vec::new();
    let mut current_page = 0;

    let chat_idx = response
        .answers
        .iter()
        .position(|a| matches!(a, AskUserQuestionAnswer::ChatAboutThis { .. }));

    // Phase 1: Process all non-ChatAboutThis answers
    for (i, answer) in response.answers.iter().enumerate() {
        if matches!(answer, AskUserQuestionAnswer::ChatAboutThis { .. }) {
            continue;
        }

        while current_page < i {
            actions.push(PtyAction::Send(RIGHT_ARROW.to_vec()));
            current_page += 1;
        }

        match answer {
            AskUserQuestionAnswer::SingleSelect(sa) => {
                actions.extend(single_select_keystrokes(sa));
            }
            AskUserQuestionAnswer::MultiSelect(ma) => {
                actions.extend(multi_select_keystrokes(ma));
            }
            AskUserQuestionAnswer::ChatAboutThis { .. } => unreachable!(),
        }
    }

    // Phase 2: ChatAboutThis present → navigate to it, select, done (no submit)
    if let Some(idx) = chat_idx {
        while current_page < idx {
            actions.push(PtyAction::Send(RIGHT_ARROW.to_vec()));
            current_page += 1;
        }
        while current_page > idx {
            actions.push(PtyAction::Send(LEFT_ARROW.to_vec()));
            current_page -= 1;
        }

        if let AskUserQuestionAnswer::ChatAboutThis {
            index,
            multi_select,
        } = &response.answers[idx]
        {
            actions.extend(chat_about_this_keystrokes(*index, *multi_select));
        }
    }
    // Phase 3: No ChatAboutThis → submit (if multiple questions)
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
                    let num_questions = response.answers.len();
                    tracing::info!(agent_id = %self.agent_id, num_questions, "sending AskUserQuestion response");
                    ask_question_keystrokes(response, num_questions)
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
    fn test_single_question_single_select_predefined() {
        let response = AskUserQuestionResponse {
            answers: vec![AskUserQuestionAnswer::SingleSelect(SingleSelectAnswer {
                selected: SelectedOption::Predefined { index: 2 },
            })],
        };
        let actions = ask_question_keystrokes(&response, 1);
        assert_eq!(sends(&actions), vec![b"2".to_vec()]);
    }

    #[test]
    fn test_single_question_single_select_custom() {
        let response = AskUserQuestionResponse {
            answers: vec![AskUserQuestionAnswer::SingleSelect(SingleSelectAnswer {
                selected: SelectedOption::Custom {
                    index: 3,
                    text: "hello".to_string(),
                },
            })],
        };
        let actions = ask_question_keystrokes(&response, 1);
        let s = sends(&actions);
        assert_eq!(s[0], b"3");
        assert_eq!(s[1], b"hello");
        assert_eq!(s[2], b"\r");
    }

    #[test]
    fn test_single_question_chat_about_this() {
        let response = AskUserQuestionResponse {
            answers: vec![AskUserQuestionAnswer::ChatAboutThis {
                index: 4,
                multi_select: false,
            }],
        };
        let actions = ask_question_keystrokes(&response, 1);
        assert_eq!(sends(&actions), vec![b"4".to_vec()]);
    }

    #[test]
    fn test_multi_question_right_arrows_and_submit() {
        let response = AskUserQuestionResponse {
            answers: vec![
                AskUserQuestionAnswer::SingleSelect(SingleSelectAnswer {
                    selected: SelectedOption::Predefined { index: 1 },
                }),
                AskUserQuestionAnswer::SingleSelect(SingleSelectAnswer {
                    selected: SelectedOption::Predefined { index: 2 },
                }),
            ],
        };
        let actions = ask_question_keystrokes(&response, 2);
        // Page 0: "1", right→page 1, "2", right→page 2 (submit), Enter
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
            answers: vec![AskUserQuestionAnswer::MultiSelect(MultiSelectAnswer {
                selected: vec![
                    SelectedOption::Predefined { index: 1 },
                    SelectedOption::Predefined { index: 3 },
                ],
            })],
        };
        let actions = ask_question_keystrokes(&response, 1);
        assert_eq!(
            sends(&actions),
            vec![
                b" ".to_vec(),       // toggle 1
                DOWN_ARROW.to_vec(), // 1→2
                DOWN_ARROW.to_vec(), // 2→3
                b" ".to_vec(),       // toggle 3
            ]
        );
    }

    #[test]
    fn test_multi_select_custom_navigate_away() {
        let response = AskUserQuestionResponse {
            answers: vec![AskUserQuestionAnswer::MultiSelect(MultiSelectAnswer {
                selected: vec![
                    SelectedOption::Predefined { index: 1 },
                    SelectedOption::Custom {
                        index: 4,
                        text: "extra".to_string(),
                    },
                ],
            })],
        };
        let actions = ask_question_keystrokes(&response, 1);
        let s = sends(&actions);
        assert_eq!(s[0], b" "); // toggle 1
        assert_eq!(s[1], DOWN_ARROW); // 1→2
        assert_eq!(s[2], DOWN_ARROW); // 2→3
        assert_eq!(s[3], DOWN_ARROW); // 3→4
        assert_eq!(s[4], b" "); // toggle 4
        assert_eq!(s[5], b"extra"); // type custom text
        assert_eq!(s[6], UP_ARROW); // navigate away
        assert_eq!(s.len(), 7);
    }

    #[test]
    fn test_chat_about_this_last_answer_no_submit() {
        let response = AskUserQuestionResponse {
            answers: vec![
                AskUserQuestionAnswer::SingleSelect(SingleSelectAnswer {
                    selected: SelectedOption::Predefined { index: 1 },
                }),
                AskUserQuestionAnswer::ChatAboutThis {
                    index: 3,
                    multi_select: false,
                },
            ],
        };
        let actions = ask_question_keystrokes(&response, 2);
        let s = sends(&actions);
        // Page 0: select "1", right to page 1, digit "3", no submit
        assert_eq!(s, vec![b"1".to_vec(), RIGHT_ARROW.to_vec(), b"3".to_vec(),]);
    }

    #[test]
    fn test_chat_about_this_not_last_navigate_back() {
        // Q0 is ChatAboutThis, Q1 answered normally — process Q1 first, navigate back
        let response = AskUserQuestionResponse {
            answers: vec![
                AskUserQuestionAnswer::ChatAboutThis {
                    index: 3,
                    multi_select: false,
                },
                AskUserQuestionAnswer::SingleSelect(SingleSelectAnswer {
                    selected: SelectedOption::Predefined { index: 2 },
                }),
            ],
        };
        let actions = ask_question_keystrokes(&response, 2);
        let s = sends(&actions);
        // Phase 1: skip Q0, navigate to Q1 (right), select "2"
        // Phase 2: navigate back to Q0 (left), digit "3"
        assert_eq!(
            s,
            vec![
                RIGHT_ARROW.to_vec(),
                b"2".to_vec(),
                LEFT_ARROW.to_vec(),
                b"3".to_vec(),
            ]
        );
    }

    #[test]
    fn test_multi_select_chat_about_this() {
        let response = AskUserQuestionResponse {
            answers: vec![AskUserQuestionAnswer::ChatAboutThis {
                index: 3,
                multi_select: true,
            }],
        };
        let actions = ask_question_keystrokes(&response, 1);
        assert_eq!(
            sends(&actions),
            vec![
                DOWN_ARROW.to_vec(), // 1→2
                DOWN_ARROW.to_vec(), // 2→3
                b"\r".to_vec(),      // select
            ]
        );
    }
}
