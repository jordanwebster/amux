//! Integration coverage for the protocol planes each provider backend exposes
//! and refuses. Value-level input dispatch is covered at the host boundary.

use agent_runtime::test_support::{create_sdk, open_in_process_plane};
use model::{AgentKind, ClaudeDriver, Protocol, ProtocolError};

async fn assert_not_exposed(kind: AgentKind, protocol: Protocol) {
    assert_eq!(
        open_in_process_plane(kind, protocol).await,
        Err(ProtocolError::NotExposed { kind, protocol })
    );
}

#[tokio::test]
async fn pty_claude_refuses_claude_sdk_protocol() {
    assert_not_exposed(
        AgentKind::Claude {
            driver: ClaudeDriver::Pty,
        },
        Protocol::ClaudeSdkV1,
    )
    .await;
}

#[tokio::test]
async fn sdk_claude_refuses_terminal_protocol() {
    assert_not_exposed(
        AgentKind::Claude {
            driver: ClaudeDriver::Sdk,
        },
        Protocol::TerminalV1,
    )
    .await;
}

#[tokio::test]
async fn claude_refuses_codex_protocol() {
    assert_not_exposed(
        AgentKind::Claude {
            driver: ClaudeDriver::Pty,
        },
        Protocol::CodexSdkV1,
    )
    .await;
}

#[tokio::test]
async fn every_exposed_provider_protocol_opens_in_process() {
    for (kind, protocol) in [
        (
            AgentKind::Claude {
                driver: ClaudeDriver::Pty,
            },
            Protocol::TerminalV1,
        ),
        (
            AgentKind::Claude {
                driver: ClaudeDriver::Pty,
            },
            Protocol::ClaudePtyTranscriptV1,
        ),
        (
            AgentKind::Claude {
                driver: ClaudeDriver::Sdk,
            },
            Protocol::ClaudeSdkV1,
        ),
        // Codex is hosted by a unix-only backend, so its protocol surface is
        // advertised everywhere but can only be opened in process where that
        // backend exists.
        #[cfg(unix)]
        (AgentKind::Codex, Protocol::CodexSdkV1),
    ] {
        open_in_process_plane(kind, protocol)
            .await
            .unwrap_or_else(|error| panic!("{kind} should expose {protocol}: {error}"));
    }
}

#[tokio::test]
async fn sdk_claude_create_constructs_the_provider_backend() {
    create_sdk().await.unwrap();
}
