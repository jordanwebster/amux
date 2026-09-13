use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use super::{Advertisement, Discovery, DiscoveryError, DiscoveryEvent, EVENT_CAPACITY};
use crate::HostId;

/// A deterministic discovery bus for tests and platform adapters.
pub struct ScriptedDiscovery {
    bus: Arc<ScriptedBus>,
    advertised: Mutex<Option<HostId>>,
}

struct ScriptedBus {
    events: broadcast::Sender<DiscoveryEvent>,
    active: Mutex<HashMap<HostId, Advertisement>>,
    suppressed: Mutex<std::collections::HashSet<HostId>>,
}

impl ScriptedDiscovery {
    pub fn new() -> Self {
        Self::default()
    }

    /// Emits a resolved service to every current browser.
    pub fn announce(&self, advert: Advertisement) {
        if self
            .bus
            .suppressed
            .lock()
            .unwrap()
            .contains(&advert.host_id)
        {
            return;
        }
        self.bus
            .active
            .lock()
            .unwrap()
            .insert(advert.host_id, advert.clone());
        let _ = self.bus.events.send(DiscoveryEvent::Found(advert));
    }

    /// Emits a goodbye or expiry to every current browser.
    pub fn withdraw_host(&self, host_id: HostId) {
        self.bus.active.lock().unwrap().remove(&host_id);
        let _ = self.bus.events.send(DiscoveryEvent::Lost { host_id });
    }

    pub fn suppress(&self, host_id: HostId) {
        self.bus.suppressed.lock().unwrap().insert(host_id);
        if self.bus.active.lock().unwrap().remove(&host_id).is_some() {
            let _ = self.bus.events.send(DiscoveryEvent::Lost { host_id });
        }
    }

    /// Injects a hostile claim even when its claimed id was suppressed from
    /// the ordinary test LAN. Used only to exercise identity verification.
    pub fn announce_unchecked(&self, advert: Advertisement) {
        self.bus
            .active
            .lock()
            .unwrap()
            .insert(advert.host_id, advert.clone());
        let _ = self.bus.events.send(DiscoveryEvent::Found(advert));
    }
}

impl Default for ScriptedDiscovery {
    fn default() -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            bus: Arc::new(ScriptedBus {
                events,
                active: Mutex::new(HashMap::new()),
                suppressed: Mutex::new(std::collections::HashSet::new()),
            }),
            advertised: Mutex::new(None),
        }
    }
}

impl Clone for ScriptedDiscovery {
    fn clone(&self) -> Self {
        Self {
            bus: self.bus.clone(),
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

    fn hand_over(&self, found: Vec<Advertisement>) {
        let handed: HashMap<HostId, Advertisement> = found
            .into_iter()
            .map(|advert| (advert.host_id, advert))
            .collect();
        let gone: Vec<HostId> = self
            .bus
            .active
            .lock()
            .unwrap()
            .keys()
            .filter(|host_id| !handed.contains_key(host_id))
            .copied()
            .collect();
        for host_id in gone {
            self.withdraw_host(host_id);
        }
        for advert in handed.into_values() {
            // Announced again even when it has not changed: an advertisement
            // is a dial hint with a lifetime, and repeating it is how a
            // browser says the machine is still there.
            self.announce(advert);
        }
    }

    fn browse(&self) -> broadcast::Receiver<DiscoveryEvent> {
        self.bus.events.subscribe()
    }

    fn requery(&self) {
        let active = self
            .bus
            .active
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for advert in active {
            let _ = self.bus.events.send(DiscoveryEvent::Found(advert));
        }
    }
}
