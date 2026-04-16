use tokio::sync::mpsc;

use crate::protocol::message::{DirectMessage, Message, ProtocolError};
use crate::server::connection::{ConnectionContext, ConnectionError};

pub(super) async fn handle(
    tx: &mpsc::Sender<Message>,
    token: String,
    ctx: &ConnectionContext,
) -> crate::server::connection::Result<()> {
    let is_cloud = {
        let state = ctx.state.read().await;
        state.is_cloud_server
    };

    // Re-check minimum client version (config may have changed since connect)
    if let Some(ref name) = ctx.client_name {
        let min_version = {
            let state = ctx.state.read().await;
            state.config.minimum_client_versions.get(name).cloned()
        };
        if let Some(ref min_ver_str) = min_version {
            let cv = ctx.client_version.as_deref().unwrap_or("");
            let reject = match (
                semver::Version::parse(cv),
                semver::Version::parse(min_ver_str),
            ) {
                (Ok(client), Ok(minimum)) => client < minimum,
                _ => true,
            };
            if reject {
                let cv = cv.to_string();
                tracing::warn!(
                    client_name = %name,
                    client_version = %cv,
                    minimum_version = %min_ver_str,
                    "re-auth: client version below minimum"
                );
                let _ = tx
                    .send(Message::Direct {
                        message: DirectMessage::ReauthResult {
                            error: Some(ProtocolError::UpgradeRequired {
                                minimum_version: min_ver_str.clone(),
                                client_version: cv.clone(),
                            }),
                        },
                    })
                    .await;
                return Err(ConnectionError::UpgradeRequired {
                    minimum_version: min_ver_str.clone(),
                    client_version: cv,
                });
            }
        }
    }

    if is_cloud {
        let (validator, host, tcp_port) = {
            let state = ctx.state.read().await;
            let validator = state
                .jwt_validator
                .clone()
                .expect("is_cloud_server requires jwt_validator");
            let tcp_port = state.config.tcp_port.expect("cloud mode requires tcp_port");
            (validator, state.config.host_name.clone(), tcp_port)
        };

        match validator.validate(&token, &host, tcp_port).await {
            Ok(claims) => {
                let token_user_id = claims.sub.parse::<uuid::Uuid>().map_err(|_| {
                    tracing::error!(sub = %claims.sub, "re-auth invalid user_id");
                    ConnectionError::InvalidCredentials
                })?;
                if token_user_id != ctx.user_id {
                    tracing::error!("re-auth user_id mismatch");
                    let _ = tx
                        .send(Message::Direct {
                            message: DirectMessage::ReauthResult {
                                error: Some(ProtocolError::InvalidCredentials),
                            },
                        })
                        .await;
                    return Err(ConnectionError::InvalidCredentials);
                }
                tracing::debug!("re-authenticated");
            }
            Err(e) => {
                tracing::warn!(error = %e, "re-auth token validation failed");
                let _ = tx
                    .send(Message::Direct {
                        message: DirectMessage::ReauthResult {
                            error: Some(ProtocolError::InvalidCredentials),
                        },
                    })
                    .await;
                return Ok(());
            }
        }
    }

    let _ = tx
        .send(Message::Direct {
            message: DirectMessage::ReauthResult { error: None },
        })
        .await;
    Ok(())
}
