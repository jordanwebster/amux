//! Specifications for AskUserQuestion over the SDK: every shape one call can
//! take, and the person dismissing the form.

use super::{HAIKU, SessionSetup, SpecDef, SpecSession};
use crate::driver::sdk::{AskUserQuestionConfig, PermissionMode, PreviewFormat, ToolConfig};
use crate::expect;

const COLOR: &str = "Which color do you prefer?";
const TOOLS: &str = "Which tools do you need?";
const LAYOUT: &str = "Which layout should the page use?";
const SNACK: &str = "Which snack do you want?";
const OTHER: &str = "Dried mango";

pub(super) static EVERY_SHAPE: SpecDef = SpecDef {
    name: "tools/question_every_shape",
    fixture: "question_every_shape",
    setup: every_shape_setup,
    run: |session| Box::pin(every_shape(session)),
};

fn every_shape_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Use exactly one AskUserQuestion call containing exactly four questions, in this order. \
         1: header Color, question 'Which color do you prefer?', single-select, options Red and Blue. \
         2: header Tools, question 'Which tools do you need?', multiSelect true, options Hammer, Saw and Drill. \
         3: header Layout, question 'Which layout should the page use?', single-select, four options Sidebar, Topbar, Grid and Stack; give every one of these four options a preview holding a small ASCII sketch of that layout. \
         4: header Snack, question 'Which snack do you want?', single-select, options Apple, Pretzel and Popcorn. \
         Add nothing else. After I answer, repeat all four answers.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.options.tool_config = Some(ToolConfig {
        ask_user_question: Some(AskUserQuestionConfig {
            preview_format: Some(PreviewFormat::Markdown),
        }),
    });
    setup.answer_questions(|question| {
        match question["header"].as_str().unwrap_or_default() {
            "Color" => "Blue",
            "Tools" => "Hammer, Drill",
            "Layout" => "Grid",
            // The free-text answer every question offers besides its options.
            _ => OTHER,
        }
        .to_owned()
    });
    setup
}

/// One call carries up to four questions, each with a header chip and two to
/// four options; a question is single- or multi-select, and single-select
/// options may carry previews. The answers go back as one map from question
/// to text: an option's label, several labels joined by ", ", or free text.
async fn every_shape(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(turn.succeeded(), "an answered form lets the turn finish");
    let asks = session
        .permission_requests
        .iter()
        .filter(|(tool, _)| tool == "AskUserQuestion")
        .map(|(_, input)| input)
        .collect::<Vec<_>>();
    expect!(asks.len() == 1, "the four questions arrive as one request");
    let questions = asks[0]["questions"].as_array().cloned().unwrap_or_default();
    let shape = questions
        .iter()
        .map(|question| {
            (
                question["header"].as_str().unwrap_or_default().to_owned(),
                question["options"].as_array().map_or(0, Vec::len),
                question["multiSelect"] == true,
            )
        })
        .collect::<Vec<_>>();
    expect!(
        shape
            == [
                ("Color".to_owned(), 2, false),
                ("Tools".to_owned(), 3, true),
                ("Layout".to_owned(), 4, false),
                ("Snack".to_owned(), 3, false),
            ],
        "the request carries each question's header, option count and selection mode: {shape:?}"
    );
    let previews = questions[2]["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|option| {
            option["preview"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        })
        .count();
    expect!(
        previews == 4,
        "every option of the layout question carries a preview: {previews}"
    );
    let results = turn.tool_results().join("\n");
    for (question, answer) in [
        (COLOR, "Blue"),
        (TOOLS, "Hammer, Drill"),
        (LAYOUT, "Grid"),
        (SNACK, OTHER),
    ] {
        expect!(
            results.contains(question) && results.contains(answer),
            "the tool result pairs {question:?} with {answer:?}: {results}"
        );
    }
}

pub(super) static DISMISSED: SpecDef = SpecDef {
    name: "tools/question_dismissed",
    fixture: "question_dismissed",
    setup: dismissed_setup,
    run: |session| Box::pin(dismissed(session)),
};

fn dismissed_setup() -> SessionSetup {
    let mut setup = SessionSetup::new(
        HAIKU,
        "Use AskUserQuestion to ask one single-select question with header Color, question \
         'Which color do you prefer?', and options Red and Blue.",
    );
    setup.options.permission_mode = Some(PermissionMode::Default);
    setup.dismiss_questions("The person dismissed the question.");
    setup
}

/// A person who dismisses the form denies the tool and stops the turn. Claude
/// then writes its own rejection into the tool result, the same text the
/// terminal shows, rather than the message the denial carried, and asks
/// nothing further.
async fn dismissed(session: &mut SpecSession) {
    let turn = session.turn().await;
    expect!(
        session.permission_request_count("AskUserQuestion") == 1,
        "the question arrived once and was not asked again"
    );
    expect!(
        turn.tools_used() == ["AskUserQuestion"],
        "nothing ran after the dismissal: {:?}",
        turn.tools_used()
    );
    expect!(
        turn.tool_results()
            .iter()
            .any(|result| result.contains("The user doesn't want to proceed with this tool use")),
        "the tool result is Claude's rejection: {:?}",
        turn.tool_results()
    );
}
