//! Trust store: the peers this device has paired with, and what each may do.

use crate::identity::Fingerprint;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const TRUST_FILE: &str = "trusted_peers.json";

/// What a paired peer is allowed to do on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    pub remote_control: bool,
    pub display_takeover: bool,
    pub input_takeover: bool,
}

impl Default for Permissions {
    /// Remote control of this desktop is opt-in; the two takeover features
    /// are on by default for paired peers.
    fn default() -> Self {
        Permissions {
            remote_control: false,
            display_takeover: true,
            input_takeover: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustedPeer {
    pub name: String,
    pub fingerprint: String,
    pub permissions: Permissions,
    pub paired_at_unix: u64,
    #[serde(default)]
    pub last_addr: Option<String>,
    /// Port the peer listens on, when we have dialled it.
    #[serde(default)]
    pub port: Option<u16>,
    /// Reconnect to this peer automatically whenever both are running.
    #[serde(default = "default_true")]
    pub auto_connect: bool,
    /// The user gave this peer its own name here, so names the peer
    /// announces for itself are not applied.
    #[serde(default)]
    pub custom_name: bool,
}

fn default_true() -> bool {
    true
}

impl TrustedPeer {
    pub fn fingerprint(&self) -> Option<Fingerprint> {
        Fingerprint::from_hex(&self.fingerprint).ok()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    peers: Vec<TrustedPeer>,
}

#[derive(Debug)]
pub struct TrustStore {
    path: Option<PathBuf>,
    peers: Vec<TrustedPeer>,
}

impl TrustStore {
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(TRUST_FILE);
        let peers = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            serde_json::from_str::<TrustFile>(&text)
                .with_context(|| format!("parsing {}", path.display()))?
                .peers
        } else {
            Vec::new()
        };
        Ok(TrustStore {
            path: Some(path),
            peers,
        })
    }

    /// A store that is never written to disk (tests).
    pub fn in_memory() -> Self {
        TrustStore {
            path: None,
            peers: Vec::new(),
        }
    }

    pub fn peers(&self) -> &[TrustedPeer] {
        &self.peers
    }

    pub fn get(&self, fp: &Fingerprint) -> Option<&TrustedPeer> {
        let hex = fp.to_hex();
        self.peers.iter().find(|p| p.fingerprint == hex)
    }

    /// Add or replace a peer, keyed by fingerprint.
    pub fn upsert(&mut self, peer: TrustedPeer) -> Result<()> {
        self.peers.retain(|p| p.fingerprint != peer.fingerprint);
        self.peers.push(peer);
        self.save()
    }

    /// Remove peers whose name or fingerprint (full or prefix) matches.
    /// Returns how many were removed.
    pub fn remove(&mut self, name_or_fp: &str) -> Result<usize> {
        let needle = name_or_fp.to_lowercase().replace([':', '-'], "");
        let before = self.peers.len();
        self.peers.retain(|p| {
            !(p.name.eq_ignore_ascii_case(name_or_fp)
                || (needle.len() >= 8 && p.fingerprint.starts_with(&needle)))
        });
        let removed = before - self.peers.len();
        if removed > 0 {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn set_permissions(&mut self, fp: &Fingerprint, perms: Permissions) -> Result<bool> {
        let hex = fp.to_hex();
        let Some(p) = self.peers.iter_mut().find(|p| p.fingerprint == hex) else {
            return Ok(false);
        };
        p.permissions = perms;
        self.save()?;
        Ok(true)
    }

    /// Apply a name the peer announced for itself, unless the user renamed
    /// it here. Returns whether the stored name changed.
    pub fn note_announced_name(&mut self, fp: &Fingerprint, announced: &str) -> Result<bool> {
        let announced = announced.trim();
        let hex = fp.to_hex();
        let Some(p) = self.peers.iter_mut().find(|p| p.fingerprint == hex) else {
            return Ok(false);
        };
        if p.custom_name || announced.is_empty() || p.name == announced {
            return Ok(false);
        }
        p.name = announced.to_string();
        self.save()?;
        Ok(true)
    }

    /// Give a peer a custom name, or with `None`/empty, stop using one (the
    /// peer's own name is applied again on the next connection).
    pub fn rename(&mut self, fp: &Fingerprint, name: Option<&str>) -> Result<bool> {
        let hex = fp.to_hex();
        let Some(p) = self.peers.iter_mut().find(|p| p.fingerprint == hex) else {
            return Ok(false);
        };
        match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => {
                p.name = n.to_string();
                p.custom_name = true;
            }
            None => p.custom_name = false,
        }
        self.save()?;
        Ok(true)
    }

    /// Remember where a peer was last reached. `port` is only known when we
    /// dialled the peer (an incoming connection comes from an ephemeral port).
    pub fn note_address(
        &mut self,
        fp: &Fingerprint,
        addr: String,
        port: Option<u16>,
    ) -> Result<()> {
        let hex = fp.to_hex();
        if let Some(p) = self.peers.iter_mut().find(|p| p.fingerprint == hex) {
            let new_port = port.or(p.port);
            if p.last_addr.as_deref() != Some(addr.as_str()) || p.port != new_port {
                p.last_addr = Some(addr);
                p.port = new_port;
                self.save()?;
            }
        }
        Ok(())
    }

    pub fn set_auto_connect(&mut self, fp: &Fingerprint, on: bool) -> Result<bool> {
        let hex = fp.to_hex();
        let Some(p) = self.peers.iter_mut().find(|p| p.fingerprint == hex) else {
            return Ok(false);
        };
        p.auto_connect = on;
        self.save()?;
        Ok(true)
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = serde_json::to_string_pretty(&TrustFile {
            peers: self.peers.clone(),
        })?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_remove_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let fp = Fingerprint([7u8; 32]);
        {
            let mut t = TrustStore::load(dir.path()).unwrap();
            t.upsert(TrustedPeer {
                name: "mac".into(),
                fingerprint: fp.to_hex(),
                permissions: Permissions::default(),
                paired_at_unix: 1,
                last_addr: None,
                port: None,
                auto_connect: true,
                custom_name: false,
            })
            .unwrap();
        }
        let mut t = TrustStore::load(dir.path()).unwrap();
        assert!(t.get(&fp).is_some());
        t.note_address(&fp, "192.168.1.9".into(), Some(47800))
            .unwrap();
        let reloaded = TrustStore::load(dir.path()).unwrap();
        let p = reloaded.get(&fp).unwrap();
        assert_eq!(
            (p.last_addr.as_deref(), p.port),
            (Some("192.168.1.9"), Some(47800))
        );
        assert_eq!(t.remove("MAC").unwrap(), 1);
        assert!(TrustStore::load(dir.path()).unwrap().get(&fp).is_none());
    }
}

#[test]
fn old_files_without_new_fields_still_load() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(TRUST_FILE),
        r#"{"peers":[{"name":"mac","fingerprint":"aa","permissions":{"remote_control":false,"display_takeover":true,"input_takeover":true},"paired_at_unix":1}]}"#,
    )
    .unwrap();
    let t = TrustStore::load(dir.path()).unwrap();
    assert!(t.peers()[0].auto_connect);
}
