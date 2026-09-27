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

/// A killed agent writes nothing more, so whatever was open stays open in
/// its state. The next incarnation rebuilds that state from the facts ring
/// (here small enough to have rotated, so from a checkpoint in the middle)
/// and starts by re-emitting every open item in full on its own key; the
/// keys it mints afterwards carry on from the last incarnation's.
/// A person's image reaches headless Claude as a native image block with
/// the blob's bytes, right after the attachment's element in the text.
#[tokio::test(flavor = "multi_thread")]
async fn headless_claude_gets_a_persons_image_as_an_image_block() {
    use sha2::Digest as _;

    let agent = Agent::start(Setup {
        steps: vec![
            Step::Text {
                chunks: vec!["a red square".into()],
            },
            Step::TurnEnd,
        ],
        ..Setup::sdk()
    })
    .await;
    let bytes = b"\x89PNG\r\n\x1a\n a tiny square".to_vec();
    let hash = sha2::Sha256::digest(&bytes).to_vec();
    // The daemon stores a person's blobs in the agent's directory by hash.
    let blob = agent.dir.join(agent::BLOBS).join(interpret::to_hex(&hash));
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, &bytes).unwrap();
    let image = wire::Attachment {
        of: Some(wire::attachment::Of::Image(wire::BlobRef {
            hash: hash.clone(),
            name: "square.png".into(),
            mime: "image/png".into(),
            size: bytes.len() as u64,
        })),
    };
    let input = wire::Input {
        input_id: b"p1".to_vec(),
        of: Some(wire::input::Of::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: format!("What colour is {} here?", attachments::PLACEHOLDER),
                attachments: vec![image.clone()],
            })),
        })),
    };
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.input(input).await, Verdict::Accepted);
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    let sent = agent
        .provider_input()
        .into_iter()
        .find(|line| line["type"] == "user")
        .expect("the prompt reached Claude");
    let element = attachments::element(&image, Some(&blob));
    assert_eq!(
        sent["message"]["content"],
        serde_json::json!([
            { "type": "text", "text": format!("What colour is {element}") },
            { "type": "image", "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": "iVBORw0KGgogYSB0aW55IHNxdWFyZQ==",
            } },
            { "type": "text", "text": " here?" },
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_from_the_checkpoint_re_emits_open_items_and_carries_keys_on() {
    let agent = Agent::start(Setup {
        steps: vec![
            Step::Text {
                chunks: vec!["one ".repeat(200)],
            },
            Step::Ask(provider_fakes::Ask::Permission(provider_fakes::Tool {
                name: None,
                class: provider_fakes::ToolClass::Consequential,
                input: None,
                outcome: provider_fakes::Outcome::default(),
                wait_for: None,
            })),
            Step::TurnEnd,
        ],
        ring_bytes: 4096,
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", "go").await, Verdict::Accepted);
    agent
        .wait("the ask opens", |log| {
            log.phase() == Some(wire::Phase::NeedsYou)
        })
        .await;
    let facts = agent.dir.join(agent::PRIVATE).join("facts");
    let segments = journal::segments(&facts).unwrap();
    assert_eq!(segments.len(), 2, "the ring rotated and kept two segments");
    for start in &segments {
        assert!(
            facts
                .join(format!("{}.checkpoint", journal::segment_name(*start)))
                .exists(),
            "each segment starts from a checkpoint"
        );
    }

    let before = agent.log();
    let full = before.full_items();
    daemon.stop(StopMode::Kill).await;
    assert_eq!(agent.exit().await, ExitCause::Killed);

    agent.resume();
    agent
        .wait("the killed incarnation's final boundary", |log| {
            log.boundaries()
                .iter()
                .any(|boundary| boundary == "EXITED ended unexpectedly")
        })
        .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    let after = agent.log();
    // The recovered final boundary, then the resume step.
    let resumed = &after.steps[before.steps.len() + 1];
    assert!(
        resumed.snapshot.is_some(),
        "the resume step carries the snapshot"
    );
    for item in &resumed.items {
        let known = full
            .get(&item.key)
            .unwrap_or_else(|| panic!("{} was emitted before the kill", item.key));
        assert_eq!(item, known, "{} is re-emitted in full", item.key);
    }
    assert_eq!(daemon.prompt(b"p2", "again").await, Verdict::Accepted);
    agent
        .wait("the resumed boundary", |log| {
            log.boundaries()
                .iter()
                .any(|boundary| boundary == "RESUMED")
        })
        .await;
    let after = agent.log();
    let boundaries: Vec<String> = after
        .items()
        .into_iter()
        .filter(|(_, item)| item.key.starts_with("boundary:"))
        .map(|(_, item)| item.key)
        .collect();
    let unique: std::collections::BTreeSet<&String> = boundaries.iter().collect();
    assert_eq!(
        unique.len(),
        boundaries.len(),
        "no boundary key is reused: {boundaries:?}"
    );
    daemon.stop(StopMode::Kill).await;
    assert_eq!(agent.exit().await, ExitCause::Killed);
}

/// Terminal Claude: the agent reads the version the binary reports,
/// resolves the keymap for it and records both on the boundary; the prompt
/// is typed through that keymap; hooks arrive through the hook binary on
/// private/hooks.sock and the transcript is followed from the path the
/// session start names.
#[test]
fn a_terminal_claude_agent_types_through_its_keymap_and_hears_its_hooks() {
    terminal_test(async {
        let agent = Agent::start(Setup {
            kind: "claude_pty",
            steps: vec![
                Step::Text {
                    chunks: vec!["hello from the terminal".into()],
                },
                Step::TurnEnd,
            ],
            ..Setup::sdk()
        })
        .await;
        let mut daemon = agent.dial().await;
        agent.ready().await;
        assert_eq!(daemon.prompt(b"p1", "say hello").await, Verdict::Accepted);
        agent
            .wait("the turn ends", |log| log.turn_ends() == 1)
            .await;
        let log = agent.log();
        assert!(log.has_text("hello from the terminal"));
        assert!(log.has_text("say hello"), "the typed prompt is reflected");
        assert_eq!(
            log.launches().first().map(String::as_str),
            Some("STARTED 2.1.283 claude-2.1"),
            "the boundary records the version and the keymap resolved for it"
        );
        daemon.stop(StopMode::Graceful).await;
        assert_eq!(agent.exit().await, ExitCause::Stopped);
    });
}

fn held_turn() -> Vec<Step> {
    vec![
        Step::Text {
            chunks: vec!["working".into()],
        },
        Step::WaitFor {
            path: agent_release(),
        },
        Step::TurnEnd,
    ]
}

/// A parent's message reaches a one-shot child through the provider's own
/// channel while its turn runs: terminal Claude's messaging socket (or a
/// paste when it has none), headless Claude's stdin, Codex's injected
/// items. The child counts it accepted but unconsumed until the provider's
/// record shows it taken, so the turn's end does not let the child exit
/// under it; once consumed and answered, the child finishes.
async fn a_message_mid_turn_is_consumed_before_a_one_shot_child_exits(
    kind: &'static str,
    socketless: bool,
) {
    let agent = Agent::start(Setup {
        kind,
        steps: held_turn(),
        initial_prompt: Some("start"),
        parent: true,
        socketless,
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent
        .wait("the first turn is running", |log| log.has_text("working"))
        .await;
    // Accepted, and queued behind the running turn where the provider
    // says so.
    let verdict = daemon.message(b"m1", "a note from the parent").await;
    assert!(
        matches!(verdict, Verdict::Accepted | Verdict::Queued),
        "{verdict:?}"
    );
    agent.release();
    assert_eq!(agent.exit().await, ExitCause::Finished);
    let log = agent.log();
    assert!(
        log.full_items()
            .contains_key(&interpret::agent_message_key(b"m1")),
        "the message has its item"
    );
    // The provider's own record of taking the message comes before the
    // agent's exit: headless Claude's reflection of it, terminal Claude's
    // user row for it. Codex drains an item injected mid-turn into that
    // turn, so the message is consumed at the inject's acknowledgement and
    // no turn is started for it.
    let facts = agent.facts();
    let exiting = facts
        .iter()
        .position(|entry| entry["event"] == "exiting")
        .expect("the exit is in the facts ring");
    let taken = |channel: &str, took: &dyn Fn(&serde_json::Value) -> bool| {
        facts[..exiting].iter().any(|entry| {
            entry["event"] == "fact"
                && entry["channel"] == channel
                && entry["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok())
                    .is_some_and(|fact: serde_json::Value| {
                        fact.to_string().contains("a note from the parent") && took(&fact)
                    })
        })
    };
    match kind {
        "claude_sdk" => assert!(
            taken("stream", &|line| line["type"] == "user"
                && line["isReplay"] == true),
            "Claude reflected the message before the exit"
        ),
        "claude_pty" => assert!(
            taken("transcript", &|row| row["type"] == "user"),
            "Claude's transcript shows the message taken before the exit"
        ),
        _ => assert_eq!(
            log.sequence(),
            ["boundary STARTED", "turn end", "boundary EXITED finished"],
            "the message was drained into the running turn"
        ),
    }
    if kind == "claude_pty" {
        assert_eq!(
            agent
                .terminal_bytes()
                .contains(r#"<cross-session-message from="amux">"#),
            socketless,
            "pasted exactly when there is no socket: {}",
            agent.terminal_bytes()
        );
    }
}

#[test]
fn terminal_claude_takes_an_agent_message_on_its_messaging_socket() {
    terminal_test(
        a_message_mid_turn_is_consumed_before_a_one_shot_child_exits("claude_pty", false),
    );
}

#[test]
fn terminal_claude_without_a_socket_has_the_message_pasted() {
    terminal_test(a_message_mid_turn_is_consumed_before_a_one_shot_child_exits("claude_pty", true));
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_claude_takes_an_agent_message_on_stdin() {
    a_message_mid_turn_is_consumed_before_a_one_shot_child_exits("claude_sdk", false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_takes_an_agent_message_as_injected_items() {
    a_message_mid_turn_is_consumed_before_a_one_shot_child_exits("codex", false).await;
}

// Strict replay: each fake plays a real recording from the claude-specs or
// codex-specs corpus (tests/replay/, its host lines rewritten to what the
// agent writes) and checks every byte the agent writes against it. An
// unexpected write, a missing one, or a write after the recording's end is
// reported in the provider's log and fails the test; a stdio provider
// reaching its end waits for the agent to close its input, which is the
// acknowledged end of file. Each test also checks the agent read the whole
// recording: the last thing the provider said is in the journal.

/// Answers the ask the snapshot holds open with `answer`, in the kind's
/// fixture answer syntax.
async fn answer(agent: &Agent, daemon: &mut Daemon, id: &[u8], answer: serde_json::Value) {
    agent
        .wait("an ask opens", |log| {
            log.phase() == Some(wire::Phase::NeedsYou)
        })
        .await;
    let ask = agent.log().ask_keys().remove(0);
    let input = interpret::FixtureInput::Answer { ask, answer };
    let input = match agent.kind() {
        "claude_pty" => interpret::claude_pty_input(id.to_vec(), &input),
        "codex" => interpret::codex_input(id.to_vec(), &input),
        _ => interpret::claude_sdk_input(id.to_vec(), &input),
    }
    .expect("an answer input");
    assert_eq!(daemon.input(input).await, Verdict::Accepted);
}

fn assert_no_drift(agent: &Agent) {
    assert_eq!(
        agent.provider_log(),
        "",
        "the agent wrote what the recording has"
    );
}

const SDK_SESSION: &str = "d7ec31b2-733c-4a1c-8f61-38a6b6d96b2b";
const SDK_PROMPT: &str = "Use AskUserQuestion to ask exactly one single-select question with header Color, question 'Which color do you prefer?', and options Red and Blue. Then repeat my answer.";

/// Headless Claude: the initialize exchange, a turn whose thinking, tool
/// call and permission request interleave, the answer as a control reply,
/// the turn's result, and the end of input when the agent stops.
#[tokio::test(flavor = "multi_thread")]
async fn headless_claude_replays_initialization_a_control_reply_and_the_end_of_input() {
    let agent = Agent::start(Setup {
        replay: Some("sdk_question"),
        session: Some(SDK_SESSION),
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", SDK_PROMPT).await, Verdict::Accepted);
    answer(
        &agent,
        &mut daemon,
        b"a1",
        serde_json::json!({"selected": [1]}),
    )
    .await;
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    assert!(agent.log().has_text("You selected **Blue**."));
    daemon.stop(StopMode::Graceful).await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
    assert_no_drift(&agent);
}

/// Headless Claude dies while its question is open: the agent records the
/// exit, closes the ask and ends.
#[tokio::test(flavor = "multi_thread")]
async fn headless_claude_replays_transport_loss_mid_turn() {
    let agent = Agent::start(Setup {
        replay: Some("sdk_lost"),
        session: Some(SDK_SESSION),
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", SDK_PROMPT).await, Verdict::Accepted);
    assert_eq!(agent.exit().await, ExitCause::ProviderExited(Some(1)));
    let log = agent.log();
    assert_eq!(
        log.boundaries().last().map(String::as_str),
        Some("EXITED exit code 1")
    );
    assert!(log.ask_keys().is_empty(), "the open ask is closed");
    assert_no_drift(&agent);
}

const CODEX_PROMPT: &str =
    "Run this exact shell command and no substitute: /usr/bin/touch <MACHINE_PATH> Then say DONE.";

/// Codex: the handshake, a turn whose deltas and items interleave, the
/// approval as the answer to the server's request, the turn's completion,
/// and the end of input when the agent stops.
#[tokio::test(flavor = "multi_thread")]
async fn codex_replays_the_handshake_an_approval_and_the_end_of_input() {
    let agent = Agent::start(Setup {
        kind: "codex",
        replay: Some("codex_approval"),
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", CODEX_PROMPT).await, Verdict::Accepted);
    answer(
        &agent,
        &mut daemon,
        b"a1",
        serde_json::json!({"decision": "approve"}),
    )
    .await;
    agent
        .wait("the turn ends", |log| log.turn_ends() == 1)
        .await;
    assert!(agent.log().has_text("DONE"));
    daemon.stop(StopMode::Graceful).await;
    assert_eq!(agent.exit().await, ExitCause::Stopped);
    assert_no_drift(&agent);
}

/// The Codex app server dies while its approval is open.
#[tokio::test(flavor = "multi_thread")]
async fn codex_replays_transport_loss_mid_turn() {
    let agent = Agent::start(Setup {
        kind: "codex",
        replay: Some("codex_lost"),
        ..Setup::sdk()
    })
    .await;
    let mut daemon = agent.dial().await;
    agent.ready().await;
    assert_eq!(daemon.prompt(b"p1", CODEX_PROMPT).await, Verdict::Accepted);
    assert_eq!(agent.exit().await, ExitCause::ProviderExited(Some(1)));
    let log = agent.log();
    assert_eq!(
        log.boundaries().last().map(String::as_str),
        Some("EXITED exit code 1")
    );
    assert!(log.ask_keys().is_empty(), "the open ask is closed");
    assert_no_drift(&agent);
}

const PTY_PROMPT: &str = "Use the Bash tool to run exactly: printf denied > denied.txt. Then stop.";

/// Terminal Claude: input goes live, the prompt is typed through the
/// keymap, the session starts, the permission menu is answered with a
/// denial and its feedback, the turn ends, and the session runs to the end
/// of the recording.
#[test]
fn terminal_claude_replays_a_prompt_a_denial_with_feedback_and_its_end() {
    terminal_test(async {
        let agent = Agent::start(Setup {
            kind: "claude_pty",
            replay: Some("pty_deny"),
            ..Setup::sdk()
        })
        .await;
        let mut daemon = agent.dial().await;
        agent.ready().await;
        assert_eq!(daemon.prompt(b"p1", PTY_PROMPT).await, Verdict::Accepted);
        answer(
            &agent,
            &mut daemon,
            b"a1",
            serde_json::json!({"deny": {"note": "Use a read-only command instead"}}),
        )
        .await;
        assert_eq!(agent.exit().await, ExitCause::ProviderExited(Some(0)));
        let log = agent.log();
        assert!(log.turn_ends() >= 1, "{:#?}", log.sequence());
        assert!(log.has_text("Use a read-only command instead"));
        assert_no_drift(&agent);
    });
}

/// Terminal Claude 2.1.283 in manual mode draws Yes / Yes, don't ask again
/// / Yes, and switch to auto mode / No. A deny with a note reaches No: the
/// call is refused, the session stays in manual mode and the note follows
/// as a prompt.
#[test]
fn terminal_claude_denies_on_the_menu_that_offers_auto_mode() {
    use prost::Message as _;
    use provider_fakes::{Ask, Outcome, Tool, ToolClass};
    terminal_test(async {
        let agent = Agent::start(Setup {
            kind: "claude_pty",
            offers_auto_mode: true,
            steps: vec![
                Step::Ask(Ask::Permission(Tool {
                    name: None,
                    class: ToolClass::Consequential,
                    input: Some(serde_json::json!({"command": "curl -s https://example.net"})),
                    outcome: Outcome {
                        output: "fetched".into(),
                        error: false,
                    },
                    wait_for: None,
                })),
                Step::Text {
                    chunks: vec!["never said".into()],
                },
                Step::TurnEnd,
                Step::Text {
                    chunks: vec!["Noted.".into()],
                },
                Step::TurnEnd,
            ],
            ..Setup::sdk()
        })
        .await;
        let mut daemon = agent.dial().await;
        agent.ready().await;
        assert_eq!(
            daemon.prompt(b"p1", "Fetch example.net").await,
            Verdict::Accepted
        );
        answer(
            &agent,
            &mut daemon,
            b"a1",
            serde_json::json!({"deny": {"note": "No network in this test"}}),
        )
        .await;
        agent
            .wait("the note's turn ends", |log| log.turn_ends() == 2)
            .await;
        let log = agent.log();
        // Each call as its newest revision has it.
        let tools = log
            .items()
            .into_iter()
            .filter_map(|(_, item)| {
                match wire::ClaudePtyItem::decode(item.body.as_slice())
                    .ok()?
                    .kind?
                {
                    wire::claude_pty_item::Kind::Tool(tool) => Some((item.key, tool)),
                    _ => None,
                }
            })
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_values()
            .collect::<Vec<_>>();
        let [tool] = tools.as_slice() else {
            panic!("one call: {tools:#?}");
        };
        assert_eq!(tool.state(), wire::ToolState::Denied, "{tool:#?}");
        let decision = tool.decision.clone().expect("a decision");
        assert_eq!(decision.outcome(), wire::DecisionOutcome::Denied);
        assert_eq!(decision.note, "No network in this test");
        assert!(!log.has_text("never said"), "the deny ended the turn");
        assert!(log.has_text("Noted."), "the note followed as a prompt");
        assert_ne!(log.permission_mode().as_deref(), Some("auto"));
        daemon.stop(StopMode::Graceful).await;
        assert_eq!(agent.exit().await, ExitCause::Stopped);
    });
}

/// Terminal Claude dies while its permission menu is open.
#[test]
fn terminal_claude_replays_transport_loss_mid_turn() {
    terminal_test(async {
        let agent = Agent::start(Setup {
            kind: "claude_pty",
            replay: Some("pty_lost"),
            ..Setup::sdk()
        })
        .await;
        let mut daemon = agent.dial().await;
        agent.ready().await;
        assert_eq!(daemon.prompt(b"p1", PTY_PROMPT).await, Verdict::Accepted);
        assert_eq!(agent.exit().await, ExitCause::ProviderExited(Some(1)));
        let log = agent.log();
        assert_eq!(
            log.boundaries().last().map(String::as_str),
            Some("EXITED exit code 1")
        );
        assert!(log.ask_keys().is_empty(), "the open ask is closed");
        assert_no_drift(&agent);
    });
}
