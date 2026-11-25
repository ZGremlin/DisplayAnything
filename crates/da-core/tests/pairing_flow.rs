//! End-to-end: two nodes on loopback pair with a code, then reconnect
//! without one. Also covers wrong codes, lockout and unpaired connects.

use da_core::{Config, Event, Identity, Node, TrustStore};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

async fn start(name: &str) -> (Arc<Node>, SocketAddr) {
    let node = Node::new(
        Identity::generate(name).unwrap(),
        TrustStore::in_memory(),
        Config::default(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(node.clone().serve(listener));
    (node, addr)
}

fn client(name: &str) -> Arc<Node> {
    Node::new(
        Identity::generate(name).unwrap(),
        TrustStore::in_memory(),
        Config::default(),
    )
    .unwrap()
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
async fn pair_then_reconnect_without_code() {
    let (mac, mac_addr) = start("mac").await;
    let pc = client("pc");
    let mut rx = mac.subscribe();

    let peer = pc
        .pair(mac_addr, || async move { Ok(next_code(&mut rx).await) })
        .await
        .expect("pairing with the right code succeeds");
    assert_eq!(peer.name, "mac");
    assert_eq!(peer.fingerprint().unwrap(), mac.identity().fingerprint());
    assert!(
        mac.trust().get(&pc.identity().fingerprint()).is_some(),
        "responder stored the initiator"
    );

    let mut session = pc
        .connect(mac_addr)
        .await
        .expect("paired peers connect without a code");
    assert!(session.ping().await.unwrap() < Duration::from_secs(1));
    assert!(session.granted_to_us.input_takeover);
    assert!(
        !session.granted_to_us.remote_control,
        "remote control is opt-in"
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn wrong_code_is_rejected_and_nothing_is_stored() {
    let (mac, mac_addr) = start("mac").await;
    let pc = client("pc");
    let mut rx = mac.subscribe();

    let err = pc
        .pair(mac_addr, || async move {
            let real = next_code(&mut rx).await;
            Ok(if real == "000000" {
                "111111".into()
            } else {
                "000000".into()
            })
        })
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("wrong pairing code"),
        "got: {err:#}"
    );
    assert!(pc.trust().peers().is_empty());
    assert!(mac.trust().peers().is_empty());
}

#[tokio::test]
async fn unpaired_device_cannot_connect() {
    let (_mac, mac_addr) = start("mac").await;
    let stranger = client("stranger");
    let err = stranger.connect(mac_addr).await.unwrap_err();
    assert!(err.to_string().contains("not paired"), "got: {err:#}");
}

#[tokio::test]
async fn responder_rejects_device_it_has_forgotten() {
    let (mac, mac_addr) = start("mac").await;
    let pc = client("pc");
    let mut rx = mac.subscribe();
    pc.pair(mac_addr, || async move { Ok(next_code(&mut rx).await) })
        .await
        .unwrap();

    // The Mac un-pairs the PC; the PC still trusts the Mac.
    assert_eq!(mac.trust().remove("pc").unwrap(), 1);
    let err = pc.connect(mac_addr).await.expect_err("connect must fail");
    assert!(err.to_string().contains("not paired"), "got: {err:#}");
}

#[tokio::test]
async fn pairing_locks_after_five_wrong_codes() {
    let (mac, mac_addr) = start("mac").await;
    let pc = client("pc");
    for _ in 0..5 {
        let mut rx = mac.subscribe();
        let r = pc
            .pair(mac_addr, || async move {
                let real = next_code(&mut rx).await;
                Ok(if real == "000000" {
                    "111111".into()
                } else {
                    "000000".into()
                })
            })
            .await;
        assert!(r.is_err());
    }
    // Even the right code is refused now.
    let mut rx = mac.subscribe();
    let err = pc
        .pair(mac_addr, || async move { Ok(next_code(&mut rx).await) })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("locked"), "got: {err:#}");
}

#[test]
fn lan_filter() {
    use da_core::net::is_lan;
    for ok in [
        "192.168.1.20",
        "10.0.0.5",
        "172.16.3.4",
        "127.0.0.1",
        "169.254.1.1",
        "fe80::1",
        "fd00::1",
        "::1",
    ] {
        assert!(is_lan(ok.parse().unwrap()), "{ok} should be LAN");
    }
    for bad in ["8.8.8.8", "100.64.0.1", "2001:4860::8888"] {
        assert!(!is_lan(bad.parse().unwrap()), "{bad} should not be LAN");
    }
}

#[tokio::test]
async fn saved_peer_reconnects_automatically_after_restart() {
    use da_core::AutoConnector;
    let dir_mac = tempfile::tempdir().unwrap();
    let dir_pc = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mac_addr = listener.local_addr().unwrap();

    // First run: pair.
    let mac_id = Identity::load_or_create(dir_mac.path(), "mac").unwrap();
    let mac = Node::new(
        mac_id,
        TrustStore::load(dir_mac.path()).unwrap(),
        Config::default(),
    )
    .unwrap();
    let mac_task = tokio::spawn(mac.clone().serve(listener));
    {
        let pc = Node::new(
            Identity::load_or_create(dir_pc.path(), "pc").unwrap(),
            TrustStore::load(dir_pc.path()).unwrap(),
            Config::default(),
        )
        .unwrap();
        let mut rx = mac.subscribe();
        pc.pair(mac_addr, || async move { Ok(next_code(&mut rx).await) })
            .await
            .unwrap();
    }

    // "Reboot" the PC: a fresh node loaded from disk reconnects on its own.
    let pc = Node::new(
        Identity::load_or_create(dir_pc.path(), "pc").unwrap(),
        TrustStore::load(dir_pc.path()).unwrap(),
        Config::default(),
    )
    .unwrap();
    let mut pc_events = pc.subscribe();
    let auto = AutoConnector::new(pc.clone(), false);
    let auto_task = tokio::spawn(auto.run());
    let online = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Event::PeerOnline { name, addr, .. }) = pc_events.recv().await {
                return (name, addr);
            }
        }
    })
    .await
    .expect("saved peer came online without user action");
    assert_eq!(online, ("mac".to_string(), mac_addr));
    auto_task.abort();
    mac_task.abort();
}

#[tokio::test]
async fn renames_reach_the_other_device_unless_renamed_locally() {
    let (mac, mac_addr) = start("mac").await;
    let pc = client("pc");
    let mut rx = mac.subscribe();
    pc.pair(mac_addr, || async move { Ok(next_code(&mut rx).await) })
        .await
        .unwrap();
    let mac_fp = mac.identity().fingerprint();
    let pc_fp = pc.identity().fingerprint();

    // Both sides rename while a session is open; the keepalive carries it.
    let mut session = pc.connect(mac_addr).await.unwrap();
    mac.set_name("Studio Mac");
    pc.set_name("Desk PC");
    session.ping().await.unwrap();
    assert_eq!(session.announced_name.as_deref(), Some("Studio Mac"));
    assert_eq!(mac.trust().get(&pc_fp).unwrap().name, "Desk PC");

    // A name chosen locally wins over what the peer announces...
    pc.rename_peer(&mac_fp, Some("Mini")).unwrap();
    session.close().await.unwrap();
    let session = pc.connect(mac_addr).await.unwrap();
    assert_eq!(session.peer.name, "Mini");
    assert_eq!(pc.trust().get(&mac_fp).unwrap().name, "Mini");
    session.close().await.unwrap();

    // ...until it is cleared, then the peer's own name comes back.
    pc.rename_peer(&mac_fp, None).unwrap();
    let session = pc.connect(mac_addr).await.unwrap();
    assert_eq!(session.peer.name, "Studio Mac");
    session.close().await.unwrap();
}
