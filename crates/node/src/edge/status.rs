//! What a profile's cloud link is doing, apart from what the person asked
//! for: the front door reports it as the profile's observed state.

use tokio::sync::watch;

/// Which carrier a relay link runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayCarrier {
    Quic,
    Tcp,
}

/// Connectivity observed by the cloud connector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Observed {
    Local,
    Connecting,
    Connected {
        tier: crate::Tier,
        carrier: RelayCarrier,
    },
    Retrying,
    AuthenticationRequired,
    /// The relay speaks no protocol version this build does.
    VersionMismatch,
}

impl Observed {
    pub fn to_wire(&self) -> wire::Observed {
        match self {
            Self::Local => wire::Observed::Local,
            Self::Connecting => wire::Observed::Connecting,
            Self::Connected { .. } => wire::Observed::Connected,
            Self::Retrying => wire::Observed::Retrying,
            Self::AuthenticationRequired => wire::Observed::AuthenticationRequired,
            Self::VersionMismatch => wire::Observed::VersionMismatch,
        }
    }
}

/// The observed state and its watchers. Kept apart from any one link, so
/// the state outlives a connector that stopped.
#[derive(Clone)]
pub(crate) struct RuntimeStatus {
    tx: watch::Sender<Observed>,
}

impl Default for RuntimeStatus {
    fn default() -> Self {
        Self {
            tx: watch::channel(Observed::Local).0,
        }
    }
}

impl RuntimeStatus {
    pub(crate) fn subscribe(&self) -> watch::Receiver<Observed> {
        self.tx.subscribe()
    }

    pub(crate) fn current(&self) -> Observed {
        self.tx.borrow().clone()
    }

    pub(crate) fn report(&self, observed: Observed) {
        self.tx.send_replace(observed);
    }
}
