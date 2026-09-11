//! Claude PTY protocol values used by the provider runtime.

pub(crate) use model::{
    AskAnswer, ClaudePtyIntent as Intent, ClaudePtyTranscriptV1Args,
    ClaudePtyTranscriptV1Input, ClaudePtyTranscriptV1Output,
    ClaudePtyTranscriptV1ReplayQuery, PermissionAnswer, PlanAnswer,
};

pub const PTY_TRANSCRIPT_V1: &str = model::CLAUDE_PTY_TRANSCRIPT_V1;
