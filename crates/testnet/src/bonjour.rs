//! A served host's advertisement on the machine's real local network, for a
//! client outside the net: the iPhone app on a simulator browses with the
//! system's own Bonjour browser and can find nothing on the scripted bus.
//!
//! The record goes through the system's mDNS responder (`dns-sd -P`) rather
//! than this process's own multicast socket, so no test binary rebuilt on
//! every run asks for local-network access, and it names the host's loopback
//! listener, so nothing listens on the Mac's network. Withdrawing ends the
//! registration, which the responder answers with a goodbye.

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use node::discovery::{Advertisement, Discovery, DiscoveryError, DiscoveryEvent, txt_properties};
use tokio::sync::broadcast;

/// Advertises through `dns-sd`; browses nothing, since the net's own hosts
/// find each other on the scripted bus or by their known addresses.
pub struct SystemBonjour {
    registration: Mutex<Option<Child>>,
    events: broadcast::Sender<DiscoveryEvent>,
}

impl SystemBonjour {
    pub fn new() -> Self {
        Self {
            registration: Mutex::new(None),
            events: broadcast::channel(1).0,
        }
    }
}

impl Default for SystemBonjour {
    fn default() -> Self {
        Self::new()
    }
}

/// The `dns-sd -P` arguments that register `advert` as its own host name at
/// its first address, with the TXT record every amux advertiser writes.
pub fn registration_arguments(advert: &Advertisement) -> Result<Vec<String>, String> {
    let address = advert
        .addrs
        .first()
        .ok_or("an advertisement needs a listener address")?;
    let mut arguments = vec![
        "-P".to_owned(),
        advert.name.clone(),
        "_amux._udp".to_owned(),
        "local".to_owned(),
        address.port().to_string(),
        format!("amux-{}.local", advert.host_id.simple()),
        address.ip().to_string(),
    ];
    arguments.extend(
        txt_properties(advert)
            .iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    Ok(arguments)
}

impl Discovery for SystemBonjour {
    fn advertise(&self, advert: Advertisement) -> Result<(), DiscoveryError> {
        let arguments = registration_arguments(&advert).map_err(DiscoveryError::Register)?;
        self.withdraw();
        let child = Command::new("dns-sd")
            .args(&arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| DiscoveryError::Register(format!("starting dns-sd: {error}")))?;
        *self.registration.lock().unwrap() = Some(child);
        Ok(())
    }

    fn withdraw(&self) {
        if let Some(mut child) = self.registration.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn browse(&self) -> broadcast::Receiver<DiscoveryEvent> {
        self.events.subscribe()
    }

    fn requery(&self) {}
}

impl Drop for SystemBonjour {
    fn drop(&mut self) {
        self.withdraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registration_names_the_loopback_listener_and_the_amux_record() {
        let advert = Advertisement {
            host_id: "d6e6b93c-114e-4d20-b29a-6fac10bfb78d".parse().unwrap(),
            name: "desk".into(),
            version: 4,
            addrs: vec!["127.0.0.1:52648".parse().unwrap()],
            scope: "journey".into(),
        };
        assert_eq!(
            registration_arguments(&advert).unwrap(),
            [
                "-P",
                "desk",
                "_amux._udp",
                "local",
                "52648",
                "amux-d6e6b93c114e4d20b29a6fac10bfb78d.local",
                "127.0.0.1",
                "v=4",
                "hid=d6e6b93c-114e-4d20-b29a-6fac10bfb78d",
                "scope=journey",
            ]
        );
        let bare = Advertisement {
            addrs: vec![],
            ..advert
        };
        assert!(registration_arguments(&bare).is_err());
    }
}
