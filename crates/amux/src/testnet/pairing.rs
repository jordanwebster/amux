//! Pairing verbs: the real pairing flows, driven through each daemon's
//! local-admin `ClientService` surface exactly as the CLI drives them.
//!
//! Responder verbs (`start_pairing`, `start_qr_pairing`, `cancel_pairing`)
//! call the pairing admin RPCs. Initiator verbs run the real bootstrap
//! flows: the one SPAKE2 protocol over direct TCP or a cloud-routed tunnel
//! (the secret typed as a PIN or scanned from a QR), and the SSH identity
//! exchange (over an in-memory stream in place of a real `ssh` child).

use std::time::Duration;

use super::Daemon;
use super::assertions::eventually;
use crate::client::PairingSecret;
use crate::{HostId, pair_via_pin_direct_tcp, pair_via_ssh_initiator, pair_via_ssh_responder};

/// A 6-digit pairing PIN handed out by [`Daemon::start_pairing`].
/// Dereferences to the PIN string for `pair(..).with_pin(&pin)`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pin(String);

impl Pin {
    /// A PIN that is definitely not this one, still six digits — the
    /// adversary's (deterministically) wrong guess.
    pub fn wrong_guess(&self) -> Pin {
        let first = self.0.as_bytes()[0];
        let flipped = if first == b'9' {
            '0'
        } else {
            (first + 1) as char
        };
        Pin(format!("{flipped}{}", &self.0[1..]))
    }
}

impl std::ops::Deref for Pin {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The contents a responder's QR code would carry, handed out by
/// [`Daemon::start_qr_pairing`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QrPayload {
    /// The responder's host id.
    pub host_id: HostId,
    /// The one-shot 256-bit SPAKE2 secret consumed by the first successful
    /// pairing; it never crosses the wire.
    pub secret: Vec<u8>,
}

impl Daemon {
    /// Responder: enters pair-mode via the local-admin `StartPairing` RPC
    /// (PIN mode) and returns the generated PIN. Panics on rejection; use
    /// [`Self::try_start_pairing`] to observe the error.
    pub async fn start_pairing(&self) -> Pin {
        self.try_start_pairing()
            .await
            .unwrap_or_else(|error| panic!("'{}' failed to start pairing: {error}", self.name()))
    }

    /// Responder: `StartPairing` in PIN mode, surfacing the RPC error (e.g.
    /// when pair-mode is already active).
    pub async fn try_start_pairing(&self) -> anyhow::Result<Pin> {
        let start = self.admin_client().await.start_pin_pairing().await?;
        match start.secret {
            PairingSecret::Pin(pin) => Ok(Pin(pin)),
            PairingSecret::QrSecret(_) => {
                anyhow::bail!("StartPairing(PIN) returned a QR secret")
            }
        }
    }

    /// Responder: enters pair-mode via `StartPairing` in QR mode and returns
    /// what the QR code would carry. Panics on rejection; use
    /// [`Self::try_start_qr_pairing`] to observe the error.
    pub async fn start_qr_pairing(&self) -> QrPayload {
        self.try_start_qr_pairing()
            .await
            .unwrap_or_else(|error| panic!("'{}' failed to start QR pairing: {error}", self.name()))
    }

    /// Responder: `StartPairing` in QR mode, surfacing the RPC error.
    pub async fn try_start_qr_pairing(&self) -> anyhow::Result<QrPayload> {
        let start = self.admin_client().await.start_qr_pairing().await?;
        match start.secret {
            PairingSecret::QrSecret(secret) => Ok(QrPayload {
                host_id: start.identity.host_id,
                secret,
            }),
            PairingSecret::Pin(_) => anyhow::bail!("StartPairing(QR) returned a PIN"),
        }
    }

    /// Test seam for pair-mode TTL expiry: the real `StartPairing` RPC always
    /// uses the production ~5-minute TTL, so this starts PIN pair-mode
    /// directly on the daemon's `PairMode` with a short TTL. Everything else
    /// (the PIN attempt, status, expiry purge) runs the production code.
    pub async fn start_pairing_with_ttl(&self, ttl: Duration) -> Pin {
        self.try_start_pairing_with_ttl(ttl)
            .await
            .unwrap_or_else(|error| panic!("'{}' failed to start pair-mode: {error}", self.name()))
    }

    /// Fallible TTL override for externally controlled test networks.
    pub async fn try_start_pairing_with_ttl(&self, ttl: Duration) -> anyhow::Result<Pin> {
        anyhow::ensure!(!ttl.is_zero(), "pairing TTL must be positive");
        let pin = format!("{:06}", uuid::Uuid::new_v4().as_u128() % 1_000_000);
        let guard = self.inner.runtime.lock().await;
        let runtime = guard
            .as_ref()
            .unwrap_or_else(|| panic!("daemon '{}' is not running", self.name()));
        runtime
            .services
            .pair_mode
            .start_pin_for_duration(pin.clone(), ttl)?;
        Ok(Pin(pin))
    }

    /// Responder: cancels pair-mode via the local-admin `CancelPairing` RPC.
    pub async fn cancel_pairing(&self) {
        self.admin_client()
            .await
            .cancel_pairing()
            .await
            .unwrap_or_else(|error| panic!("'{}' failed to cancel pairing: {error}", self.name()));
    }

    /// Pair-mode assertion via the `GetPairingStatus` RPC: pair-mode is (or
    /// becomes) active.
    pub async fn pair_mode_active(&self) {
        let assertion = format!("pair-mode on '{}' is active", self.name());
        eventually(
            &assertion,
            async || self.pair_mode_active_now().await,
            self.failure_dump(),
        )
        .await;
    }

    /// Pair-mode assertion via the `GetPairingStatus` RPC: pair-mode ends
    /// (consumed, cancelled, attempt-capped, or expired).
    pub async fn pair_mode_ends(&self) {
        let assertion = format!("pair-mode on '{}' ends", self.name());
        eventually(
            &assertion,
            async || !self.pair_mode_active_now().await,
            self.failure_dump(),
        )
        .await;
    }

    /// Initiator: starts a pairing attempt toward `other`; finish with one of
    /// the [`PairAttempt`] flow verbs.
    pub fn pair<'a>(&'a self, other: &'a Daemon) -> PairAttempt<'a> {
        PairAttempt {
            from: self,
            to: other,
        }
    }

    /// Revocation via the local-admin `Unpair` RPC.
    pub async fn unpair(&self, other: &Daemon) {
        self.admin_client()
            .await
            .unpair(other.host_id(), "spec test revocation")
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "'{}' failed to unpair '{}': {error}",
                    self.name(),
                    other.name()
                )
            });
    }

    async fn pair_mode_active_now(&self) -> bool {
        self.admin_client()
            .await
            .pairing_is_active()
            .await
            .unwrap_or_else(|error| {
                panic!("'{}' failed to read pairing status: {error}", self.name())
            })
    }
}

/// Pending pairing attempt created by [`Daemon::pair`].
pub struct PairAttempt<'a> {
    from: &'a Daemon,
    to: &'a Daemon,
}

impl PairAttempt<'_> {
    /// PIN pairing over direct TCP: dial the responder's listener, run the
    /// real SPAKE2 exchange, and commit trust through the initiator's
    /// local-admin `PairPeer` RPC.
    pub async fn with_pin(self, pin: &str) -> anyhow::Result<()> {
        let addr = self.to.inner.tcp_addr.unwrap_or_else(|| {
            panic!(
                "with_pin: responder '{}' has no direct-TCP listener (cloud_only)",
                self.to.name()
            )
        });
        let client = self.from.admin_client().await;
        pair_via_pin_direct_tcp(
            &self.from.inner.data_dir,
            self.from.name(),
            addr,
            pin,
            &client,
        )
        .await?;
        Ok(())
    }

    /// PIN pairing through the cloud: the local-admin `PairPinCloudPeer` RPC
    /// runs SPAKE2 over a cloud-routed pairing tunnel to `other`.
    pub async fn with_cloud_pin(self, pin: &str) -> anyhow::Result<()> {
        self.from
            .admin_client()
            .await
            .pair_pin_cloud_peer(self.to.host_id(), pin.to_string())
            .await?;
        Ok(())
    }

    /// QR pairing through the cloud: the local-admin `PairQrCloudPeer` RPC
    /// feeds the QR's 256-bit secret into the same SPAKE2 stream the typed
    /// PIN uses, over a cloud-routed pairing tunnel.
    pub async fn with_qr(self, qr: &QrPayload) -> anyhow::Result<()> {
        self.from
            .admin_client()
            .await
            .pair_qr_cloud_peer(qr.host_id, qr.secret.clone())
            .await?;
        Ok(())
    }

    /// SSH pairing: runs the real identity exchange (initiator on `self`,
    /// `amux pair-recv` responder on `other`) over an in-memory stream
    /// standing in for the authenticated SSH stdio. Both sides commit via
    /// their own local-admin `PairPeer` RPCs, exactly like the CLI flow.
    pub async fn over_ssh(self) -> anyhow::Result<()> {
        let (initiator_io, responder_io) = tokio::io::duplex(64 * 1024);
        let initiator_client = self.from.admin_client().await;
        let responder_client = self.to.admin_client().await;
        let responder_data_dir = self.to.inner.data_dir.clone();
        let responder_name = self.to.name().to_string();
        let responder = tokio::spawn(async move {
            pair_via_ssh_responder(
                responder_io,
                responder_data_dir,
                responder_name,
                &responder_client,
            )
            .await
        });
        // `.invalid` is reserved (RFC 2606): the runtime `ssh` spawn this
        // reachability triggers can never accidentally reach a real host.
        let target = format!("{}.amux-testnet.invalid", self.to.name());
        let initiated = pair_via_ssh_initiator(
            initiator_io,
            &self.from.inner.data_dir,
            self.from.name(),
            target,
            &initiator_client,
        )
        .await;
        let responded = responder.await.expect("ssh pairing responder panicked");
        initiated?;
        responded?;

        // In production the initiator's commit immediately dials its stored
        // SSH reachability (`ssh <target> amux relay`) and brings the Link
        // up. The hermetic harness cannot spawn `ssh`, so the responder's
        // test TCP transport stands in for the SSH stdio link — the Link
        // semantics under test (a live, bidirectional, tunnel-carrying
        // stream) are transport-agnostic.
        let addr = self.to.inner.tcp_addr.expect(
            "over_ssh: the responder needs a TCP listener to stand in for the SSH stdio link",
        );
        self.from
            .spawn_direct_link(
                self.to.host_id(),
                crate::trust::Reachability::DirectTcp { addr },
            )
            .await;
        Ok(())
    }
}
