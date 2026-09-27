//! Every choice an ask card offers reaches the interpreter as an input it
//! accepts. Each kind's fixtures are driven through their interpreter and
//! committed into a session; wherever a card is open, every choice (and a
//! question card's picks) goes through `answer_input` to a copy of the
//! interpreter at that point, which must accept it and close the ask.

mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{Checkpoint, Effect, Event, Interpreter, decode_checkpoint, encode_checkpoint};
use support::Committer;
use ui_state::{InputOutcome, Msg, SessionState};
use ui_view::{Answer, AskBody, AskCard, CardState, Pick, answer_input, ask_card, question_answer};
use wire::{Kind, SessionEvent, send_input_response, session_event};

fn fixtures(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../interpret/fixtures")
        .join(kind)
}

fn agent(kind: Kind) -> wire::Agent {
    wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: kind as i32,
        lifecycle: wire::Lifecycle::Live as i32,
        phase: wire::Phase::NeedsYou as i32,
        incarnation: 1,
        ..wire::Agent::default()
    }
}

/// One interpreter and the session its emission builds, in step.
#[derive(Clone)]
struct Driven<S> {
    interpreter: S,
    session: SessionState,
    committer: Committer,
}

impl<S: Checkpoint + Clone> Driven<S> {
    fn start<I: Interpreter<State = S>>(
        kind: Kind,
        spec: &wire::AgentSpec,
        producer: &str,
    ) -> Self {
        let (interpreter, first) = I::initial(spec, producer);
        let mut driven = Driven {
            interpreter,
            session: SessionState::new(agent(kind)),
            committer: Committer::default(),
        };
        driven.commit(&first);
        let caught_up = driven.committer.revision;
        driven.session.update(Msg::Event(SessionEvent {
            of: Some(session_event::Of::CaughtUp(wire::CaughtUp {
                revision: caught_up,
            })),
        }));
        driven
    }

    fn commit(&mut self, step: &wire::Step) {
        for event in self.committer.commit(step) {
            self.session.update(Msg::Event(event));
        }
    }

    /// Feeds one event; the verdicts it replied.
    fn step<I: Interpreter<State = S>>(
        &mut self,
        event: Option<Event>,
    ) -> Vec<(Vec<u8>, wire::SendInputResponse)> {
        let Some(event) = event else {
            let bytes = encode_checkpoint(&self.interpreter);
            let decoded: S = decode_checkpoint(&bytes).expect("checkpoint decodes");
            let (resumed, step) = decoded.resume();
            self.interpreter = resumed;
            self.commit(&step);
            return Vec::new();
        };
        if let Event::Input(input) = &event {
            self.session.update(Msg::Send(input.clone()));
        }
        let stepped = I::step(&mut self.interpreter, event);
        self.commit(&stepped.step);
        let replies: Vec<_> = stepped
            .effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Reply { input_id, verdict } => Some((input_id, verdict)),
                _ => None,
            })
            .collect();
        for (id, verdict) in &replies {
            self.session
                .update(Msg::Sent(id.clone(), InputOutcome::Reply(verdict.clone())));
        }
        replies
    }
}

/// Every answer the card offers, with the note the person may add.
fn offered(card: &AskCard) -> Vec<(String, Answer, &'static str)> {
    let mut answers: Vec<_> = card
        .choices
        .iter()
        .map(|choice| {
            let note = if choice.takes_note {
                "use the fixture"
            } else {
                ""
            };
            (format!("{:?}", choice.outcome), choice.answer.clone(), note)
        })
        .collect();
    if let AskBody::Question(questions) = &card.body {
        let first: Vec<Pick> = questions
            .iter()
            .map(|question| match question.options.is_empty() {
                true => Pick::Other("typed".into()),
                false => Pick::Options(vec![0]),
            })
            .collect();
        answers.push((
            "first options".into(),
            question_answer(card, &first, ""),
            "",
        ));
        if questions.iter().all(|question| question.allow_other) {
            let typed: Vec<Pick> = questions
                .iter()
                .map(|_| Pick::Other("something else".into()))
                .collect();
            answers.push((
                "something else".into(),
                question_answer(card, &typed, "a note"),
                "",
            ));
        }
    }
    answers
}

fn body_name(body: &AskBody) -> &'static str {
    match body {
        AskBody::Command { .. } => "command",
        AskBody::Edit { .. } => "edit",
        AskBody::Tool { .. } => "tool",
        AskBody::Question(_) => "question",
        AskBody::Plan { .. } => "plan",
        AskBody::Form { .. } => "form",
        AskBody::Link { .. } => "link",
        AskBody::Access { .. } => "access",
        AskBody::Unanswerable { .. } => "unanswerable",
    }
}

/// Drives every fixture of `dir` and answers every open card every way it
/// offers; returns the card bodies it answered.
fn answer_all<I>(kind: Kind, dir: &str) -> BTreeSet<&'static str>
where
    I: Interpreter,
    I::State: Clone,
{
    let mut names: Vec<PathBuf> = std::fs::read_dir(fixtures(dir))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "json").then_some(path)
        })
        .collect();
    names.sort();
    let mut failures = Vec::new();
    let mut answered = BTreeSet::new();
    for path in &names {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let script = interpret::fixture_script::<I>(path).unwrap();
        let mut driven = Driven::start::<I>(kind, &script.spec, &script.producer);
        let mut tried = BTreeSet::new();
        for (label, event) in script.events {
            driven.step::<I>(event);
            let Some(card) = ask_card(&driven.session) else {
                continue;
            };
            if card.state != CardState::Open || !tried.insert(card.key.clone()) {
                continue;
            }
            for (choice, answer, note) in offered(&card) {
                let at = format!("{dir}/{name} after {label:.80}: {} {choice}", card.key);
                let Some(mut input) = answer_input(&card, &answer, note) else {
                    failures.push(format!("{at}: no input"));
                    continue;
                };
                input.input_id = format!("answer {choice}").into_bytes();
                let mut fork = driven.clone();
                let replies = fork.step::<I>(Some(Event::Input(input.clone())));
                let verdict = replies
                    .iter()
                    .find(|(id, _)| *id == input.input_id)
                    .and_then(|(_, verdict)| verdict.of.clone());
                match verdict {
                    Some(send_input_response::Of::Accepted(_)) => {}
                    other => {
                        failures.push(format!("{at}: not accepted: {other:?}"));
                        continue;
                    }
                }
                if fork
                    .session
                    .open_asks()
                    .iter()
                    .any(|ask| ask.key() == card.key)
                {
                    failures.push(format!("{at}: the ask is still open"));
                    continue;
                }
                answered.insert(body_name(&card.body));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    answered
}

#[test]
fn claude_pty_cards_answer_the_interpreter() {
    let answered = answer_all::<ClaudePty>(Kind::ClaudePty, "claude_pty");
    assert_eq!(
        answered,
        BTreeSet::from(["command", "edit", "plan", "question"]),
        "card bodies answered"
    );
}

#[test]
fn claude_sdk_cards_answer_the_interpreter() {
    let answered = answer_all::<ClaudeSdk>(Kind::ClaudeSdk, "claude_sdk");
    assert_eq!(
        answered,
        BTreeSet::from([
            "command", "edit", "form", "link", "plan", "question", "tool"
        ]),
        "card bodies answered"
    );
}

#[test]
fn codex_cards_answer_the_interpreter() {
    let answered = answer_all::<Codex>(Kind::Codex, "codex");
    assert_eq!(
        answered,
        BTreeSet::from([
            "access", "command", "edit", "form", "link", "question", "tool"
        ]),
        "card bodies answered"
    );
}
