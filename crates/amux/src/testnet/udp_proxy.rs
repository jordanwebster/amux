//! Loopback UDP network for deterministic QUIC fault injection.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

use crate::HostId;

pub(crate) fn transport_config() -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport
        .keep_alive_interval(Some(Duration::from_millis(250)))
        .max_idle_timeout(Some(
            Duration::from_secs(2)
                .try_into()
                .expect("testnet QUIC idle timeout fits"),
        ))
        .max_concurrent_bidi_streams(64_u32.into())
        .max_concurrent_uni_streams(0_u32.into());
    Arc::new(transport)
}

#[derive(Clone)]
pub(crate) struct UdpProxy {
    inner: Arc<UdpProxyInner>,
}

struct UdpProxyInner {
    peers: Arc<RwLock<PeerTable>>,
    controls: Arc<Controls>,
    cancel: CancellationToken,
}

#[derive(Default)]
struct PeerTable {
    by_id: HashMap<HostId, Peer>,
    by_private_addr: HashMap<SocketAddr, HostId>,
}

struct Peer {
    private_addr: SocketAddr,
}

#[derive(Default)]
struct Controls {
    latency_ms: AtomicU64,
    loss_percent: AtomicU8,
    packet_number: AtomicU64,
    blocked: RwLock<HashSet<HostId>>,
}

pub(crate) struct UdpProxyBinding {
    pub(crate) socket: std::net::UdpSocket,
    pub(crate) public_addr: SocketAddr,
}

impl UdpProxy {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(UdpProxyInner {
                peers: Arc::new(RwLock::new(PeerTable::default())),
                controls: Arc::new(Controls::default()),
                cancel: CancellationToken::new(),
            }),
        }
    }

    pub(crate) fn register(&self, id: HostId) -> UdpProxyBinding {
        assert!(
            !self.inner.peers.read().unwrap().by_id.contains_key(&id),
            "UDP proxy peer {id} is already registered"
        );
        let private_socket = bind_std_udp();
        let private_addr = private_socket.local_addr().unwrap();
        let public_socket = Arc::new(bind_tokio_udp());
        let public_addr = public_socket.local_addr().unwrap();
        {
            let mut peers = self.inner.peers.write().unwrap();
            peers.by_private_addr.insert(private_addr, id);
            peers.by_id.insert(id, Peer { private_addr });
        }
        spawn_public_forwarder(
            id,
            public_socket,
            self.inner.peers.clone(),
            self.inner.controls.clone(),
            self.inner.cancel.clone(),
        );
        UdpProxyBinding {
            socket: private_socket,
            public_addr,
        }
    }

    pub(crate) fn rebind(&self, id: HostId) -> std::net::UdpSocket {
        let socket = bind_std_udp();
        let new_addr = socket.local_addr().unwrap();
        let mut peers = self.inner.peers.write().unwrap();
        let old_addr = peers
            .by_id
            .get(&id)
            .unwrap_or_else(|| panic!("UDP proxy peer {id} is not registered"))
            .private_addr;
        peers.by_private_addr.remove(&old_addr);
        peers.by_private_addr.insert(new_addr, id);
        peers.by_id.get_mut(&id).unwrap().private_addr = new_addr;
        socket
    }

    pub(crate) fn latency(&self, millis: u64) {
        self.inner
            .controls
            .latency_ms
            .store(millis, Ordering::SeqCst);
    }

    pub(crate) fn loss(&self, percent: u8) {
        assert!(percent <= 100, "UDP loss must be between 0 and 100 percent");
        self.inner
            .controls
            .loss_percent
            .store(percent, Ordering::SeqCst);
    }

    pub(crate) fn blocked(&self, id: HostId, blocked: bool) {
        let mut peers = self.inner.controls.blocked.write().unwrap();
        if blocked {
            peers.insert(id);
        } else {
            peers.remove(&id);
        }
    }
}

impl Drop for UdpProxyInner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn bind_std_udp() -> std::net::UdpSocket {
    std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind UDP proxy private socket")
}

fn bind_tokio_udp() -> UdpSocket {
    let socket = bind_std_udp();
    socket
        .set_nonblocking(true)
        .expect("set UDP proxy socket nonblocking");
    UdpSocket::from_std(socket).expect("adopt UDP proxy socket")
}

fn spawn_public_forwarder(
    target: HostId,
    public_socket: Arc<UdpSocket>,
    peers: Arc<RwLock<PeerTable>>,
    controls: Arc<Controls>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let flows = Arc::new(tokio::sync::Mutex::new(
            HashMap::<SocketAddr, Arc<UdpSocket>>::new(),
        ));
        let mut buffer = vec![0_u8; 65_535];
        loop {
            let received = tokio::select! {
                received = public_socket.recv_from(&mut buffer) => received,
                _ = cancel.cancelled() => return,
            };
            let (len, source_addr) = match received {
                Ok(received) => received,
                Err(error) => {
                    tracing::warn!(%error, "UDP proxy public receive failed");
                    continue;
                }
            };
            let source = peers
                .read()
                .unwrap()
                .by_private_addr
                .get(&source_addr)
                .copied();
            if controls.should_drop(source, Some(target)) {
                continue;
            }
            let flow = get_or_create_flow(
                source,
                source_addr,
                target,
                public_socket.clone(),
                flows.clone(),
                peers.clone(),
                controls.clone(),
                cancel.clone(),
            )
            .await;
            let payload = buffer[..len].to_vec();
            let destination = peers
                .read()
                .unwrap()
                .by_id
                .get(&target)
                .map(|p| p.private_addr);
            let latency = controls.latency();
            if latency.is_zero() {
                if let Some(destination) = destination {
                    let _ = flow.send_to(&payload, destination).await;
                }
            } else {
                tokio::spawn(async move {
                    tokio::time::sleep(latency).await;
                    if let Some(destination) = destination {
                        let _ = flow.send_to(&payload, destination).await;
                    }
                });
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn get_or_create_flow(
    source: Option<HostId>,
    source_addr: SocketAddr,
    target: HostId,
    public_socket: Arc<UdpSocket>,
    flows: Arc<tokio::sync::Mutex<HashMap<SocketAddr, Arc<UdpSocket>>>>,
    peers: Arc<RwLock<PeerTable>>,
    controls: Arc<Controls>,
    cancel: CancellationToken,
) -> Arc<UdpSocket> {
    let mut flows_guard = flows.lock().await;
    if let Some(flow) = flows_guard.get(&source_addr) {
        return flow.clone();
    }
    let flow = Arc::new(bind_tokio_udp());
    flows_guard.insert(source_addr, flow.clone());
    drop(flows_guard);

    let response_socket = flow.clone();
    tokio::spawn(async move {
        let mut buffer = vec![0_u8; 65_535];
        loop {
            let received = tokio::select! {
                received = response_socket.recv_from(&mut buffer) => received,
                _ = cancel.cancelled() => return,
            };
            let (len, _) = match received {
                Ok(received) => received,
                Err(error) => {
                    tracing::warn!(%error, "UDP proxy flow receive failed");
                    continue;
                }
            };
            if controls.should_drop(Some(target), source) {
                continue;
            }
            let destination = source
                .and_then(|id| peers.read().unwrap().by_id.get(&id).map(|p| p.private_addr))
                .unwrap_or(source_addr);
            let payload = buffer[..len].to_vec();
            let latency = controls.latency();
            if latency.is_zero() {
                let _ = public_socket.send_to(&payload, destination).await;
            } else {
                let public_socket = public_socket.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(latency).await;
                    let _ = public_socket.send_to(&payload, destination).await;
                });
            }
        }
    });
    flow
}

impl Controls {
    fn latency(&self) -> Duration {
        Duration::from_millis(self.latency_ms.load(Ordering::SeqCst))
    }

    fn should_drop(&self, source: Option<HostId>, target: Option<HostId>) -> bool {
        let blocked = self.blocked.read().unwrap();
        if source.is_some_and(|id| blocked.contains(&id))
            || target.is_some_and(|id| blocked.contains(&id))
        {
            return true;
        }
        drop(blocked);
        let loss = u64::from(self.loss_percent.load(Ordering::SeqCst));
        loss > 0 && self.packet_number.fetch_add(1, Ordering::SeqCst) % 100 < loss
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket(binding: &UdpProxyBinding) -> UdpSocket {
        binding.socket.set_nonblocking(true).unwrap();
        UdpSocket::from_std(binding.socket.try_clone().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn configured_loss_drops_within_tolerance_over_ten_thousand_datagrams() {
        let proxy = UdpProxy::new();
        let a = proxy.register(HostId::from_u128(1));
        let b = proxy.register(HostId::from_u128(2));
        let a_socket = socket(&a);
        let b_socket = socket(&b);
        proxy.loss(25);

        let receive = tokio::spawn(async move {
            let mut received = 0_usize;
            let mut buffer = [0_u8; 8];
            while tokio::time::timeout(Duration::from_millis(250), b_socket.recv_from(&mut buffer))
                .await
                .is_ok()
            {
                received += 1;
            }
            received
        });
        for sequence in 0_u64..10_000 {
            a_socket
                .send_to(&sequence.to_be_bytes(), b.public_addr)
                .await
                .unwrap();
            if sequence % 64 == 0 {
                tokio::task::yield_now().await;
            }
        }
        let received = receive.await.unwrap();
        assert!(
            (7_000..=8_000).contains(&received),
            "25% configured loss delivered {received} of 10,000 datagrams"
        );
    }

    #[tokio::test]
    async fn latency_delays_both_directions() {
        let proxy = UdpProxy::new();
        let a = proxy.register(HostId::from_u128(1));
        let b = proxy.register(HostId::from_u128(2));
        let a_socket = socket(&a);
        let b_socket = socket(&b);
        proxy.latency(40);

        let started = tokio::time::Instant::now();
        a_socket.send_to(b"request", b.public_addr).await.unwrap();
        let mut request = [0_u8; 16];
        let (len, reply_addr) = b_socket.recv_from(&mut request).await.unwrap();
        assert_eq!(&request[..len], b"request");
        assert!(started.elapsed() >= Duration::from_millis(35));

        let started = tokio::time::Instant::now();
        b_socket.send_to(b"reply", reply_addr).await.unwrap();
        let mut reply = [0_u8; 16];
        let (len, source) = a_socket.recv_from(&mut reply).await.unwrap();
        assert_eq!(&reply[..len], b"reply");
        assert_eq!(source, b.public_addr);
        assert!(started.elapsed() >= Duration::from_millis(35));
    }

    #[tokio::test]
    async fn udp_blocked_drops_everything_for_the_named_daemon() {
        let proxy = UdpProxy::new();
        let a_id = HostId::from_u128(1);
        let a = proxy.register(a_id);
        let b = proxy.register(HostId::from_u128(2));
        let a_socket = socket(&a);
        let b_socket = socket(&b);
        proxy.blocked(a_id, true);

        a_socket.send_to(b"outbound", b.public_addr).await.unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                b_socket.recv_from(&mut [0_u8; 16])
            )
            .await
            .is_err()
        );
        b_socket.send_to(b"inbound", a.public_addr).await.unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                a_socket.recv_from(&mut [0_u8; 16])
            )
            .await
            .is_err()
        );
    }
}
