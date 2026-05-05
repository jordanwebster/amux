use std::sync::Arc;

use tokio::sync::RwLock;

use crate::protocol::message::DebugFormat;
use crate::server::ServerState;

pub(crate) struct AdminService;

#[derive(Clone)]
pub(crate) struct AdminServiceCtx {
    state: Arc<RwLock<ServerState>>,
}

impl AdminServiceCtx {
    pub(crate) fn new(state: Arc<RwLock<ServerState>>) -> Self {
        Self { state }
    }

    fn state(&self) -> &Arc<RwLock<ServerState>> {
        &self.state
    }
}

impl AdminService {
    pub(crate) async fn debug(ctx: &AdminServiceCtx, format: DebugFormat, verbose: bool) -> String {
        crate::server::dump_server_debug_info(ctx.state(), format, verbose).await
    }
}
