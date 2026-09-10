use std::sync::Mutex;

use tokio::sync::broadcast;

use super::{Advertisement, Discovery, DiscoveryError, DiscoveryEvent, EVENT_CAPACITY};
use crate::HostId;

/// A deterministic discovery bus for tests and platform adapters.
pub struct ScriptedDiscovery {
    events: broadcast::Sender<DiscoveryEvent>,
    advertised: Mutex<Option<HostId>>,
}

impl ScriptedDiscovery {
    pub fn new() -> Self {
        Self::default()
    }

    /// Emits a resolved service to every current browser.
    pub fn announce(&self, advert: Advertisement) {
        let _ = self.events.send(DiscoveryEvent::Found(advert));
    }

    /// Emits a goodbye or expiry to every current browser.
    pub fn withdraw_host(&self, host_id: HostId) {
        let _ = self.events.send(DiscoveryEvent::Lost { host_id });
    }
}

impl Default for ScriptedDiscovery {
    fn default() -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            events,
            advertised: Mutex::new(None),
        }
    }
}

impl Clone for ScriptedDiscovery {
    fn clone(&self) -> Self {
        Self {
            events: self.events.clone(),
            advertised: Mutex::new(None),
        }
    }
}

impl Discovery for ScriptedDiscovery {
    fn advertise(&self, advert: Advertisement) -> Result<(), DiscoveryError> {
        let previous = self.advertised.lock().unwrap().replace(advert.host_id);
        if let Some(host_id) = previous.filter(|host_id| *host_id != advert.host_id) {
            self.withdraw_host(host_id);
        }
        self.announce(advert);
        Ok(())
    }

    fn withdraw(&self) {
        if let Some(host_id) = self.advertised.lock().unwrap().take() {
            self.withdraw_host(host_id);
        }
    }

    fn browse(&self) -> broadcast::Receiver<DiscoveryEvent> {
        self.events.subscribe()
    }

    fn requery(&self) {}
}
