//! A UDP forwarder standing in for the network between a host and one
//! address: it can eat every datagram, as a network that blocks UDP does,
//! or lose some and delay the rest, as a poor one does.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// What the gate does to each datagram it lets through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// Drops this many datagrams in a hundred, each way.
    pub loss_percent: u32,
    /// Holds each datagram this long before passing it on.
    pub delay: Duration,
}

struct Shared {
    blocked: AtomicBool,
    faults: Mutex<Faults>,
    /// A small generator for which datagrams are lost, seeded so a run can
    /// be told from another only by timing.
    draw: AtomicU64,
}

impl Shared {
    /// Whether this datagram goes through, and after how long.
    fn pass(&self) -> Option<Duration> {
        if self.blocked.load(Ordering::SeqCst) {
            return None;
        }
        let faults = *self.faults.lock().unwrap();
        if faults.loss_percent > 0 {
            let mut x = self.draw.load(Ordering::Relaxed);
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.draw.store(x, Ordering::Relaxed);
            if (x % 100) < u64::from(faults.loss_percent) {
                return None;
            }
        }
        Some(faults.delay)
    }
}

/// Each sender gets its own upstream socket, so the far end sees one peer
/// per sender.
pub struct UdpGate {
    addr: SocketAddr,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl UdpGate {
    pub async fn start(upstream: SocketAddr) -> std::io::Result<Self> {
        let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
        let addr = front.local_addr()?;
        let shared = Arc::new(Shared {
            blocked: AtomicBool::new(false),
            faults: Mutex::new(Faults::default()),
            draw: AtomicU64::new(0x9E37_79B9_7F4A_7C15),
        });
        let task = tokio::spawn({
            let shared = shared.clone();
            async move {
                let mut senders: HashMap<SocketAddr, Sender> = HashMap::new();
                let mut buf = vec![0; 65_536];
                loop {
                    let Ok((n, from)) = front.recv_from(&mut buf).await else {
                        return;
                    };
                    let sender = match senders.get(&from) {
                        Some(sender) => sender,
                        None => {
                            let Ok(back) = UdpSocket::bind("127.0.0.1:0").await else {
                                continue;
                            };
                            if back.connect(upstream).await.is_err() {
                                continue;
                            }
                            let back = Arc::new(back);
                            let returning = tokio::spawn({
                                let (back, front, shared) =
                                    (back.clone(), front.clone(), shared.clone());
                                async move {
                                    let replies = Held::start(move |datagram| {
                                        let front = front.clone();
                                        async move {
                                            let _ = front.send_to(&datagram, from).await;
                                        }
                                    });
                                    let mut buf = vec![0; 65_536];
                                    while let Ok(n) = back.recv(&mut buf).await {
                                        if let Some(delay) = shared.pass() {
                                            replies.hold(buf[..n].to_vec(), delay);
                                        }
                                    }
                                }
                            });
                            let onward = Held::start({
                                let back = back.clone();
                                move |datagram| {
                                    let back = back.clone();
                                    async move {
                                        let _ = back.send(&datagram).await;
                                    }
                                }
                            });
                            senders.entry(from).or_insert(Sender {
                                onward,
                                _returning: Returning(returning),
                            })
                        }
                    };
                    if let Some(delay) = shared.pass() {
                        sender.onward.hold(buf[..n].to_vec(), delay);
                    }
                }
            }
        });
        Ok(Self { addr, shared, task })
    }

    /// Where a host sends to go through the gate.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn block(&self, blocked: bool) {
        self.shared.blocked.store(blocked, Ordering::SeqCst);
    }

    pub fn set_faults(&self, faults: Faults) {
        *self.shared.faults.lock().unwrap() = faults;
    }
}

/// One sender through the gate: the queue carrying its datagrams on, and
/// the task carrying its replies back.
struct Sender {
    onward: Held,
    _returning: Returning,
}

/// Datagrams held for their delay and passed on in the order they came,
/// as a slow wire passes them. Holding each on its own timer would let
/// two sent in the same millisecond swap places, and a QUIC packet that
/// overtakes the handshake it follows is dropped by the receiver and
/// counted lost by the sender, which no household network does.
struct Held {
    queue: mpsc::UnboundedSender<(Instant, Vec<u8>)>,
    task: JoinHandle<()>,
}

impl Held {
    fn start<F, Fut>(deliver: F) -> Self
    where
        F: Fn(Vec<u8>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let (queue, mut held) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
        let task = tokio::spawn(async move {
            while let Some((due, datagram)) = held.recv().await {
                tokio::time::sleep_until(due).await;
                deliver(datagram).await;
            }
        });
        Self { queue, task }
    }

    fn hold(&self, datagram: Vec<u8>, delay: Duration) {
        let _ = self.queue.send((Instant::now() + delay, datagram));
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The task carrying one sender's replies back, stopped with the gate.
struct Returning(JoinHandle<()>);

impl Drop for Returning {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Drop for UdpGate {
    fn drop(&mut self) {
        self.task.abort();
    }
}
