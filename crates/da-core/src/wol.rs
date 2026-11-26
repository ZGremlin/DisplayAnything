//! Wake-on-LAN: wake a sleeping computer with a "magic packet".
//!
//! A magic packet is 6 bytes of `0xFF` followed by the target's hardware
//! (MAC) address 16 times, sent over UDP. A sleeping computer whose network
//! card is set to wake on it (on a Mac, "Wake for network access" in Energy
//! settings) powers up when one arrives.
//!
//! The host tells each paired viewer its hardware address when they pair or
//! connect (`wake_mac` in `PairOk` and `Welcome`), so a viewer can wake it
//! later without the person having to look the address up. Hardware
//! addresses never leave the local network: they travel inside the paired,
//! encrypted connection, and the magic packet itself is a LAN broadcast.

use crate::identity::Fingerprint;
use crate::net::{is_lan, Node};
use anyhow::{anyhow, bail, Result};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// The port magic packets are sent to ("discard"). Network cards look at
/// the payload, not the port, so this is only convention.
pub const WOL_PORT: u16 = 9;

/// How many times each packet is sent. UDP can drop one; a sleeping card
/// only needs one to arrive.
const REPEATS: usize = 3;

/// A 6-byte hardware (MAC) address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    /// Parse `aa:bb:cc:dd:ee:ff` (or with `-`, or bare hex).
    pub fn parse(s: &str) -> Result<Self> {
        let hex: String = s
            .trim()
            .chars()
            .filter(|c| !matches!(c, ':' | '-' | '.'))
            .collect();
        let bytes = hex::decode(&hex).map_err(|_| anyhow!("'{s}' is not a hardware address"))?;
        let arr: [u8; 6] = bytes
            .try_into()
            .map_err(|_| anyhow!("'{s}' is not a 6-byte hardware address"))?;
        let mac = MacAddr(arr);
        if !mac.is_usable() {
            bail!("'{s}' can't be used to wake a computer");
        }
        Ok(mac)
    }

    /// False for all-zero, broadcast and multicast addresses, which no
    /// network card owns.
    pub fn is_usable(&self) -> bool {
        self.0 != [0; 6] && self.0 != [0xFF; 6] && self.0[0] & 1 == 0
    }
}

impl std::fmt::Display for MacAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

/// The 102-byte magic packet for `mac`.
pub fn magic_packet(mac: MacAddr) -> [u8; 102] {
    let mut p = [0xFFu8; 102];
    for chunk in p[6..].chunks_exact_mut(6) {
        chunk.copy_from_slice(&mac.0);
    }
    p
}

/// Where a wake attempt sent its packets, and what failed.
#[derive(Clone, Debug, Default)]
pub struct WakeReport {
    pub sent_to: Vec<SocketAddr>,
    pub failed: Vec<(SocketAddr, String)>,
}

/// The addresses to send a magic packet to: the limited broadcast
/// (255.255.255.255), the directed broadcast of each local network (the
/// one holding `last_ip` if we know it, else all of them), and `last_ip`
/// itself. The directed broadcasts matter on a computer with more than one
/// network, where 255.255.255.255 only leaves by the default one. The
/// unicast copy helps where broadcasts are blocked (an iPhone without the
/// multicast entitlement) and the sender still knows the sleeping
/// computer's hardware address, or a Bonjour Sleep Proxy answers for it.
fn targets(last_ip: Option<IpAddr>) -> Vec<SocketAddr> {
    let last_v4 = match last_ip.map(|ip| ip.to_canonical()) {
        Some(IpAddr::V4(v)) if is_lan(IpAddr::V4(v)) => Some(v),
        _ => None,
    };
    let mut out = vec![SocketAddr::from((Ipv4Addr::BROADCAST, WOL_PORT))];
    let nets: Vec<(Ipv4Addr, Ipv4Addr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v) => Some((v.ip, v.netmask)),
            _ => None,
        })
        .collect();
    let on_same_net = |ip: Ipv4Addr, mask: Ipv4Addr, other: Ipv4Addr| {
        u32::from(ip) & u32::from(mask) == u32::from(other) & u32::from(mask)
    };
    let mut nets_to_use: Vec<_> = match last_v4 {
        Some(t) => nets
            .iter()
            .copied()
            .filter(|&(ip, mask)| on_same_net(ip, mask, t))
            .collect(),
        None => Vec::new(),
    };
    if nets_to_use.is_empty() {
        nets_to_use = nets;
    }
    for (ip, mask) in nets_to_use {
        // A /31 or /32 has no broadcast address.
        if u32::from(mask) >= 0xFFFF_FFFE {
            continue;
        }
        let bcast = Ipv4Addr::from(u32::from(ip) | !u32::from(mask));
        let a = SocketAddr::from((bcast, WOL_PORT));
        if !out.contains(&a) {
            out.push(a);
        }
    }
    if let Some(t) = last_v4 {
        out.push(SocketAddr::from((t, WOL_PORT)));
    }
    out
}

/// Send magic packets for `mac` to the local network. Succeeds if at least
/// one packet left this device; there is no reply to wait for, so whether
/// the computer woke is only known by trying to connect to it.
pub fn send_magic_packet(mac: MacAddr, last_ip: Option<IpAddr>) -> Result<WakeReport> {
    send_to(mac, targets(last_ip))
}

fn send_to(mac: MacAddr, targets: Vec<SocketAddr>) -> Result<WakeReport> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    let packet = magic_packet(mac);
    let mut report = WakeReport::default();
    for target in targets {
        let mut last_err = None;
        let mut ok = false;
        for _ in 0..REPEATS {
            match socket.send_to(&packet, target) {
                Ok(_) => ok = true,
                Err(e) => last_err = Some(e.to_string()),
            }
        }
        if ok {
            report.sent_to.push(target);
        } else if let Some(e) = last_err {
            report.failed.push((target, e));
        }
    }
    if report.sent_to.is_empty() {
        let why = report
            .failed
            .iter()
            .map(|(a, e)| format!("{a}: {e}"))
            .collect::<Vec<_>>()
            .join("; ");
        bail!("could not send a wake packet on this network ({why})");
    }
    tracing::info!(%mac, sent_to = ?report.sent_to, failed = ?report.failed, "sent wake-on-LAN packets");
    Ok(report)
}

impl Node {
    /// Wake a paired computer that told us its hardware address.
    pub fn wake_peer(&self, fp: &Fingerprint) -> Result<WakeReport> {
        let peer = self
            .trust()
            .get(fp)
            .cloned()
            .ok_or_else(|| anyhow!("that device is not paired with this one"))?;
        let mac = peer
            .wake_mac
            .as_deref()
            .map(MacAddr::parse)
            .transpose()?
            .ok_or_else(|| {
                anyhow!(
                    "{} hasn't told this device how to wake it yet: connect to it once \
                     while it's awake, with a DisplayAnything version that supports waking",
                    peer.name
                )
            })?;
        let last_ip = peer.last_addr.as_deref().and_then(|a| a.parse().ok());
        send_magic_packet(mac, last_ip)
    }

    /// Host side: the hardware address to give a peer that reached us on
    /// `local`, so it can wake us later. `None` for phones (never woken)
    /// and where the address can't be read.
    pub(crate) fn own_wake_mac(&self, local: Option<SocketAddr>) -> Option<String> {
        if !self.device_kind().is_desktop() {
            return None;
        }
        let mac = local_mac_for(local?.ip())?;
        Some(mac.to_string())
    }
}

/// The hardware address of the network interface that holds `ip`.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub fn local_mac_for(ip: IpAddr) -> Option<MacAddr> {
    let ip = ip.to_canonical();
    if ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    let ifaces = getifaddrs::all();
    let name = ifaces
        .iter()
        .find(|i| i.ip == Some(ip))
        .map(|i| i.name.clone())?;
    ifaces
        .into_iter()
        .find(|i| i.name == name && i.mac.is_some())
        .and_then(|i| i.mac)
        .filter(MacAddr::is_usable)
}

/// Not implemented on this platform yet (Windows): the host simply doesn't
/// offer waking.
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
pub fn local_mac_for(_ip: IpAddr) -> Option<MacAddr> {
    None
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
mod getifaddrs {
    use super::MacAddr;
    use std::ffi::CStr;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    pub struct Iface {
        pub name: String,
        pub ip: Option<IpAddr>,
        pub mac: Option<MacAddr>,
    }

    /// Every address of every interface, as `getifaddrs(3)` lists them. An
    /// interface appears once per address: its IP entries and its link-layer
    /// entry (which carries the hardware address) share a name.
    pub fn all() -> Vec<Iface> {
        let mut out = Vec::new();
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs fills `head` with a list we free below; every
        // pointer is read only while the list is alive, and each sockaddr
        // is read as the type its family says it is.
        unsafe {
            if libc::getifaddrs(&mut head) != 0 {
                return out;
            }
            let mut cur = head;
            while !cur.is_null() {
                let ifa = &*cur;
                cur = ifa.ifa_next;
                if ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() {
                    continue;
                }
                let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                let family = (*ifa.ifa_addr).sa_family as i32;
                let (ip, mac) = match family {
                    libc::AF_INET => {
                        let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                        let ip = Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr));
                        (Some(IpAddr::V4(ip)), None)
                    }
                    libc::AF_INET6 => {
                        let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                        (Some(IpAddr::V6(Ipv6Addr::from(sa.sin6_addr.s6_addr))), None)
                    }
                    _ => (None, link_addr(ifa.ifa_addr)),
                };
                out.push(Iface { name, ip, mac });
            }
            libc::freeifaddrs(head);
        }
        out
    }

    /// macOS and iOS: an `AF_LINK` entry holds a `sockaddr_dl`, whose data
    /// is the interface name followed by the hardware address.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    unsafe fn link_addr(sa: *const libc::sockaddr) -> Option<MacAddr> {
        if (*sa).sa_family as i32 != libc::AF_LINK {
            return None;
        }
        let dl = &*(sa as *const libc::sockaddr_dl);
        if dl.sdl_alen != 6 {
            return None;
        }
        // The struct is variable-length (`sdl_len` bytes); sdl_data can run
        // past the 12 bytes the type declares, so index from its start.
        let data_off = std::mem::offset_of!(libc::sockaddr_dl, sdl_data);
        let start = data_off + dl.sdl_nlen as usize;
        if start + 6 > dl.sdl_len as usize {
            return None;
        }
        let base = sa as *const u8;
        let mut mac = [0u8; 6];
        std::ptr::copy_nonoverlapping(base.add(start), mac.as_mut_ptr(), 6);
        Some(MacAddr(mac))
    }

    /// Linux (tests and CI): an `AF_PACKET` entry holds a `sockaddr_ll`.
    #[cfg(target_os = "linux")]
    unsafe fn link_addr(sa: *const libc::sockaddr) -> Option<MacAddr> {
        if (*sa).sa_family as i32 != libc::AF_PACKET {
            return None;
        }
        let ll = &*(sa as *const libc::sockaddr_ll);
        if ll.sll_halen != 6 {
            return None;
        }
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&ll.sll_addr[..6]);
        Some(MacAddr(mac))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_is_ff_then_mac_sixteen_times() {
        let mac = MacAddr([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        let p = magic_packet(mac);
        assert_eq!(p.len(), 6 + 16 * 6);
        assert_eq!(&p[..6], &[0xFF; 6]);
        for i in 0..16 {
            assert_eq!(&p[6 + i * 6..12 + i * 6], &mac.0, "copy {i}");
        }
    }

    #[test]
    fn parses_and_prints_addresses() {
        let mac = MacAddr::parse("A4:83:E7:0B:1C:2D").unwrap();
        assert_eq!(mac.0, [0xa4, 0x83, 0xe7, 0x0b, 0x1c, 0x2d]);
        assert_eq!(mac.to_string(), "a4:83:e7:0b:1c:2d");
        assert_eq!(MacAddr::parse("a4-83-e7-0b-1c-2d").unwrap(), mac);
        assert_eq!(MacAddr::parse("a483e70b1c2d").unwrap(), mac);
        assert!(MacAddr::parse("a4:83:e7:0b:1c").is_err());
        assert!(MacAddr::parse("zz:83:e7:0b:1c:2d").is_err());
        assert!(MacAddr::parse("00:00:00:00:00:00").is_err());
        assert!(MacAddr::parse("ff:ff:ff:ff:ff:ff").is_err());
        assert!(MacAddr::parse("01:00:5e:00:00:01").is_err(), "multicast");
    }

    #[test]
    fn targets_include_broadcast_and_the_last_address() {
        let t = targets(Some("192.168.1.20".parse().unwrap()));
        assert_eq!(t[0], "255.255.255.255:9".parse().unwrap());
        assert_eq!(*t.last().unwrap(), "192.168.1.20:9".parse().unwrap());
        // A public address is never sent to directly.
        let t = targets(Some("8.8.8.8".parse().unwrap()));
        assert!(!t.contains(&"8.8.8.8:9".parse().unwrap()));
        assert!(t.iter().all(|a| a.port() == WOL_PORT));
    }

    #[test]
    fn sent_packets_arrive_intact() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mac = MacAddr([2, 3, 4, 5, 6, 7]);
        let report = send_to(mac, vec![rx.local_addr().unwrap()]).unwrap();
        assert_eq!(report.sent_to, vec![rx.local_addr().unwrap()]);
        let mut buf = [0u8; 200];
        for _ in 0..REPEATS {
            let n = rx.recv(&mut buf).unwrap();
            assert_eq!(&buf[..n], &magic_packet(mac)[..]);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn finds_the_hardware_address_of_a_local_interface() {
        // Loopback has none; any other IPv4 interface with a hardware
        // address should be found from its IP.
        assert_eq!(local_mac_for("127.0.0.1".parse().unwrap()), None);
        let all = getifaddrs::all();
        for i in all
            .iter()
            .filter(|i| matches!(i.ip, Some(IpAddr::V4(v)) if !v.is_loopback()))
        {
            let has_mac = all
                .iter()
                .any(|j| j.name == i.name && j.mac.is_some_and(|m| m.is_usable()));
            if has_mac {
                assert!(local_mac_for(i.ip.unwrap()).is_some(), "{}", i.name);
            }
        }
    }
}
