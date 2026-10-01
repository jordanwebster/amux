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
mod plan;
mod review;
mod rows;
mod segments;
mod settings;
mod stretch;

pub use ask::{
    Answer, AskBody, AskCard, CardState, Choice, ChoiceOutcome, OptionView, Pick, PlanStep,
    QuestionView, Scope, answer_input, ask_card, question_answer, with_form_content,
};
pub use composer::{
    CONTEXT_STRIP_PERCENT, ComposerView, ContextView, JobView, OutboxRow, OutboxState, QueuedRow,
    ServerView, SignInView, Strip, TaskLine, TaskMark, TasksView, UsageView, UsageWindowView,
    background_jobs, composer, composer_tokens, outbox_rows, queue_rows, session_strip, waiting,
};
pub use fleet::{
    Away, FamilyHeader, FleetCard, FleetRow, away, family_header, fleet_card, fleet_list,
    signed_out,
};
pub use plan::{IMPLEMENT_PLAN, composed, plan_inputs};
pub use review::{DiffLine, FileStatus, Hunk, LineKind, ReviewDoc, ReviewFile, review_doc};
pub use rows::{
    AnswerView, AskRow, ChatOptions, Decision, DecisionView, ExploreVerb, FileChangeView, FileRow,
    Granted, OUTPUT_HEAD_LINES, PatchHead, PatchLine, PlanVerdict, Resolution, Row, RowKind,
    RunInfo, ToolRows, ToolStateView, chat_rows, chat_rows_for, patch_head, run_subjects,
};
pub use segments::{AttachmentView, Segment, segments};
pub use settings::{
    CommandView, EffortChoice, ModeChoice, ModeValue, ModelChoice, SettingChange, SettingsView,
    setting_input, settings,
};
pub use stretch::{Stretch, StretchCounts, stretch_at, stretch_steps};
