//! Message dispatch handlers for the three protocol message categories.

mod command;
mod direct;
mod routable;
mod subscription;

use super::connection::ConnectionContext;
use crate::protocol::message::Message;
use tokio::sync::mpsc;

use command::handle_command;
use direct::handle_direct;
use routable::handle_routable;

pub(super) async fn handle_message(
    tx: &mpsc::Sender<Message>,
    msg: Message,
    ctx: &ConnectionContext,
) -> super::connection::Result<()> {
    match &msg {
        Message::Routable { .. } => tracing::trace!("received routable"),
        _ => tracing::debug!(msg_type = msg.type_label(), "received message"),
    }

    match msg {
        Message::Routable {
            src,
            dst,
            request_id,
            payload,
        } => handle_routable(tx, src, dst, request_id, payload, ctx).await,
        Message::Direct { message: direct } => handle_direct(tx, direct, ctx).await,
        Message::Command { command: cmd } => {
            if !ctx.is_local {
                tracing::warn!(cmd = cmd.type_label(), "rejecting command from remote peer");
                return Ok(());
            }
            handle_command(tx, cmd, ctx).await
        }
        Message::Unknown => {
            tracing::warn!("dropping unknown top-level message");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
