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

    pub fn note_address(&mut self, fp: &Fingerprint, addr: String) -> Result<()> {
        let hex = fp.to_hex();
        if let Some(p) = self.peers.iter_mut().find(|p| p.fingerprint == hex) {
            if p.last_addr.as_deref() != Some(addr.as_str()) {
                p.last_addr = Some(addr);
                self.save()?;
            }
        }
        Ok(())
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
            })
            .unwrap();
        }
        let mut t = TrustStore::load(dir.path()).unwrap();
        assert!(t.get(&fp).is_some());
        assert_eq!(t.remove("MAC").unwrap(), 1);
        assert!(TrustStore::load(dir.path()).unwrap().get(&fp).is_none());
    }
}
