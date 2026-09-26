//! A UDP forwarder standing in for the network between a host and one
//! address: it can eat every datagram, as a network that blocks UDP does,
//! or lose some and delay the rest, as a poor one does.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

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
                let mut senders: HashMap<SocketAddr, (Arc<UdpSocket>, Returning)> = HashMap::new();
                let mut buf = vec![0; 65_536];
                loop {
                    let Ok((n, from)) = front.recv_from(&mut buf).await else {
                        return;
                    };
                    let back = match senders.get(&from) {
                        Some((back, _)) => back.clone(),
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
                                    let mut buf = vec![0; 65_536];
                                    while let Ok(n) = back.recv(&mut buf).await {
                                        match shared.pass() {
                                            Some(delay) if delay.is_zero() => {
                                                let _ = front.send_to(&buf[..n], from).await;
                                            }
                                            Some(delay) => {
                                                let (front, datagram) =
                                                    (front.clone(), buf[..n].to_vec());
                                                tokio::spawn(async move {
                                                    tokio::time::sleep(delay).await;
                                                    let _ = front.send_to(&datagram, from).await;
                                                });
                                            }
                                            None => {}
                                        }
                                    }
                                }
                            });
                            senders.insert(from, (back.clone(), Returning(returning)));
                            back
                        }
                    };
                    match shared.pass() {
                        Some(delay) if delay.is_zero() => {
                            let _ = back.send(&buf[..n]).await;
                        }
                        Some(delay) => {
                            let datagram = buf[..n].to_vec();
                            tokio::spawn(async move {
                                tokio::time::sleep(delay).await;
                                let _ = back.send(&datagram).await;
                            });
                        }
                        None => {}
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

    pub fn is_blocked(&self) -> bool {
        self.shared.blocked.load(Ordering::SeqCst)
    }

    pub fn set_faults(&self, faults: Faults) {
        *self.shared.faults.lock().unwrap() = faults;
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
