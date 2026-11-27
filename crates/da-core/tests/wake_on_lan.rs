//! Wake-on-LAN: a desktop tells the devices it pairs with its hardware
//! address, so they can wake it later; a phone never hands out its own.
//!
//! The hardware address is read from the interface the connection arrives
//! on, and loopback has none, so these tests listen on this machine's LAN
//! address. Where there is none (an offline machine), they say so and pass.

use da_core::protocol::DeviceKind;
use da_core::wol::local_mac_for;
use da_core::{Config, Event, Identity, Node, TrustStore};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

struct NoScreen;

impl da_core::ScreenSource for NoScreen {
    fn request_keyframe(&self) {}
}

/// This machine's own LAN address and that interface's hardware address.
/// Connecting a UDP socket sends nothing; it only picks the route.
fn lan_ip_with_mac() -> Option<(IpAddr, String)> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("10.255.255.255:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    Some((ip, local_mac_for(ip)?.to_string()))
}

fn node(name: &str, kind: DeviceKind) -> Arc<Node> {
    Node::new(
        Identity::generate(name).unwrap(),
        TrustStore::in_memory(),
        Config {
            device_kind: kind,
            // CI machines may sit on a non-private address; real use keeps
            // this off.
            allow_public_addresses: true,
            ..Config::default()
        },
    )
    .unwrap()
}

async fn serve(node: &Arc<Node>, ip: IpAddr) -> SocketAddr {
    node.set_screen_starter(Some(Arc::new(|_opts, _sink| {
        Ok(Box::new(NoScreen) as Box<dyn da_core::ScreenSource>)
    })));
    let listener = TcpListener::bind((ip, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(node.clone().serve(listener));
    addr
}

async fn next_code(rx: &mut broadcast::Receiver<Event>) -> String {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Event::PairCodeShown { code, .. } => return code,
            _ => continue,
        }
    }
}

#[tokio::test]
async fn phone_learns_how_to_wake_the_desktop() {
    let Some((ip, mac)) = lan_ip_with_mac() else {
        eprintln!("no LAN interface with a hardware address here; skipping");
        return;
    };
    let desktop = node("mac", DeviceKind::Desktop);
    let addr = serve(&desktop, ip).await;
    let phone = node("iphone", DeviceKind::IosViewer);
    let desktop_fp = desktop.identity().fingerprint();

    let mut rx = desktop.subscribe();
    let saved = phone
        .pair(addr, || async move { Ok(next_code(&mut rx).await) })
        .await
        .unwrap();
    assert_eq!(
        saved.wake_mac.as_deref(),
        Some(mac.as_str()),
        "from pairing"
    );

    // A phone paired before waking existed learns it the next time it
    // opens the screen.
    let mut peer = phone.trust().get(&desktop_fp).cloned().unwrap();
    peer.wake_mac = None;
    phone.trust().upsert(peer).unwrap();
    let phone_fp = phone.identity().fingerprint();
    let mut perms = desktop.trust().get(&phone_fp).unwrap().permissions;
    perms.remote_control = true;
    desktop.trust().set_permissions(&phone_fp, perms).unwrap();
    let _view = phone.open_remote(addr, Default::default()).await.unwrap();
    assert_eq!(
        phone.trust().get(&desktop_fp).unwrap().wake_mac.as_deref(),
        Some(mac.as_str()),
        "from opening the screen"
    );

    // The desktop never stores a wake address for the phone.
    assert_eq!(desktop.trust().get(&phone_fp).unwrap().wake_mac, None);

    // And the phone can send the packets. Nothing here will wake, but the
    // unicast copy to a LAN desktop's address must leave (it is never sent
    // to a public address).
    let report = phone.wake_peer(&desktop_fp).expect("packets sent");
    assert_eq!(
        report.sent_to.contains(&SocketAddr::new(ip, 9)),
        da_core::net::is_lan(ip),
        "{report:?}"
    );
}

#[tokio::test]
async fn desktops_learn_it_from_their_session_too() {
    let Some((ip, mac)) = lan_ip_with_mac() else {
        eprintln!("no LAN interface with a hardware address here; skipping");
        return;
    };
    let host = node("mac", DeviceKind::Desktop);
    let addr = serve(&host, ip).await;
    let pc = node("pc", DeviceKind::Desktop);
    let host_fp = host.identity().fingerprint();
    let mut rx = host.subscribe();
    pc.pair(addr, || async move { Ok(next_code(&mut rx).await) })
        .await
        .unwrap();
    let mut peer = pc.trust().get(&host_fp).cloned().unwrap();
    peer.wake_mac = None;
    pc.trust().upsert(peer).unwrap();

    let session = pc.connect(addr).await.unwrap();
    assert_eq!(session.peer.wake_mac.as_deref(), Some(mac.as_str()));
}

#[tokio::test]
async fn waking_a_peer_without_an_address_explains_why() {
    let Some((ip, _)) = lan_ip_with_mac() else {
        eprintln!("no LAN interface with a hardware address here; skipping");
        return;
    };
    let host = node("old mac", DeviceKind::Desktop);
    let addr = serve(&host, ip).await;
    let phone = node("iphone", DeviceKind::IosViewer);
    let fp = host.identity().fingerprint();
    let mut rx = host.subscribe();
    phone
        .pair(addr, || async move { Ok(next_code(&mut rx).await) })
        .await
        .unwrap();
    let mut peer = phone.trust().get(&fp).cloned().unwrap();
    peer.wake_mac = None;
    phone.trust().upsert(peer).unwrap();

    let err = phone.wake_peer(&fp).unwrap_err().to_string();
    assert!(
        err.contains("hasn't told this device how to wake it"),
        "{err}"
    );
}
