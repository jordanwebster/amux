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
mod overview;
mod plan;
mod review;
mod rows;
mod segments;
mod settings;
mod stretch;

pub use ask::{
    Answer, AskBody, AskCard, CardState, Choice, ChoiceOutcome, OptionView, Pick, QuestionView,
    Scope, answer_input, ask_card, question_answer, with_form_content,
};
pub use composer::{
    CONTEXT_NEAR_FULL_PERCENT, ComposerView, ContextView, OutboxRow, OutboxState, QueuedRow,
    SignInView, composer, composer_tokens, context, outbox_rows, queue_rows, sign_in, waiting,
};
pub use fleet::{
    ActivityLine, AskSubject, AskSummary, Away, ExitCause, FamilyHeader, FleetCard, FleetRow,
    FleetSection, FleetView, LoudMember, SecondLine, SectionKind, SessionLine, StuckReason, away,
    family_header, fleet_card, fleet_view, session_line, signed_out,
};
pub use overview::{
    ChangeTotals, ChangedFile, Changes, Comparison, Folder, JobRow, Overview, ServerView, TaskLine,
    TaskMark, TasksView, UsageLabel, UsageView, UsageWindowView, changes, diff_base, overview,
    usage_windows,
};
pub use review::{DiffLine, FileStatus, Hunk, LineKind, ReviewDoc, ReviewFile, review_doc};
pub use rows::{
    AnswerView, AskRow, ChatOptions, Decision, DecisionView, ExploreVerb, FileChangeView, FileRow,
    Granted, OUTPUT_HEAD_LINES, PatchHead, PatchLine, PlanVerdict, Resolution, Row, RowKind,
    RunInfo, ToolRows, ToolStateView, chat_rows, chat_rows_for, patch_head, run_subjects,
};
pub use segments::{AttachmentView, Segment, segments};
pub use settings::{
    CommandView, EffortChoice, ModeChoice, ModeValue, ModelChoice, SettingChange, SettingsView,
    effort_in_force, setting_input, settings,
};
pub use stretch::{Stretch, StretchCounts, stretch_at, stretch_steps};
