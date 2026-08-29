use serde::{Deserialize, Serialize};

/// The driver used to host a Claude agent.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeDriver {
    Pty,
    Sdk,
}

/// A closed description of an agent and its provider-specific driver.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentKind {
    Claude { driver: ClaudeDriver },
    Codex,
    TestAgent,
}

/// A protocol exposed by an agent session.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    TerminalV1,
    ClaudePtyTranscriptV1,
    ClaudeSdkV1,
    CodexSdkV1,
    TestEchoV1,
}

const CLAUDE_PTY_PROTOCOLS: &[Protocol] = &[Protocol::TerminalV1, Protocol::ClaudePtyTranscriptV1];
const CLAUDE_SDK_PROTOCOLS: &[Protocol] = &[Protocol::ClaudeSdkV1];
const CODEX_PROTOCOLS: &[Protocol] = &[Protocol::TerminalV1, Protocol::CodexSdkV1];
const TEST_AGENT_PROTOCOLS: &[Protocol] = &[Protocol::TerminalV1, Protocol::TestEchoV1];

impl AgentKind {
    /// Protocols are a property of the kind, never runtime advertisement.
    pub const fn protocols(&self) -> &'static [Protocol] {
        match self {
            Self::Claude {
                driver: ClaudeDriver::Pty,
            } => CLAUDE_PTY_PROTOCOLS,
            Self::Claude {
                driver: ClaudeDriver::Sdk,
            } => CLAUDE_SDK_PROTOCOLS,
            Self::Codex => CODEX_PROTOCOLS,
            Self::TestAgent => TEST_AGENT_PROTOCOLS,
        }
    }

    pub fn exposes(&self, protocol: Protocol) -> bool {
        self.protocols().contains(&protocol)
    }

    /// Provider label used by legacy surfaces that do not yet carry the kind.
    pub const fn provider(&self) -> &'static str {
        match self {
            Self::Claude { .. } => "claude",
            Self::Codex => "codex",
            Self::TestAgent => "test-agent",
        }
    }

    pub(crate) fn from_legacy(provider: &str, protocols: &[String]) -> Result<Self, String> {
        match provider {
            "claude" => Ok(Self::Claude {
                driver: if protocols
                    .iter()
                    .any(|protocol| protocol == Protocol::ClaudeSdkV1.as_str())
                {
                    ClaudeDriver::Sdk
                } else {
                    ClaudeDriver::Pty
                },
            }),
            "codex" => Ok(Self::Codex),
            "test-agent" => Ok(Self::TestAgent),
            other => Err(format!("unknown agent kind `{other}`")),
        }
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claude {
                driver: ClaudeDriver::Pty,
            } => formatter.write_str("claude/pty"),
            Self::Claude {
                driver: ClaudeDriver::Sdk,
            } => formatter.write_str("claude/sdk"),
            Self::Codex => formatter.write_str("codex"),
            Self::TestAgent => formatter.write_str("test-agent"),
        }
    }
}

impl Protocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TerminalV1 => "terminal_v1",
            Self::ClaudePtyTranscriptV1 => "claude_pty_transcript_v1",
            Self::ClaudeSdkV1 => "claude_sdk_v1",
            Self::CodexSdkV1 => "codex_sdk_v1",
            Self::TestEchoV1 => "test_echo_v1",
        }
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for Protocol {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "terminal_v1" => Ok(Self::TerminalV1),
            "claude_pty_transcript_v1" => Ok(Self::ClaudePtyTranscriptV1),
            "claude_sdk_v1" => Ok(Self::ClaudeSdkV1),
            "codex_sdk_v1" => Ok(Self::CodexSdkV1),
            "test_echo_v1" => Ok(Self::TestEchoV1),
            other => Err(format!("unknown session protocol `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_table_is_derived_for_every_kind() {
        let cases = [
            (
                AgentKind::Claude {
                    driver: ClaudeDriver::Pty,
                },
                &[Protocol::TerminalV1, Protocol::ClaudePtyTranscriptV1][..],
            ),
            (
                AgentKind::Claude {
                    driver: ClaudeDriver::Sdk,
                },
                &[Protocol::ClaudeSdkV1][..],
            ),
            (
                AgentKind::Codex,
                &[Protocol::TerminalV1, Protocol::CodexSdkV1][..],
            ),
            (
                AgentKind::TestAgent,
                &[Protocol::TerminalV1, Protocol::TestEchoV1][..],
            ),
        ];

        for (kind, expected) in cases {
            assert_eq!(kind.protocols(), expected, "{kind}");
            for protocol in [
                Protocol::TerminalV1,
                Protocol::ClaudePtyTranscriptV1,
                Protocol::ClaudeSdkV1,
                Protocol::CodexSdkV1,
                Protocol::TestEchoV1,
            ] {
                assert_eq!(
                    kind.exposes(protocol),
                    expected.contains(&protocol),
                    "{kind} and {protocol}"
                );
            }
        }
    }
}
