use serde::{Deserialize, Serialize};

use crate::suspend::SuspendedLocalAgentNameSource;

/// Local-only provenance for an agent session's current display name.
///
/// This is used to decide whether provider-derived candidates may rename a
/// local session. It is never sent over the peer protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalAgentNameSource {
    Unset,
    Amux,
    ProviderName,
    ProviderSlug,
}

impl LocalAgentNameSource {
    /// Whether this source can be overridden by automatic name discovery.
    pub(crate) fn is_automatic(self) -> bool {
        !matches!(self, Self::Amux)
    }

    /// Precedence rank among automatic sources. Higher wins.
    /// Only valid for automatic sources — panics for `Amux`.
    pub(crate) fn rank(self) -> u8 {
        match self {
            Self::Unset => 0,
            Self::ProviderSlug => 1,
            Self::ProviderName => 2,
            Self::Amux => unreachable!("check is_automatic() before calling rank()"),
        }
    }
}

impl From<LocalAgentNameSource> for SuspendedLocalAgentNameSource {
    fn from(source: LocalAgentNameSource) -> Self {
        match source {
            LocalAgentNameSource::Unset => Self::Unset,
            LocalAgentNameSource::Amux => Self::Amux,
            LocalAgentNameSource::ProviderName => Self::ProviderName,
            LocalAgentNameSource::ProviderSlug => Self::ProviderSlug,
        }
    }
}

impl From<SuspendedLocalAgentNameSource> for LocalAgentNameSource {
    fn from(source: SuspendedLocalAgentNameSource) -> Self {
        match source {
            SuspendedLocalAgentNameSource::Unset => Self::Unset,
            SuspendedLocalAgentNameSource::Amux => Self::Amux,
            SuspendedLocalAgentNameSource::ProviderName => Self::ProviderName,
            SuspendedLocalAgentNameSource::ProviderSlug => Self::ProviderSlug,
        }
    }
}
