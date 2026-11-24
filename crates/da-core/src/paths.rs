//! Shared locations and settings, so the tray app and `da-agent` use the
//! same identity and trusted peers.

use crate::DEFAULT_PORT;
use anyhow::{bail, Context, Result};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

const NAME_FILE: &str = "device_name.txt";

/// Per-user data directory, e.g. `%APPDATA%\DisplayAnything\data` on
/// Windows or `~/Library/Application Support/DisplayAnything` on macOS.
pub fn default_data_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "DisplayAnything")
        .context("could not find a data directory for this user")?;
    Ok(dirs.data_dir().to_path_buf())
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "DisplayAnything device".into())
}

/// The name other devices see. Saved once set, so it survives restarts;
/// defaults to the computer's hostname.
pub fn device_name(data_dir: &Path, set: Option<&str>) -> Result<String> {
    let file = data_dir.join(NAME_FILE);
    if let Some(n) = set.map(str::trim).filter(|n| !n.is_empty()) {
        std::fs::create_dir_all(data_dir)?;
        std::fs::write(&file, n)?;
        return Ok(n.to_string());
    }
    Ok(std::fs::read_to_string(&file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(hostname))
}

/// Accept `192.168.1.20`, `192.168.1.20:47800`, `fe80::1` or `[fe80::1]:47800`.
pub fn parse_addr(s: &str) -> Result<SocketAddr> {
    let s = s.trim();
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Ok(a);
    }
    if let Ok(ip) = s.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, DEFAULT_PORT));
    }
    bail!("'{s}' is not an IP address (try 192.168.1.20 or 192.168.1.20:{DEFAULT_PORT})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!(parse_addr("192.168.1.20").unwrap().port(), DEFAULT_PORT);
        assert_eq!(parse_addr("192.168.1.20:5000").unwrap().port(), 5000);
        assert_eq!(parse_addr("[fe80::1]:5000").unwrap().port(), 5000);
        assert!(parse_addr("fe80::1").is_ok());
        assert!(parse_addr("my-mac").is_err());
    }

    #[test]
    fn name_is_remembered() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(device_name(d.path(), Some("Studio")).unwrap(), "Studio");
        assert_eq!(device_name(d.path(), None).unwrap(), "Studio");
    }
}
