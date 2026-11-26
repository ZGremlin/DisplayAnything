//! A phone or tablet (device kind `IosViewer`) pairs with a desktop: it is
//! saved view-only, never auto-connected, and gets no takeover permissions.

use da_core::protocol::DeviceKind;
use da_core::{Config, Event, Identity, Node, TrustStore};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// A screen that never sends anything: enough for the handshake, which is
/// what these tests check.
struct NoScreen;

impl da_core::ScreenSource for NoScreen {
    fn request_keyframe(&self) {}
}

async fn desktop(name: &str) -> (Arc<Node>, SocketAddr) {
    let node = Node::new(
        Identity::generate(name).unwrap(),
        TrustStore::in_memory(),
        Config::default(),
    )
    .unwrap();
    node.set_screen_starter(Some(Arc::new(|_opts, _sink| {
        Ok(Box::new(NoScreen) as Box<dyn da_core::ScreenSource>)
    })));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(node.clone().serve(listener));
    (node, addr)
}

fn phone(name: &str) -> Arc<Node> {
    Node::new(
        Identity::generate(name).unwrap(),
        TrustStore::in_memory(),
        Config {
            device_kind: DeviceKind::IosViewer,
            ..Config::default()
        },
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
async fn phone_is_saved_view_only_and_never_dialled() {
    let (pc, pc_addr) = desktop("pc").await;
    let ipad = phone("ipad");
    let mut rx = pc.subscribe();

    ipad.pair(pc_addr, || async move { Ok(next_code(&mut rx).await) })
        .await
        .expect("pairing with the right code succeeds");

    let saved = pc
        .trust()
        .get(&ipad.identity().fingerprint())
        .cloned()
        .unwrap();
    assert_eq!(saved.device_kind, DeviceKind::IosViewer);
    assert!(!saved.auto_connect, "the desktop must never dial the phone");
    assert!(saved.is_viewer_only());
    assert_eq!(
        saved.permissions,
        da_core::Permissions {
            remote_control: false,
            display_takeover: false,
            input_takeover: false,
        },
        "a phone gets only the remote control permission, and that starts off"
    );
}

#[tokio::test]
async fn phone_must_be_allowed_before_it_can_view() {
    let (pc, pc_addr) = desktop("pc").await;
    let ipad = phone("ipad");
    let mut rx = pc.subscribe();
    ipad.pair(pc_addr, || async move { Ok(next_code(&mut rx).await) })
        .await
        .unwrap();

    let Err(err) = ipad.open_remote(pc_addr, Default::default()).await else {
        panic!("remote control is off by default");
    };
    assert!(err.to_string().contains("does not allow"), "got: {err:#}");

    let fp = ipad.identity().fingerprint();
    let mut perms = pc.trust().get(&fp).unwrap().permissions;
    perms.remote_control = true;
    pc.trust().set_permissions(&fp, perms).unwrap();
    ipad.open_remote(pc_addr, Default::default())
        .await
        .expect("allowed once the desktop turns remote control on");
}

#[tokio::test]
async fn desktop_pairing_is_unchanged() {
    let (mac, mac_addr) = desktop("mac").await;
    let pc = Node::new(
        Identity::generate("pc").unwrap(),
        TrustStore::in_memory(),
        Config::default(),
    )
    .unwrap();
    let mut rx = mac.subscribe();
    pc.pair(mac_addr, || async move { Ok(next_code(&mut rx).await) })
        .await
        .unwrap();
    let saved = mac
        .trust()
        .get(&pc.identity().fingerprint())
        .cloned()
        .unwrap();
    assert_eq!(saved.device_kind, DeviceKind::Desktop);
    assert!(saved.auto_connect);
    assert!(saved.permissions.display_takeover && saved.permissions.input_takeover);
}
