//! Per-device identity: a self-signed certificate generated on first run.
//! Peers recognise each other by the SHA-256 fingerprint of that certificate.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::Path;

const CERT_FILE: &str = "identity.cert.der";
const KEY_FILE: &str = "identity.key.der";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of(cert_der: &[u8]) -> Self {
        Fingerprint(Sha256::digest(cert_der).into())
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        let bytes = hex::decode(s.replace(':', "")).context("fingerprint is not hex")?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("fingerprint must be 32 bytes"))?;
        Ok(Fingerprint(arr))
    }

    /// Short form for showing to people, e.g. `3FA2-91C0-77DE-0B14`.
    pub fn short(&self) -> String {
        self.0[..8]
            .chunks(2)
            .map(hex::encode_upper)
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

#[derive(Clone)]
pub struct Identity {
    pub name: String,
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

impl Identity {
    /// Load the identity from `dir`, creating a new one if none exists.
    pub fn load_or_create(dir: &Path, name: &str) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating data dir {}", dir.display()))?;
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        if cert_path.exists() && key_path.exists() {
            return Ok(Identity {
                name: name.to_string(),
                cert_der: std::fs::read(&cert_path)?,
                key_der: std::fs::read(&key_path)?,
            });
        }
        let id = Self::generate(name)?;
        write_private(&key_path, &id.key_der)?;
        std::fs::write(&cert_path, &id.cert_der)?;
        Ok(id)
    }

    /// Generate a fresh in-memory identity (used by tests).
    pub fn generate(name: &str) -> Result<Self> {
        let ck = rcgen::generate_simple_self_signed(vec!["displayanything.local".to_string()])
            .context("generating device certificate")?;
        Ok(Identity {
            name: name.to_string(),
            cert_der: ck.cert.der().to_vec(),
            key_der: ck.key_pair.serialize_der(),
        })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.cert_der)
    }
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(data)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        // On Windows the per-user data dir is already private to the user.
        std::fs::write(path, data)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_persists() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(dir.path(), "a").unwrap();
        let b = Identity::load_or_create(dir.path(), "a").unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        let hex = a.fingerprint().to_hex();
        assert_eq!(Fingerprint::from_hex(&hex).unwrap(), a.fingerprint());
    }
}
