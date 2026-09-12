//! Names and stable ordering for deterministic TUI render states.

use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedState {
    ClaudeIdle,
    ClaudeWorking,
    ClaudePermissionAsk,
    ClaudeQuestionAsk,
    ClaudePlanReader,
    ClaudeDiffReader,
    ClaudeSdkIdle,
    ClaudeSdkStreaming,
    ClaudeSdkPermissionAsk,
    ClaudeSdkPlanReader,
    ClaudeSdkQuestionAsk,
    ClaudeSdkElicitation,
    ClaudeSdkDialog,
    ClaudeSdkTasks,
    ClaudeSdkExploration,
    ClaudeSdkExplorationExpanded,
    ClaudeSdkContext,
    ClaudeSdkContextBreakdown,
    ClaudeSdkLongFeed,
    ClaudeSdkScrolledBack,
    CodexIdle,
    CodexWorking,
    CodexApproval,
    CodexNetworkPolicy,
    CodexMcpStartup,
    HelpOverlay,
    Fleet,
    FleetEmpty,
    FleetMixed,
    FleetSdkHelp,
    A2aSdkFamily,
    ProfileSwitcher,
    FleetSwitched,
    ClaudeLongFeed,
    CodexLongFeed,
    ClaudeScrolledBack,
    CodexScrolledBack,
    ComponentGallery,
    ComponentGalleryCodex,
    ExplorationCollapsed,
    ExplorationExpanded,
    ChatAttachmentBlocks,
    ChatMixedDraft,
    ReviewOpen,
    ReviewSelection,
    ReviewCommentBox,
    ReviewThreads,
    ReviewFileList,
    ReviewFolded,
    ReviewBranchBase,
    ChatReviewToken,
}

const ALL_STATES: &[NamedState] = &[
    NamedState::ClaudeIdle,
    NamedState::ClaudeWorking,
    NamedState::ClaudePermissionAsk,
    NamedState::ClaudeQuestionAsk,
    NamedState::ClaudePlanReader,
    NamedState::ClaudeDiffReader,
    NamedState::ClaudeSdkIdle,
    NamedState::ClaudeSdkStreaming,
    NamedState::ClaudeSdkPermissionAsk,
    NamedState::ClaudeSdkPlanReader,
    NamedState::ClaudeSdkQuestionAsk,
    NamedState::ClaudeSdkElicitation,
    NamedState::ClaudeSdkDialog,
    NamedState::ClaudeSdkTasks,
    NamedState::ClaudeSdkExploration,
    NamedState::ClaudeSdkExplorationExpanded,
    NamedState::ClaudeSdkContext,
    NamedState::ClaudeSdkContextBreakdown,
    NamedState::ClaudeSdkLongFeed,
    NamedState::ClaudeSdkScrolledBack,
    NamedState::CodexIdle,
    NamedState::CodexWorking,
    NamedState::CodexApproval,
    NamedState::CodexNetworkPolicy,
    NamedState::CodexMcpStartup,
    NamedState::HelpOverlay,
    NamedState::Fleet,
    NamedState::FleetEmpty,
    NamedState::FleetMixed,
    NamedState::FleetSdkHelp,
    NamedState::A2aSdkFamily,
    NamedState::ProfileSwitcher,
    NamedState::FleetSwitched,
    NamedState::ClaudeLongFeed,
    NamedState::CodexLongFeed,
    NamedState::ClaudeScrolledBack,
    NamedState::CodexScrolledBack,
    NamedState::ComponentGallery,
    NamedState::ComponentGalleryCodex,
    NamedState::ExplorationCollapsed,
    NamedState::ExplorationExpanded,
    NamedState::ChatAttachmentBlocks,
    NamedState::ChatMixedDraft,
    NamedState::ReviewOpen,
    NamedState::ReviewSelection,
    NamedState::ReviewCommentBox,
    NamedState::ReviewThreads,
    NamedState::ReviewFileList,
    NamedState::ReviewFolded,
    NamedState::ReviewBranchBase,
    NamedState::ChatReviewToken,
];

impl NamedState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ClaudeIdle => "claude-idle",
            Self::ClaudeWorking => "claude-working",
            Self::ClaudePermissionAsk => "claude-permission-ask",
            Self::ClaudeQuestionAsk => "claude-question-ask",
            Self::ClaudePlanReader => "claude-plan-reader",
            Self::ClaudeDiffReader => "claude-diff-reader",
            Self::ClaudeSdkIdle => "claude-sdk-idle",
            Self::ClaudeSdkStreaming => "claude-sdk-streaming",
            Self::ClaudeSdkPermissionAsk => "claude-sdk-permission-ask",
            Self::ClaudeSdkPlanReader => "claude-sdk-plan-reader",
            Self::ClaudeSdkQuestionAsk => "claude-sdk-question-ask",
            Self::ClaudeSdkElicitation => "claude-sdk-elicitation",
            Self::ClaudeSdkDialog => "claude-sdk-dialog",
            Self::ClaudeSdkTasks => "claude-sdk-tasks",
            Self::ClaudeSdkExploration => "claude-sdk-exploration",
            Self::ClaudeSdkExplorationExpanded => "claude-sdk-exploration-expanded",
            Self::ClaudeSdkContext => "claude-sdk-context",
            Self::ClaudeSdkContextBreakdown => "claude-sdk-context-breakdown",
            Self::ClaudeSdkLongFeed => "claude-sdk-long-feed",
            Self::ClaudeSdkScrolledBack => "claude-sdk-scrolled-back",
            Self::CodexIdle => "codex-idle",
            Self::CodexWorking => "codex-working",
            Self::CodexApproval => "codex-approval",
            Self::CodexNetworkPolicy => "codex-network-policy",
            Self::CodexMcpStartup => "codex-mcp-startup",
            Self::HelpOverlay => "help-overlay",
            Self::Fleet => "fleet",
            Self::FleetEmpty => "fleet-empty",
            Self::FleetMixed => "fleet-mixed",
            Self::FleetSdkHelp => "fleet-sdk-help",
            Self::A2aSdkFamily => "a2a-sdk-family",
            Self::ProfileSwitcher => "profile-switcher",
            Self::FleetSwitched => "fleet-switched",
            Self::ClaudeLongFeed => "claude-long-feed",
            Self::CodexLongFeed => "codex-long-feed",
            Self::ClaudeScrolledBack => "claude-scrolled-back",
            Self::CodexScrolledBack => "codex-scrolled-back",
            Self::ComponentGallery => "component-gallery",
            Self::ComponentGalleryCodex => "component-gallery-codex",
            Self::ExplorationCollapsed => "exploration-collapsed",
            Self::ExplorationExpanded => "exploration-expanded",
            Self::ChatAttachmentBlocks => "chat-attachment-blocks",
            Self::ChatMixedDraft => "chat-mixed-draft",
            Self::ReviewOpen => "review-open",
            Self::ReviewSelection => "review-selection",
            Self::ReviewCommentBox => "review-comment-box",
            Self::ReviewThreads => "review-threads",
            Self::ReviewFileList => "review-file-list",
            Self::ReviewFolded => "review-folded",
            Self::ReviewBranchBase => "review-branch-base",
            Self::ChatReviewToken => "chat-review-token",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        all_states()
            .iter()
            .copied()
            .find(|state| state.name() == name)
    }
}

impl fmt::Display for NamedState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Error returned when a screenshot subject is not in the named registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownNamedState(pub String);

impl fmt::Display for UnknownNamedState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown TUI state `{}`", self.0)
    }
}

impl std::error::Error for UnknownNamedState {}

impl FromStr for NamedState {
    type Err = UnknownNamedState;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::parse(name).ok_or_else(|| UnknownNamedState(name.to_string()))
    }
}

/// The ordered registry used by screenshot listing and exhaustive tests.
pub const fn all_states() -> &'static [NamedState] {
    ALL_STATES
}
