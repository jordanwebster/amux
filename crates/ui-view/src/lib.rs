//! Pure content values both amux clients compose. Every view is a function
//! of a [`ui_state::SessionState`] or [`ui_state::FleetState`] and the
//! client's explicit arguments (an order range, keys, an option struct, an
//! expansion set), never a size. Views carry typed facts; wording, geometry,
//! theme and animation belong to each renderer.
//!
//! Rows are items: the row id is the item key and never moves, and a run of
//! exploration calls is an attribute on its members, so paging and
//! streaming only ever add ids at the two edges.

mod ask;
mod composer;
mod fleet;
mod review;
mod rows;
mod segments;

pub use ask::{
    Answer, AskBody, AskCard, CardState, Choice, ChoiceOutcome, OptionView, Pick, QuestionView,
    Scope, ask_card, question_answer,
};
pub use composer::{
    CONTEXT_STRIP_PERCENT, ComposerView, ContextView, OutboxRow, OutboxState, QueuedRow,
    ServerView, SignInView, Strip, TasksView, UsageView, composer, composer_tokens, outbox_rows,
    queue_rows, session_strip, waiting,
};
pub use fleet::{FamilyHeader, FleetCard, FleetRow, family_header, fleet_card, fleet_list};
pub use review::{DiffLine, FileStatus, Hunk, LineKind, ReviewDoc, ReviewFile, review_doc};
pub use rows::{
    AskRow, ChatOptions, Decision, DecisionView, ExploreVerb, FileChangeView, FileRow,
    OUTPUT_HEAD_LINES, PlanVerdict, Row, RowKind, RunInfo, ToolRows, ToolStateView, chat_rows,
    chat_rows_for,
};
pub use segments::{AttachmentView, Segment, segments};
