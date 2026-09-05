use super::*;
use crate::claude_io::{PermissionAnswer, PlanAnswer, QuestionAnswer, QuestionResponse};

fn prompt(text: &str) -> Intent {
    Intent::Prompt { text: text.into() }
}
fn markdown(text: &str) -> Step {
    Step::Markdown { text: text.into() }
}
fn script(reactions: Vec<(Trigger, Vec<Step>)>) -> Script {
    Script {
        reactions: reactions
            .into_iter()
            .map(|(on, play)| Reaction { on, play })
            .collect(),
        ..Script::default()
    }
}

#[test]
fn triggers_and_script_round_trip() {
    let inputs = [
        (Trigger::AnyPrompt, prompt("hello")),
        (
            Trigger::PromptContains("needle".into()),
            prompt("a needle here"),
        ),
        (
            Trigger::Command {
                name: "compact".into(),
            },
            prompt("/compact focus"),
        ),
        (Trigger::Interrupt, Intent::Interrupt),
        (Trigger::Any, Intent::CyclePermissionMode),
        (
            Trigger::Answer(AskKindMatch::Permission),
            Intent::Answer {
                ask_id: "ask".into(),
                answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
            },
        ),
        (
            Trigger::Answer(AskKindMatch::Question),
            Intent::Answer {
                ask_id: "ask".into(),
                answer: AskAnswer::Question(QuestionResponse { answers: vec![] }),
            },
        ),
        (
            Trigger::Answer(AskKindMatch::Plan),
            Intent::Answer {
                ask_id: "ask".into(),
                answer: AskAnswer::Plan(PlanAnswer::ApproveManual),
            },
        ),
    ];
    for (trigger, input) in inputs {
        let script = script(vec![
            (Trigger::PromptContains("never".into()), vec![]),
            (trigger, vec![markdown("matched")]),
        ]);
        let serialized = serde_json::to_string(&script).unwrap();
        assert_eq!(script, serde_json::from_str(&serialized).unwrap());
        let mut engine = Engine::new(script);
        engine.pending_ask = Some((AskId("ask".into()), AskKindMatch::Permission));
        assert_eq!(engine.feed(input).unwrap(), vec![markdown("matched")]);
        assert_eq!(engine.cursor, 2);
        assert_eq!(engine.feed(Intent::Interrupt), Err(ScriptError::Exhausted));
    }
    let command = Trigger::Command {
        name: "compact".into(),
    };
    assert!(!command.matches(&prompt("/compaction")));
    assert!(!command.matches(&prompt("please /compact")));
    assert!(!Trigger::AnyPrompt.matches(&Intent::Interrupt));
    assert!(!Trigger::Interrupt.matches(&prompt("interrupt")));
    assert!(!Trigger::PromptContains("yes".into()).matches(&prompt("no")));
}

#[test]
fn unknown_answers_are_observed_and_do_not_consume_a_reaction() {
    let mut engine = Engine::new(script(vec![(Trigger::Any, vec![Step::EndTurn])]));
    let answer = Intent::Answer {
        ask_id: "missing".into(),
        answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
    };
    assert_eq!(
        engine.feed(answer.clone()),
        Err(ScriptError::UnknownAsk(AskId("missing".into())))
    );
    engine.pending_ask = Some((AskId("known".into()), AskKindMatch::Permission));
    assert_eq!(
        engine.feed(answer),
        Err(ScriptError::UnknownAsk(AskId("missing".into())))
    );
    assert_eq!(engine.cursor, 0);
    assert_eq!(engine.observed().len(), 2);
    assert_eq!(engine.observed()[1].seq, 2);
    assert_eq!(engine.observed()[1].ask_id.as_deref(), Some("missing"));
    assert!(engine.observed()[1].answer.is_some());
}

async fn next(events: &mut claude::pty::EventStream) -> PtyEvent {
    tokio::time::timeout(Duration::from_secs(8), events.recv())
        .await
        .expect("session stalled")
        .expect("session closed early")
}

async fn collect(mut events: claude::pty::EventStream) -> Vec<PtyEvent> {
    let mut captured = Vec::new();
    loop {
        let event = next(&mut events).await;
        let exited = matches!(event, PtyEvent::Exited(_));
        captured.push(event);
        if exited {
            assert!(
                tokio::time::timeout(Duration::from_secs(1), events.recv())
                    .await
                    .unwrap()
                    .is_none(),
                "Exit must close even while Control and Provider remain held"
            );
            return captured;
        }
    }
}

fn transcript(events: &[PtyEvent]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match event {
            PtyEvent::Transcript { row, .. } => Some(row.as_value().clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn every_step_reaches_the_real_session_and_exit_closes_it() {
    let raw = json!({"type":"future-provider-row","payload":{"keep":[1,2,3]}});
    let steps = vec![
        Step::Rows {
            jsonl: vec![json!({"type":"user","message":{"role":"user","content":"raw row"}})],
        },
        markdown("# Hello\n\nA real transcript."),
        Step::Tool {
            name: "Bash".into(),
            input: json!({"command":"pwd"}),
            output: Some("/workspace".into()),
            denied: false,
        },
        Step::Tool {
            name: "Write".into(),
            input: json!({"file_path":"blocked"}),
            output: None,
            denied: true,
        },
        Step::Todo {
            items: vec![
                ("Read".into(), TodoState::Completed),
                ("Build".into(), TodoState::InProgress),
                ("Check".into(), TodoState::Pending),
            ],
        },
        Step::ChildStarted {
            name: "reviewer".into(),
        },
        Step::ChildFinished {
            name: "reviewer".into(),
        },
        Step::AgentMessage {
            from: "reviewer/host".into(),
            text: "Looks <good> & ready".into(),
        },
        Step::Working { secs: 0.01 },
        Step::Compaction,
        Step::ApiError {
            message: "rate limited".into(),
        },
        Step::Unknown { raw: raw.clone() },
        Step::EndTurn,
        Step::EndTurn,
        Step::Exit { code: 17 },
        markdown("unreachable"),
    ];
    let script = script(vec![(Trigger::AnyPrompt, steps)]);
    assert_eq!(
        script,
        serde_json::from_value(serde_json::to_value(&script).unwrap()).unwrap()
    );
    let (session, provider) = session(script).await.unwrap();
    let capture = tokio::spawn(collect(session.events));
    provider.feed(prompt("start")).unwrap();
    let events = capture.await.unwrap();
    let rows = transcript(&events);
    assert!(rows.contains(&raw));
    assert!(
        rows.iter()
            .any(|r| r.pointer("/message/content").and_then(Value::as_str) == Some("raw row"))
    );
    assert!(rows.iter().any(
        |r| r.pointer("/message/content/0/text").and_then(Value::as_str)
            == Some("# Hello\n\nA real transcript.")
    ));
    assert!(rows.iter().any(|r| {
        r.pointer("/message/content/0/content")
            .and_then(Value::as_str)
            == Some("/workspace")
    }));
    assert!(rows.iter().any(|r| r["toolDenialKind"] == "user_rejected"
        && r.pointer("/message/content/0/is_error") == Some(&json!(true))));
    let todos = rows
        .iter()
        .find_map(|r| r.pointer("/message/content/0/input/todos"))
        .unwrap();
    assert_eq!(todos[0]["status"], "completed");
    assert_eq!(todos[1]["status"], "in_progress");
    assert_eq!(todos[2]["status"], "pending");
    assert!(
        rows.iter()
            .any(|r| r.pointer("/origin/kind") == Some(&json!("task-notification")))
    );
    assert!(rows.iter().any(|r| {
        r.pointer("/message/content")
            .and_then(Value::as_str)
            .is_some_and(|s| {
                s.contains("from=\"reviewer/host\"") && s.contains("&lt;good&gt; &amp; ready")
            })
    }));
    assert!(rows.iter().any(|r| r["isApiErrorMessage"] == true));
    assert!(rows.iter().any(|r| r["subtype"] == "compact_boundary"));
    let turns: Vec<_> = rows
        .iter()
        .filter(|r| r["subtype"] == "turn_duration")
        .collect();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["durationMs"], 10);
    assert_eq!(turns[0]["messageCount"], 14);
    let hooks: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let PtyEvent::Hook(h) = e {
                Some(h.name())
            } else {
                None
            }
        })
        .collect();
    for name in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "SubagentStart",
        "SubagentStop",
        "Stop",
        "SessionEnd",
    ] {
        assert!(hooks.contains(&name), "missing {name}");
    }
    assert_eq!(hooks.iter().filter(|&&h| h == "Stop").count(), 1);
    assert!(matches!(events.last(),Some(PtyEvent::Exited(status)) if status.exit_code() == 17));
    assert_eq!(session.control.exit_status().unwrap().exit_code(), 17);
    assert!(!rows.iter().any(|r| r.to_string().contains("unreachable")));
    let stop = events
        .iter()
        .position(|e| matches!(e,PtyEvent::Hook(h) if h.name() == "Stop"))
        .unwrap();
    let unknown = events
        .iter()
        .position(|e| matches!(e,PtyEvent::Transcript {row,..} if row.as_value() == &raw))
        .unwrap();
    assert!(unknown < stop, "transcript ingestion must precede Stop");
    for event in &events {
        match event {
            PtyEvent::Transcript { row, .. } => {
                println!("{}", json!({"transcript":row.as_value()}))
            }
            PtyEvent::Hook(h) => println!("{}", json!({"hook":h.name()})),
            PtyEvent::Exited(s) => println!("{}", json!({"exit":s.exit_code()})),
            _ => {}
        }
    }
}

#[tokio::test]
async fn queued_prompts_are_observed_before_end_turn_and_play_once_in_order() {
    let (session, provider) = session(script(vec![
        (Trigger::AnyPrompt, vec![markdown("first started")]),
        (
            Trigger::PromptContains("second".into()),
            vec![markdown("second played"), Step::EndTurn, Step::EndTurn],
        ),
        (
            Trigger::AnyPrompt,
            vec![
                markdown("third played"),
                Step::EndTurn,
                Step::Exit { code: 0 },
            ],
        ),
    ]))
    .await
    .unwrap();
    let capture = tokio::spawn(collect(session.events));
    provider.feed(prompt("first")).unwrap();
    provider.play(vec![]).await.unwrap();
    provider.feed(prompt("second")).unwrap();
    provider.feed(prompt("third")).unwrap();
    assert_eq!(
        provider
            .observed()
            .iter()
            .map(|i| i.text.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["first", "second", "third"]
    );
    assert_eq!(provider.0.engine.lock().unwrap().cursor, 1);
    provider
        .play(vec![Step::EndTurn, Step::EndTurn])
        .await
        .unwrap();
    let events = capture.await.unwrap();
    let rows = transcript(&events);
    let mut sequence = Vec::new();
    for row in &rows {
        if row["type"] == "user"
            && let Some(text) = row.pointer("/message/content").and_then(Value::as_str)
        {
            sequence.push(text);
        }
        if row["subtype"] == "turn_duration" {
            sequence.push("end");
        }
    }
    assert_eq!(sequence, ["first", "end", "second", "end", "third", "end"]);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e,PtyEvent::Hook(h) if h.name() == "Stop"))
            .count(),
        3
    );
    assert_eq!(
        provider.observed().len(),
        3,
        "dequeue must not observe twice"
    );
}

#[tokio::test]
async fn asks_have_real_semantic_ids_and_answers_select_the_matching_reaction() {
    let asks = [
        (
            ScriptAsk::Permission {
                tool: "Bash".into(),
                invocation: json!({"command":"pwd"}),
                scoped_directories: vec!["/workspace".into()],
            },
            AskKindMatch::Permission,
            AskAnswer::Permission(PermissionAnswer::AllowOnce),
        ),
        (
            ScriptAsk::Question {
                questions: vec![QuestionSpec {
                    question: "Which?".into(),
                    header: "Choice".into(),
                    options: vec![QuestionOption {
                        label: "One".into(),
                        description: "First".into(),
                    }],
                    multi_select: true,
                }],
            },
            AskKindMatch::Question,
            AskAnswer::Question(QuestionResponse {
                answers: vec![QuestionAnswer {
                    selected: vec![0],
                    other: None,
                }],
            }),
        ),
        (
            ScriptAsk::Plan {
                markdown: "# Plan\n\nBuild it.".into(),
            },
            AskKindMatch::Plan,
            AskAnswer::Plan(PlanAnswer::ApproveManual),
        ),
    ];
    for (ask, kind, answer) in asks {
        let (session, provider) = session(script(vec![
            (Trigger::AnyPrompt, vec![Step::Ask(ask)]),
            (
                Trigger::Answer(kind),
                vec![markdown("answered"), Step::EndTurn, Step::Exit { code: 0 }],
            ),
        ]))
        .await
        .unwrap();
        provider.feed(prompt("ask me")).unwrap();
        // The empty control batch settles both the transcript-derived ask and its hook.
        provider.play(vec![]).await.unwrap();
        let facts = session.control.pending_asks();
        assert_eq!(facts.len(), 1);
        match (&facts[0].kind, kind) {
            (
                claude::pty::AskKind::Permission {
                    suggestions,
                    is_plan,
                    ..
                },
                AskKindMatch::Permission,
            ) => {
                assert_eq!(*suggestions, 1);
                assert!(!is_plan);
            }
            (claude::pty::AskKind::Permission { is_plan, .. }, AskKindMatch::Plan) => {
                assert!(is_plan)
            }
            (claude::pty::AskKind::Question { questions }, AskKindMatch::Question) => {
                assert_eq!(questions[0].options, 1);
                assert!(questions[0].multi_select);
            }
            other => panic!("unexpected facts {other:?}"),
        }
        let id = facts[0].id.0.clone();
        let input = Intent::Answer {
            ask_id: id.clone(),
            answer: answer.clone(),
        };
        let (writes, mut written) = mpsc::unbounded_channel();
        session.control.observe_writes(writes);
        session
            .control
            .send(serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap())
            .await
            .unwrap();
        assert!(
            written.try_recv().is_ok(),
            "the semantic answer must write to the PTY"
        );
        assert!(session.control.pending_asks().is_empty());
        provider.feed(input).unwrap();
        let events = collect(session.events).await;
        assert!(
            transcript(&events)
                .iter()
                .any(|r| r.pointer("/message/content/0/text") == Some(&json!("answered")))
        );
        assert_eq!(provider.observed()[1].ask_id.as_deref(), Some(id.as_str()));
        assert_eq!(
            provider.observed()[1].answer,
            Some(serde_json::to_value(answer).unwrap())
        );
        // Keep the real control alive through EOF; it must not hold the public stream open.
        assert_eq!(session.control.exit_status().unwrap().exit_code(), 0);
        println!(
            "{}",
            json!({"ask":{"id":id,"kind":facts[0].kind},"observed":provider.observed()})
        );
    }
}

#[tokio::test]
async fn dropping_provider_closes_the_session_and_removes_its_transcript() {
    let (mut session, provider) = session(Script::default()).await.unwrap();
    provider.play(vec![markdown("temporary")]).await.unwrap();
    let path = loop {
        if let PtyEvent::Transcript { path, .. } = next(&mut session.events).await {
            break path;
        }
    };
    assert!(path.exists());
    drop(provider);
    tokio::time::timeout(Duration::from_secs(2), async {
        while session.events.recv().await.is_some() {}
        while path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn testnet_agents_shutdown_releases_the_backend_provider_while_executor_lives() {
    let net = crate::testnet::TestNet::builder()
        .daemon("host")
        .start()
        .await;
    let (_, provider) = net
        .daemon("host")
        .spawn_scripted_agent("helper", std::env::temp_dir(), Script::default(), None)
        .await
        .unwrap();
    provider
        .play(vec![markdown("ingestion is live")])
        .await
        .unwrap();
    let weak = Arc::downgrade(&provider.0);
    drop(provider);
    assert!(
        weak.upgrade().is_some(),
        "the backend owns the live provider"
    );
    net.shutdown().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("daemon shutdown releases provider ownership without stopping the executor");
}
