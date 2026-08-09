//! Per-agent summarizers: pure folds deriving kernel vocabulary (attention)
//! from an agent's native structured stream.
//!
//! Summarize for chrome; never normalize content. Each agent type gets its
//! own fold over its own protocol; the asymmetry between agents is
//! expressed, not papered over. Folds are pure and depend on nothing in the
//! runtime, so *where* one executes is a deployment decision — future push
//! senders reuse them unchanged.

pub mod claude;

use serde::{Deserialize, Serialize};

use crate::model::Attention;

/// Typed per-agent summarizer state, carried on the `AgentCard`. There is no
/// generic intermediate representation: unknown agent types simply have no
/// summarizer and stay `Unknown`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "summarizer", rename_all = "snake_case")]
pub enum SummarizerState {
    Claude(claude::ClaudeSummarizer),
}

impl SummarizerState {
    pub fn attention(&self) -> Attention {
        match self {
            SummarizerState::Claude(fold) => fold.attention(),
        }
    }
}
