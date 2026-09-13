//! What a client can say about its relay link, in the few kinds a person can
//! be told apart.

use serde::{Deserialize, Serialize};

/// Relay connectivity, independent of the in-process client-service connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayConnection {
    Connecting,
    Connected,
    Disconnected { reason: DisconnectReason },
}

/// Why the relay is not connected.
///
/// Deliberately a closed set rather than the transport's own error. What a
/// screen has to say about being offline is whether the network is the
/// problem, whether the account is, or whether there is nothing to do but
/// wait — and a formatted transport status answers none of those while reading
/// like a crash. The words belong to whoever is drawing; this says only which
/// of them applies. Diagnostic detail stays in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisconnectReason {
    /// The relay could not be dialled: no network, no route, refused.
    Unreachable,
    /// The relay answered and would not take this device's credentials.
    Rejected,
    /// Dialled, but nothing came back before the handshake deadline.
    TimedOut,
    /// A connection that was up has ended; the loop will dial again.
    Ended,
    /// The client itself stopped running, so nothing is dialling.
    Stopped,
    /// The app is not in front of anybody, so it holds no connection. Nothing
    /// is wrong and nothing is retrying; coming back is what restores it.
    Suspended,
}
