//! Optional LAN discovery over mDNS / DNS-SD. Discovery only fills in an
//! address for the user; trust always comes from pairing, never from an
//! advertisement. The advertisement carries no secrets.

use crate::identity::Fingerprint;
use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

pub const SERVICE_TYPE: &str = "_displayanything._tcp.local.";

/// Keeps the advertisement alive; it is withdrawn when dropped.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

fn instance_label(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == ' ' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        "DisplayAnything device".into()
    } else {
        clean.chars().take(48).collect()
    }
}

pub fn advertise(name: &str, port: u16, fp: &Fingerprint) -> Result<Advertiser> {
    let daemon = ServiceDaemon::new().context("starting mDNS")?;
    let host = format!("da-{}.local.", &fp.to_hex()[..12]);
    let props: HashMap<String, String> = [
        ("fp".to_string(), fp.to_hex()),
        ("v".to_string(), crate::PROTOCOL_VERSION.to_string()),
    ]
    .into_iter()
    .collect();
    let info = ServiceInfo::new(SERVICE_TYPE, &instance_label(name), &host, "", port, props)
        .context("building mDNS record")?
        .enable_addr_auto();
    let fullname = info.get_fullname().to_string();
    daemon.register(info).context("registering mDNS service")?;
    Ok(Advertiser { daemon, fullname })
}

#[derive(Clone, Debug)]
pub struct Found {
    pub name: String,
    pub fingerprint: Option<String>,
    pub addrs: Vec<IpAddr>,
    pub port: u16,
}

/// Listen for advertisements for `wait`, then return what was seen.
pub async fn browse(wait: Duration) -> Result<Vec<Found>> {
    let daemon = ServiceDaemon::new().context("starting mDNS")?;
    let rx = daemon.browse(SERVICE_TYPE).context("browsing mDNS")?;
    let mut found: HashMap<String, Found> = HashMap::new();
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let ev = tokio::time::timeout_at(deadline, rx.recv_async()).await;
        let Ok(Ok(ev)) = ev else { break };
        if let ServiceEvent::ServiceResolved(info) = ev {
            let name = info
                .get_fullname()
                .strip_suffix(&format!(".{SERVICE_TYPE}"))
                .unwrap_or(info.get_fullname())
                .to_string();
            let mut addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
            addrs.sort();
            found.insert(
                info.get_fullname().to_string(),
                Found {
                    name,
                    fingerprint: info.get_property_val_str("fp").map(str::to_string),
                    addrs,
                    port: info.get_port(),
                },
            );
        }
    }
    let _ = daemon.shutdown();
    let mut v: Vec<Found> = found.into_values().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}
