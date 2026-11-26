//! The phone's client core against a desktop on loopback, with a fake
//! screen in place of capture and encoding. Covers what the iOS app does:
//! pair with a code, be refused until allowed, view, ask for a keyframe,
//! stop.

use da_core::da_screen::host::Sink;
use da_core::da_screen::{Codec, EncodedFrame, StreamEvent, StreamInfo};
use da_core::identity::Fingerprint;
use da_core::{Config, Event, Identity, Node, Permissions, ScreenSource, TrustStore};
use da_mobile::{
    generate_identity, Client, DaError, FrameSink, IdentityBytes, PairingListener, ScreenRequest,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// SPS, PPS and an IDR slice in Annex B, as the desktop's encoder emits them.
const KEYFRAME: &[u8] = &[
    0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, // SPS
    0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, // PPS
    0, 0, 0, 1, 0x65, 0x88, 0x84, // IDR
];

struct FakeScreen {
    sink: Arc<Mutex<Sink>>,
    requests: Arc<AtomicU32>,
}

impl ScreenSource for FakeScreen {
    fn request_keyframe(&self) {
        let n = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        (self.sink.lock().unwrap())(StreamEvent::Frame(EncodedFrame {
            keyframe: true,
            pts_us: u64::from(n) * 1000,
            data: KEYFRAME.to_vec(),
        }));
    }
}

async fn desktop() -> (Arc<Node>, std::net::SocketAddr) {
    let node = Node::new(
        Identity::generate("studio-mac").unwrap(),
        TrustStore::in_memory(),
        Config::default(),
    )
    .unwrap();
    node.set_screen_starter(Some(Arc::new(|_opts, mut sink: Sink| {
        sink(StreamEvent::Info(StreamInfo {
            width: 2560,
            height: 1440,
            codec: Codec::H264,
            displays: 1,
            display: 0,
            audio: false,
        }));
        sink(StreamEvent::Frame(EncodedFrame {
            keyframe: true,
            pts_us: 0,
            data: KEYFRAME.to_vec(),
        }));
        Ok(Box::new(FakeScreen {
            sink: Arc::new(Mutex::new(sink)),
            requests: Arc::new(AtomicU32::new(0)),
        }) as Box<dyn ScreenSource>)
    })));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(node.clone().serve(listener));
    (node, addr)
}

#[derive(Debug, PartialEq)]
enum Ev {
    Info(u32, u32, u32, u32),
    Format(Vec<u8>, Vec<u8>),
    Frame(Vec<u8>, u64, bool),
    Error(String),
    Closed,
}

struct Sinks(mpsc::UnboundedSender<Ev>);

impl FrameSink for Sinks {
    fn on_info(&self, width: u32, height: u32, displays: u32, display: u32) {
        let _ = self.0.send(Ev::Info(width, height, displays, display));
    }
    fn on_format(&self, sps: Vec<u8>, pps: Vec<u8>) {
        let _ = self.0.send(Ev::Format(sps, pps));
    }
    fn on_frame(&self, avcc: Vec<u8>, pts_us: u64, keyframe: bool) {
        let _ = self.0.send(Ev::Frame(avcc, pts_us, keyframe));
    }
    fn on_error(&self, message: String) {
        let _ = self.0.send(Ev::Error(message));
    }
    fn on_closed(&self) {
        let _ = self.0.send(Ev::Closed);
    }
}

struct NoListener;
impl PairingListener for NoListener {
    fn code_needed(&self) {}
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Ev>) -> Ev {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for the stream")
        .expect("stream closed early")
}

fn phone_fingerprint(id: &IdentityBytes) -> String {
    Fingerprint::of(&id.cert_der).to_hex()
}

/// Pair a fresh phone with `desktop`, typing in the code the desktop shows.
async fn paired_phone(
    desktop: &Arc<Node>,
    addr: std::net::SocketAddr,
    dir: &std::path::Path,
) -> (Arc<Client>, IdentityBytes) {
    let ident = generate_identity("ipad".into()).unwrap();
    let phone = Client::new(
        dir.to_string_lossy().into_owned(),
        "ipad".into(),
        ident.clone(),
    )
    .unwrap();

    let mut events = desktop.subscribe();
    let typer = phone.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(Event::PairCodeShown { code, .. }) = events.recv().await {
                typer.submit_pairing_code(code);
                return;
            }
        }
    });

    let saved = phone
        .pair(addr.to_string(), Arc::new(NoListener))
        .await
        .expect("pairing with the right code succeeds");
    assert_eq!(saved.name, "studio-mac");
    assert_eq!(saved.fingerprint, desktop.identity().fingerprint().to_hex());
    assert_eq!(saved.address.as_deref(), Some("127.0.0.1"));
    assert_eq!(saved.port, addr.port());
    (phone, ident)
}

#[tokio::test]
async fn pairs_views_asks_for_a_keyframe_and_stops() {
    let (desktop, addr) = desktop().await;
    let dir = tempfile::tempdir().unwrap();
    let (phone, ident) = paired_phone(&desktop, addr, dir.path()).await;

    // The phone is saved, and the desktop saved it view-only.
    assert_eq!(phone.saved_desktops().len(), 1);
    let fp = phone_fingerprint(&ident);
    let saved_on_desktop = desktop
        .trust()
        .get(&Fingerprint::from_hex(&fp).unwrap())
        .cloned()
        .unwrap();
    assert!(saved_on_desktop.is_viewer_only());
    assert!(!saved_on_desktop.auto_connect);

    // Refused until the desktop allows remote control for this phone.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let request = ScreenRequest {
        display: 0,
        fps: 60,
        bitrate_kbps: 15_000,
        max_width: 1024,
        max_height: 768,
        audio: false,
    };
    let refused = phone
        .open_screen(
            phone.saved_desktops()[0].fingerprint.clone(),
            request.clone(),
            Arc::new(Sinks(tx.clone())),
        )
        .await;
    match refused {
        Err(DaError::NotAllowed { message }) => assert!(message.contains("does not allow")),
        Err(other) => panic!("expected NotAllowed, got {other:?}"),
        Ok(_) => panic!("the desktop should refuse until remote control is allowed"),
    }

    desktop
        .trust()
        .set_permissions(
            &Fingerprint::from_hex(&fp).unwrap(),
            Permissions {
                remote_control: true,
                display_takeover: false,
                input_takeover: false,
            },
        )
        .unwrap();

    let screen = phone
        .open_screen(
            phone.saved_desktops()[0].fingerprint.clone(),
            request,
            Arc::new(Sinks(tx)),
        )
        .await
        .expect("allowed now");

    assert_eq!(next(&mut rx).await, Ev::Info(2560, 1440, 1, 0));
    let sps = vec![0x67, 0x42, 0x00, 0x1f];
    let pps = vec![0x68, 0xce, 0x3c, 0x80];
    assert_eq!(next(&mut rx).await, Ev::Format(sps, pps));
    // Frames come out as AVCC: the parameter sets are gone and the IDR slice
    // has a 4-byte length in front.
    assert_eq!(
        next(&mut rx).await,
        Ev::Frame(vec![0, 0, 0, 3, 0x65, 0x88, 0x84], 0, true)
    );

    screen.request_keyframe();
    match next(&mut rx).await {
        Ev::Frame(_, pts, true) => assert_eq!(pts, 1000),
        other => panic!("expected a keyframe after asking, got {other:?}"),
    }

    screen.stop();
    assert_eq!(next(&mut rx).await, Ev::Closed);
}

#[tokio::test]
async fn unknown_desktop_is_not_paired() {
    let dir = tempfile::tempdir().unwrap();
    let ident = generate_identity("ipad".into()).unwrap();
    let phone = Client::new(
        dir.path().to_string_lossy().into_owned(),
        "ipad".into(),
        ident,
    )
    .unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let err = phone
        .open_screen(
            Fingerprint([9; 32]).to_hex(),
            ScreenRequest {
                display: 0,
                fps: 60,
                bitrate_kbps: 15_000,
                max_width: 0,
                max_height: 0,
                audio: false,
            },
            Arc::new(Sinks(tx)),
        )
        .await
        .err()
        .expect("nothing is paired");
    assert!(matches!(err, DaError::NotPaired { .. }), "got {err:?}");
}

#[tokio::test]
async fn wrong_code_is_a_pairing_failure_and_nothing_is_saved() {
    let (desktop, addr) = desktop().await;
    let dir = tempfile::tempdir().unwrap();
    let ident = generate_identity("ipad".into()).unwrap();
    let phone = Client::new(
        dir.path().to_string_lossy().into_owned(),
        "ipad".into(),
        ident,
    )
    .unwrap();

    let mut events = desktop.subscribe();
    let typer = phone.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(Event::PairCodeShown { code, .. }) = events.recv().await {
                let wrong = if code == "000000" { "111111" } else { "000000" };
                typer.submit_pairing_code(wrong.to_string());
                return;
            }
        }
    });
    let err = phone
        .pair(addr.to_string(), Arc::new(NoListener))
        .await
        .expect_err("a wrong code must not pair");
    assert!(matches!(err, DaError::PairingFailed { .. }), "got {err:?}");
    assert!(phone.saved_desktops().is_empty());
    assert!(desktop.trust().peers().is_empty());
}

#[tokio::test]
async fn saved_desktops_survive_a_restart_and_can_be_forgotten() {
    let (desktop, addr) = desktop().await;
    let dir = tempfile::tempdir().unwrap();
    let (phone, ident) = paired_phone(&desktop, addr, dir.path()).await;
    drop(phone);

    // Relaunch: same identity from the Keychain, same data directory.
    let phone = Client::new(
        dir.path().to_string_lossy().into_owned(),
        "ipad".into(),
        ident,
    )
    .unwrap();
    let saved = phone.saved_desktops();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved[0].fingerprint,
        desktop.identity().fingerprint().to_hex()
    );

    // Forget the desktop (not this phone's own fingerprint).
    phone
        .forget(desktop.identity().fingerprint().to_hex())
        .unwrap();
    assert!(phone.saved_desktops().is_empty());
}

#[test]
fn creates_its_data_folder_when_missing() {
    // A first launch: the app's own folder does not exist yet.
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("DisplayAnything");
    let ident = generate_identity("ipad".into()).unwrap();
    let phone = Client::new(dir.to_string_lossy().into_owned(), "ipad".into(), ident)
        .expect("the folder is created on first launch");
    assert!(phone.saved_desktops().is_empty());
    assert!(dir.is_dir());
}
