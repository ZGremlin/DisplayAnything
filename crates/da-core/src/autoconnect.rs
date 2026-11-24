//! Keep sessions open with saved peers, reconnecting in the background.
//!
//! For each trusted peer with `auto_connect` on, a task dials its last known
//! address. If that fails it looks the peer up by fingerprint over mDNS (its
//! address may have changed) and retries with backoff from 1 s to 30 s. Once
//! connected it pings every few seconds so a dropped link is noticed quickly.

use crate::discovery;
use crate::identity::Fingerprint;
use crate::net::{Event, Node};
use crate::DEFAULT_PORT;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const KEEPALIVE: Duration = Duration::from_secs(5);
const RESCAN_EVERY: Duration = Duration::from_secs(5);
const MDNS_WAIT: Duration = Duration::from_secs(2);

/// Owns one background task per saved peer. Dropping it stops them all.
pub struct AutoConnector {
    node: Arc<Node>,
    tasks: HashMap<Fingerprint, JoinHandle<()>>,
    use_mdns: bool,
}

impl Drop for AutoConnector {
    fn drop(&mut self) {
        for (_, t) in self.tasks.drain() {
            t.abort();
        }
    }
}

impl AutoConnector {
    pub fn new(node: Arc<Node>, use_mdns: bool) -> Self {
        AutoConnector {
            node,
            tasks: HashMap::new(),
            use_mdns,
        }
    }

    /// Start tasks for newly saved peers and stop tasks for removed ones or
    /// ones whose auto-connect was switched off. Call after pairing or when
    /// settings change; `run` also calls it periodically.
    pub fn refresh(&mut self) {
        let wanted: Vec<Fingerprint> = self
            .node
            .trust()
            .peers()
            .iter()
            .filter(|p| p.auto_connect)
            .filter_map(|p| p.fingerprint())
            .collect();
        self.tasks.retain(|fp, t| {
            let keep = wanted.contains(fp) && !t.is_finished();
            if !keep {
                t.abort();
            }
            keep
        });
        for fp in wanted {
            if !self.tasks.contains_key(&fp) {
                let node = self.node.clone();
                let use_mdns = self.use_mdns;
                self.tasks
                    .insert(fp, tokio::spawn(maintain(node, fp, use_mdns)));
            }
        }
    }

    /// Refresh every few seconds forever.
    pub async fn run(mut self) {
        loop {
            self.refresh();
            tokio::time::sleep(RESCAN_EVERY).await;
        }
    }
}

async fn maintain(node: Arc<Node>, fp: Fingerprint, use_mdns: bool) {
    let mut backoff = BACKOFF_MIN;
    loop {
        let Some(peer) = node.trust().get(&fp).cloned() else {
            return;
        };
        if !peer.auto_connect {
            return;
        }
        let port = peer.port.unwrap_or(DEFAULT_PORT);
        let mut candidates: Vec<SocketAddr> = peer
            .last_addr
            .as_deref()
            .and_then(|a| a.parse::<IpAddr>().ok())
            .map(|ip| SocketAddr::new(ip, port))
            .into_iter()
            .collect();

        let mut session = None;
        for addr in &candidates {
            if let Ok(s) = node.connect(*addr).await {
                session = Some((s, *addr));
                break;
            }
        }
        if session.is_none() && use_mdns {
            if let Ok(found) = discovery::browse(MDNS_WAIT).await {
                let hex = fp.to_hex();
                for f in found
                    .into_iter()
                    .filter(|f| f.fingerprint.as_deref() == Some(hex.as_str()))
                {
                    candidates = f
                        .addrs
                        .iter()
                        .map(|ip| SocketAddr::new(*ip, f.port))
                        .collect();
                    for addr in &candidates {
                        if let Ok(s) = node.connect(*addr).await {
                            session = Some((s, *addr));
                            break;
                        }
                    }
                }
            }
        }

        let Some((mut s, addr)) = session else {
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
            continue;
        };
        backoff = BACKOFF_MIN;
        node.emit(Event::PeerOnline {
            name: peer.name.clone(),
            fingerprint: fp,
            addr,
        });
        loop {
            tokio::time::sleep(KEEPALIVE).await;
            let still_wanted = node.trust().get(&fp).is_some_and(|p| p.auto_connect);
            if !still_wanted {
                let _ = s.close().await;
                node.emit(Event::PeerOffline {
                    name: peer.name.clone(),
                    fingerprint: fp,
                });
                return;
            }
            if s.ping().await.is_err() {
                break;
            }
        }
        node.emit(Event::PeerOffline {
            name: peer.name.clone(),
            fingerprint: fp,
        });
    }
}
