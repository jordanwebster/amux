use tokio::sync::mpsc;

use crate::agent::{AgentSession, ExternalHookBootstrap, HookOutcome, StopPolicy};
use crate::protocol::message::{Command, Message, ProtocolError};
use crate::server::accept::tcp_connect;
use crate::server::connection::ConnectionContext;
use crate::server::routing::{
    announce_agent_message, broadcast_to_peers, resume_agents, withdraw_agent,
};
use crate::server::{self, ShutdownRequest};

pub(super) async fn handle_command(
    tx: &mpsc::Sender<Message>,
    command: Command,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    match command {
        Command::Shutdown => {
            tracing::info!("shutdown requested");
            let shutdown_tx = {
                let state = ctx.state.read().await;
                state.shutdown_tx.clone()
            };
            let _ = shutdown_tx
                .send(ShutdownRequest::Shutdown {
                    reply: tx.clone(),
                    link: ctx.link.clone(),
                })
                .await;
            Ok(())
        }

        Command::ConnectToServer { address } => {
            // block_in_place + block_on breaks the async type recursion cycle:
            // handle_message -> tcp_connect -> connection_loop -> handle_message
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(tcp_connect(
                    &address,
                    &ctx.state,
                    &ctx.user_state,
                    ctx.user_id,
                    ctx.event_tx.clone(),
                ))
            });
            let response = match result {
                Ok(()) => Message::Command {
                    command: Command::ConnectToServerResult { error: None },
                },
                Err(e) => Message::Command {
                    command: Command::ConnectToServerResult {
                        error: Some(ProtocolError::ServerError {
                            message: e.to_string(),
                        }),
                    },
                },
            };
            let _ = tx.send(response).await;
            Ok(())
        }

        Command::Debug { verbose, format } => {
            let dump = server::debug::dump_server_debug_info(&ctx.state, format, verbose).await;
            let _ = tx
                .send(Message::Command {
                    command: Command::DebugResult { dump },
                })
                .await;
            Ok(())
        }

        Command::ListAgents => {
            let agents = {
                let us = ctx.user_state.read().await;
                us.registry.list_all(&us.hosts)
            };
            let _ = tx
                .send(Message::Command {
                    command: Command::ListAgentsResult {
                        agents: agents.into_iter().map(Into::into).collect(),
                    },
                })
                .await;
            Ok(())
        }

        Command::HandleHook {
            agent_id,
            provider,
            payload,
            external,
        } => {
            tracing::debug!(%agent_id, ?provider, external, "received hook event");

            let mut session_to_stop = None;
            let result = {
                let mut us = ctx.user_state.write().await;
                if let Some(session) = us.agents.get_mut(&agent_id) {
                    match session.handle_hook(provider, &payload).await {
                        Ok(HookOutcome::Noop | HookOutcome::KeepSession) => Ok(()),
                        Ok(HookOutcome::WithdrawSession) => {
                            session_to_stop = withdraw_agent(&mut us, agent_id);
                            Ok(())
                        }
                        Err(error) => Err(error.into_protocol_error()),
                    }
                } else if !external {
                    tracing::warn!(%agent_id, ?provider, "hook target not found");
                    Err(ProtocolError::NoAgentFound)
                } else {
                    match AgentSession::bootstrap_external_hook(agent_id, provider, &payload).await
                    {
                        Ok(ExternalHookBootstrap::Noop) => Ok(()),
                        Ok(ExternalHookBootstrap::Register(session)) => {
                            let host_id = {
                                let state = ctx.state.read().await;
                                state.host_id
                            };
                            let info = session.to_agent(host_id);
                            let announce = announce_agent_message(&info);
                            us.agents.insert(agent_id, session);
                            if let Err(e) = us.registry.register_local(info) {
                                us.agents.remove(&agent_id);
                                Err(ProtocolError::ServerError {
                                    message: format!(
                                        "failed to register readonly agent {agent_id}: {e}"
                                    ),
                                })
                            } else {
                                if let Some(session) = us.agents.get_mut(&agent_id) {
                                    session.maybe_start_name_sniffer(ctx.user_id, &ctx.event_tx);
                                }
                                broadcast_to_peers(&mut us, &announce, None);
                                tracing::info!(%agent_id, ?provider, "created readonly session from external hook");
                                Ok(())
                            }
                        }
                        Err(error) => Err(error.into_protocol_error()),
                    }
                }
            };

            if let Some(session) = session_to_stop {
                session.stop(StopPolicy::Interrupt).await;
            }

            let response = match result {
                Ok(()) => Message::Command {
                    command: Command::HandleHookResult { error: None },
                },
                Err(e) => Message::Command {
                    command: Command::HandleHookResult { error: Some(e) },
                },
            };
            let _ = tx.send(response).await;
            Ok(())
        }

        Command::ResolveAgent { identifier } => {
            let us = ctx.user_state.read().await;
            let agent = us.registry.resolve(&us.hosts, &identifier).map(Into::into);
            let _ = tx
                .send(Message::Command {
                    command: Command::ResolveAgentResult { agent },
                })
                .await;
            Ok(())
        }

        Command::Suspend => {
            tracing::info!("suspend requested");
            let shutdown_tx = {
                let state = ctx.state.read().await;
                state.shutdown_tx.clone()
            };
            let _ = shutdown_tx
                .send(ShutdownRequest::Suspend {
                    reply: tx.clone(),
                    link: ctx.link.clone(),
                })
                .await;
            Ok(())
        }

        Command::Resume => {
            tracing::info!("resume requested");
            let (state_path, host_id, is_cloud_server) = {
                let state = ctx.state.read().await;
                (
                    state.config.state_path.clone(),
                    state.host_id,
                    state.is_cloud_server,
                )
            };
            if is_cloud_server {
                let _ = tx
                    .send(Message::Command {
                        command: Command::ResumeResult {
                            resumed_count: 0,
                            failed_count: 0,
                            error: Some(ProtocolError::ServerError {
                                message: "cloud relays do not host local agents".to_string(),
                            }),
                        },
                    })
                    .await;
                return Ok(());
            }
            let suspended = match crate::suspend::load_and_remove_suspended(&state_path) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "failed to load suspended agents");
                    let _ = tx
                        .send(Message::Command {
                            command: Command::ResumeResult {
                                resumed_count: 0,
                                failed_count: 0,
                                error: Some(ProtocolError::ServerError {
                                    message: format!("failed to load state: {e}"),
                                }),
                            },
                        })
                        .await;
                    return Ok(());
                }
            };
            let (resumed_count, failed_count) = resume_agents(
                &ctx.user_state,
                &ctx.event_tx,
                ctx.user_id,
                suspended.agents,
                host_id,
            )
            .await;
            let _ = tx
                .send(Message::Command {
                    command: Command::ResumeResult {
                        resumed_count: resumed_count as u64,
                        failed_count: failed_count as u64,
                        error: None,
                    },
                })
                .await;
            Ok(())
        }

        // Response variants — should not arrive at the server
        Command::ListAgentsResult { .. }
        | Command::ResolveAgentResult { .. }
        | Command::ShutdownNotification { .. }
        | Command::DebugResult { .. }
        | Command::ConnectToServerResult { .. }
        | Command::HandleHookResult { .. }
        | Command::SuspendResult { .. }
        | Command::ResumeResult { .. } => {
            tracing::warn!("unexpected command response variant");
            Ok(())
        }
        Command::Unknown => {
            tracing::warn!("dropping unknown command");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;

    use chrono::Utc;
    use serde_json::json;
    use tokio::sync::{RwLock, mpsc, oneshot};
    use tokio::time::Instant;
    use uuid::Uuid;

    use super::super::tests::*;
    use super::*;
    use crate::agent::claude::hooks::{ClaudeHook, HookCommon};
    use crate::agent::{Agent, AgentSession, LocalAgentNameSource, SessionEvent};
    use crate::protocol::ProtocolError;
    use crate::protocol::link::Link;
    use crate::protocol::message::{Command, DirectMessage, HookProvider, Message, SubscriptionId};
    use crate::protocol::route::Route;
    use crate::server::{
        LOCAL_USER_ID, SUBSCRIPTION_LEASE_DURATION, ServerState, ServerUserState, SubscriptionMode,
    };

    async fn populate_debug_state(
        state: &Arc<RwLock<ServerState>>,
        user_state: &Arc<RwLock<ServerUserState>>,
    ) {
        let _term_rx = setup_named_route(user_state, "term-debug").await;
        let _peer_rx = add_peer_link(user_state, "peer-debug").await;

        let local_host_id = state.read().await.host_id;
        let local_agent_id = Uuid::new_v4();
        let mut local_session =
            AgentSession::new_readonly_claude(local_agent_id, PathBuf::from("/tmp/local-agent"));
        local_session.set_local_name(Some("local-agent".to_string()), LocalAgentNameSource::Amux);
        let local_info = local_session.to_agent(local_host_id);

        let remote_host_id = Uuid::new_v4();
        let remote_agent_id = Uuid::new_v4();
        let mut us = user_state.write().await;
        us.hosts.insert(
            remote_host_id,
            crate::protocol::message::Host {
                id: remote_host_id,
                name: "remote-host".to_string(),
                route: Route::from_link(Link::new("peer-debug").unwrap()),
                version: "9.9.9".to_string(),
            },
        );
        us.agents.insert(local_agent_id, local_session);
        us.registry.register_local(local_info).unwrap();
        us.registry
            .register_remote(Agent {
                id: remote_agent_id,
                host_id: remote_host_id,
                name: Some("remote-agent".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp/remote-agent"),
                route: Route::from_link(Link::new("peer-debug").unwrap()),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec!["--model".to_string(), "sonnet".to_string()],
                created_at: Utc::now(),
            })
            .unwrap();

        let (raw_cancel_tx, _raw_cancel_rx) = oneshot::channel();
        crate::server::connection::register_subscription(
            &mut us,
            SubscriptionId::random(),
            local_agent_id,
            SubscriptionMode::Raw,
            raw_cancel_tx,
            Route::from_link(Link::new("term-debug").unwrap()),
            Instant::now() + SUBSCRIPTION_LEASE_DURATION,
        );
    }

    #[tokio::test]
    async fn debug_yaml_dump_is_non_empty_and_parses() {
        let (state, user_state) = test_state().await;
        populate_debug_state(&state, &user_state).await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_command(
            &tx,
            Command::Debug {
                verbose: true,
                format: crate::protocol::message::DebugFormat::Yaml,
            },
            &ctx,
        )
        .await
        .unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        let Some(Message::Command {
            command: Command::DebugResult { dump },
        }) = msgs.first()
        else {
            panic!("expected DebugResult, got {:?}", msgs.first());
        };
        assert!(!dump.is_empty(), "yaml dump should be non-empty");
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(dump).expect("dump should be valid yaml");
        assert!(parsed.get("user_count").is_some());
        assert!(parsed.get("local_host").is_some());
    }

    #[tokio::test]
    async fn debug_json_dump_is_non_empty_and_parses() {
        let (state, user_state) = test_state().await;
        populate_debug_state(&state, &user_state).await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_command(
            &tx,
            Command::Debug {
                verbose: true,
                format: crate::protocol::message::DebugFormat::Json,
            },
            &ctx,
        )
        .await
        .unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        let Some(Message::Command {
            command: Command::DebugResult { dump },
        }) = msgs.first()
        else {
            panic!("expected DebugResult, got {:?}", msgs.first());
        };
        assert!(!dump.is_empty(), "json dump should be non-empty");
        let parsed: serde_json::Value =
            serde_json::from_str(dump).expect("dump should be valid json");
        assert!(parsed.get("user_count").is_some());
        assert!(parsed.get("local_host").is_some());
    }

    #[tokio::test]
    async fn resolve_agent_by_name() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_remote_agent(
            &user_state,
            Agent {
                id: agent_id,
                host_id: Uuid::new_v4(),
                name: Some("my-agent".to_string()),
                command: "claude".to_string(),
                working_dir: PathBuf::from("/tmp"),
                route: Route::from_link(Link::new("peer-a").unwrap()),
                agent_type: claude_agent_type(),
                structured_protocol: claude_structured_protocol(),
                readonly: false,
                args: vec![],
                created_at: Utc::now(),
            },
        )
        .await;

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_command(
            &tx,
            Command::ResolveAgent {
                identifier: "my-agent".to_string(),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::ResolveAgentResult { agent },
        } = &msgs[0]
        else {
            panic!("expected ResolveAgentResult, got {:?}", msgs[0]);
        };
        assert!(agent.is_some());
        assert_eq!(agent.as_ref().unwrap().id, agent_id);
    }

    #[tokio::test]
    async fn resolve_agent_not_found() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_command(
            &tx,
            Command::ResolveAgent {
                identifier: "nonexistent".to_string(),
            },
            &ctx,
        )
        .await
        .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::ResolveAgentResult { agent },
        } = &msgs[0]
        else {
            panic!("expected ResolveAgentResult, got {:?}", msgs[0]);
        };
        assert!(agent.is_none());
    }

    #[tokio::test]
    async fn resume_is_rejected_on_cloud_relay() {
        let (state, user_state) = test_state().await;
        {
            let mut s = state.write().await;
            s.is_cloud_server = true;
        }
        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        handle_command(&tx, Command::Resume, &ctx).await.unwrap();
        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command:
                Command::ResumeResult {
                    resumed_count,
                    failed_count,
                    error,
                },
        } = &msgs[0]
        else {
            panic!("expected ResumeResult, got {:?}", msgs[0]);
        };
        assert_eq!(*resumed_count, 0);
        assert_eq!(*failed_count, 0);
        assert!(matches!(
            error,
            Some(ProtocolError::ServerError { message })
                if message.contains("cloud relays do not host local agents")
        ));
    }

    #[tokio::test]
    async fn handle_hook_session_start_no_session_creates_readonly() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = ClaudeHook::SessionStart(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, true), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should create readonly session when hook has cwd/transcript_path: {:?}",
            error
        );
        let us = user_state.read().await;
        assert!(us.agents.contains_key(&agent_id));
    }

    #[tokio::test]
    async fn handle_hook_session_end_no_session_is_ignored() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = ClaudeHook::SessionEnd(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, true), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        {
            let msgs = written.lock().await;
            assert_eq!(msgs.len(), 1);
            let Message::Command {
                command: Command::HandleHookResult { error },
            } = &msgs[0]
            else {
                panic!("expected HandleHookResult, got {:?}", msgs[0]);
            };
            assert!(
                error.is_none(),
                "SessionEnd for an unknown session should be ignored: {:?}",
                error
            );
        }

        let us = user_state.read().await;
        assert!(
            !us.agents.contains_key(&agent_id),
            "unknown SessionEnd should not create a readonly session"
        );
    }

    #[tokio::test]
    async fn handle_hook_session_start_with_session_succeeds() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_local_claude(
            &user_state,
            agent_id,
            Some("hook-test"),
            LocalAgentNameSource::Amux,
        )
        .await;

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let hook = ClaudeHook::SessionStart(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/nonexistent_transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, true), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should succeed when session exists: {:?}",
            error
        );
    }

    #[tokio::test]
    async fn handle_hook_permission_request_no_session_creates_readonly() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = ClaudeHook::PermissionRequest(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, true), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should create readonly session when hook has cwd/transcript_path: {:?}",
            error
        );
        let us = user_state.read().await;
        assert!(us.agents.contains_key(&agent_id));
    }

    #[tokio::test]
    async fn handle_hook_permission_request_with_session_succeeds() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_local_claude(
            &user_state,
            agent_id,
            Some("hook-test"),
            LocalAgentNameSource::Amux,
        )
        .await;

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let hook = ClaudeHook::PermissionRequest(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, true), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should succeed when session exists: {:?}",
            error
        );
    }

    #[tokio::test]
    async fn handle_hook_stop_no_session_creates_readonly() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = ClaudeHook::Stop(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, true), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should create readonly session when hook has cwd/transcript_path: {:?}",
            error
        );
        let us = user_state.read().await;
        assert!(us.agents.contains_key(&agent_id));
    }

    #[tokio::test]
    async fn handle_hook_stop_with_session_succeeds() {
        let (state, user_state) = test_state().await;

        let agent_id = Uuid::new_v4();
        insert_local_claude(
            &user_state,
            agent_id,
            Some("hook-test"),
            LocalAgentNameSource::Amux,
        )
        .await;

        let ctx = test_ctx(state, user_state);
        let (tx, written) = mock_tx();

        let hook = ClaudeHook::Stop(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, true), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(
            error.is_none(),
            "should succeed when session exists: {:?}",
            error
        );
    }

    #[tokio::test]
    async fn handle_hook_session_end_with_readonly_session_withdraws_agent() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let session = AgentSession::new_readonly_claude(agent_id, PathBuf::from("/tmp"));
        let info = session.to_agent(Uuid::new_v4());
        {
            let mut us = user_state.write().await;
            us.agents.insert(agent_id, session);
            us.registry.register_local(info).unwrap();
        }

        let hook = ClaudeHook::SessionEnd(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, true), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        {
            let msgs = written.lock().await;
            assert_eq!(msgs.len(), 1);
            let Message::Command {
                command: Command::HandleHookResult { error },
            } = &msgs[0]
            else {
                panic!("expected HandleHookResult, got {:?}", msgs[0]);
            };
            assert!(
                error.is_none(),
                "SessionEnd for a readonly session should succeed: {:?}",
                error
            );
        }

        let us = user_state.read().await;
        assert!(
            !us.agents.contains_key(&agent_id),
            "readonly session should be withdrawn on SessionEnd"
        );
        assert!(
            us.registry.get(&agent_id).is_none(),
            "readonly session should be removed from registry on SessionEnd"
        );
    }

    #[tokio::test]
    async fn handle_hook_existing_session_with_wrong_provider_returns_error() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        insert_test_session(&user_state, agent_id).await;

        let hook = ClaudeHook::SessionStart(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, true), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let msgs = written.lock().await;
        assert_eq!(msgs.len(), 1);
        let Message::Command {
            command: Command::HandleHookResult { error },
        } = &msgs[0]
        else {
            panic!("expected HandleHookResult, got {:?}", msgs[0]);
        };
        assert!(matches!(
            error,
            Some(ProtocolError::ServerError { message })
                if message.contains("hook provider mismatch")
        ));
    }

    #[tokio::test]
    async fn handle_hook_unknown_session_with_external_false_returns_no_agent_found() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let hook = ClaudeHook::SessionStart(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, false), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        {
            let msgs = written.lock().await;
            assert_eq!(msgs.len(), 1);
            let Message::Command {
                command: Command::HandleHookResult { error },
            } = &msgs[0]
            else {
                panic!("expected HandleHookResult, got {:?}", msgs[0]);
            };
            assert_eq!(error, &Some(ProtocolError::NoAgentFound));
        }

        let us = user_state.read().await;
        assert!(!us.agents.contains_key(&agent_id));
    }

    #[tokio::test]
    async fn handle_hook_unknown_variant_is_acked_without_creating_session() {
        let (state, user_state) = test_state().await;
        let ctx = test_ctx(state, user_state.clone());
        let (tx, written) = mock_tx();

        let agent_id = Uuid::new_v4();
        let command = Command::HandleHook {
            agent_id,
            provider: HookProvider::Claude,
            payload: serde_json::to_vec(&json!({
                "hook_event_name": "SomeFutureEvent",
                "session_id": Uuid::new_v4(),
                "transcript_path": "/tmp/transcript.jsonl",
                "cwd": "/tmp"
            }))
            .unwrap(),
            external: true,
        };

        handle_command(&tx, command, &ctx).await.unwrap();

        tokio::task::yield_now().await;

        {
            let msgs = written.lock().await;
            assert_eq!(msgs.len(), 1);
            let Message::Command {
                command: Command::HandleHookResult { error },
            } = &msgs[0]
            else {
                panic!("expected HandleHookResult, got {:?}", msgs[0]);
            };
            assert!(error.is_none(), "unknown Claude hook variants should ack");
        }

        let us = user_state.read().await;
        assert!(
            !us.agents.contains_key(&agent_id),
            "unknown variants should not create readonly sessions"
        );
    }

    #[tokio::test]
    async fn readonly_external_claude_session_gets_name_updates() {
        let (state, user_state) = test_state().await;
        let (event_tx, mut event_rx) = mpsc::channel::<SessionEvent>(16);
        let ctx = ConnectionContext {
            state: state.clone(),
            user_state: user_state.clone(),
            user_id: LOCAL_USER_ID,
            event_tx,
            link: Link::new("test-link").unwrap(),
            is_local: true,
            heartbeat_role: crate::server::connection::HeartbeatRole::Disabled,
            next_request_id: Arc::new(AtomicU64::new(1)),
            client_name: Some("amux-cli".to_string()),
            client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        };
        let (tx, written) = mock_tx();
        let mut peer_rx = add_peer_link(&user_state, "peer-a").await;

        let agent_id = Uuid::new_v4();
        let hook = ClaudeHook::SessionStart(HookCommon {
            session_id: Uuid::new_v4(),
            transcript_path: "/tmp/transcript.jsonl".to_string(),
            cwd: "/tmp".to_string(),
        });

        handle_command(&tx, claude_hook_command(agent_id, hook, true), &ctx)
            .await
            .unwrap();

        tokio::task::yield_now().await;

        let log_source = {
            let us = user_state.read().await;
            us.agents
                .get(&agent_id)
                .and_then(|session| session.log_source())
                .expect("readonly session should have a log source")
        };

        log_source
            .write(json!({
                "type": "user",
                "message": {"content": "hello"},
                "uuid": "u1",
                "timestamp": "2026-04-03T10:00:00Z",
                "slug": "readonly-slug"
            }))
            .await;

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event,
            SessionEvent::NameCandidateChanged {
                agent_id: id,
                ref name,
                source: crate::agent::LocalAgentNameSource::ProviderSlug,
                ..
            } if id == agent_id && name == "readonly-slug"
        ));

        crate::server::handle_session_event(&state, event).await;

        {
            let msgs = written.lock().await;
            assert_eq!(msgs.len(), 1);
            let Message::Command {
                command: Command::HandleHookResult { error },
            } = &msgs[0]
            else {
                panic!("expected HandleHookResult, got {:?}", msgs[0]);
            };
            assert!(error.is_none());
        }

        {
            let us = user_state.read().await;
            let entry = us.registry.get(&agent_id).unwrap();
            assert_eq!(entry.name.as_deref(), Some("readonly-slug"));
            assert_eq!(
                us.agents.get(&agent_id).and_then(|session| session.name()),
                Some("readonly-slug")
            );
        }

        let initial = peer_rx
            .try_recv()
            .expect("readonly creation should announce unnamed agent");
        assert!(matches!(
            initial,
            Message::Direct {
                message: DirectMessage::AnnounceAgent {
                    agent_id: id,
                    name: None,
                    readonly: true,
                    ..
                }
            } if id == agent_id
        ));

        let renamed = peer_rx
            .try_recv()
            .expect("readonly rename should be re-announced");
        assert!(matches!(
            renamed,
            Message::Direct {
                message: DirectMessage::AnnounceAgent {
                    agent_id: id,
                    name: Some(name),
                    readonly: true,
                    ..
                }
            } if id == agent_id && name == "readonly-slug"
        ));
    }
}
