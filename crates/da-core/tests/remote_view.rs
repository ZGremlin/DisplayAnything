//! Remote screen control end to end on loopback, with a fake screen in
//! place of the real capture and encoder.

use da_core::da_screen::host::Sink;
use da_core::da_screen::wire::StartRequest;
use da_core::da_screen::{Codec, EncodedFrame, StreamEvent, StreamInfo, ViewMsg};
use da_core::{Config, Event, Identity, Node, Permissions, ScreenSource, TrustStore};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;

async fn paired() -> (Arc<Node>, SocketAddr, Arc<Node>) {
    let host = Node::new(
        Identity::generate("host").unwrap(),
        TrustStore::in_memory(),
        Config::default(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(host.clone().serve(listener));
    let viewer = Node::new(
        Identity::generate("viewer").unwrap(),
        TrustStore::in_memory(),
        Config::default(),
    )
    .unwrap();
    let mut rx = host.subscribe();
    viewer
        .pair(addr, || async move {
            loop {
                if let Ok(Event::PairCodeShown { code, .. }) = rx.recv().await {
                    return Ok(code);
                }
            }
        })
        .await
        .unwrap();
    (host, addr, viewer)
}

fn allow_remote(host: &Node, viewer: &Node) {
    let fp = viewer.identity().fingerprint();
    let perms = Permissions {
        remote_control: true,
        ..host.trust().get(&fp).unwrap().permissions
    };
    host.trust().set_permissions(&fp, perms).unwrap();
}

/// A fake screen: sends its info and a keyframe, then a frame for every
/// keyframe request.
struct FakeScreen {
    sink: Arc<Mutex<Sink>>,
    requests: Arc<AtomicU32>,
}

impl ScreenSource for FakeScreen {
    fn request_keyframe(&self) {
        let n = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        (self.sink.lock().unwrap())(StreamEvent::Frame(EncodedFrame {
            keyframe: true,
            pts_us: n as u64,
            data: vec![0, 0, 0, 1, 0x65, n as u8],
        }));
    }
}

fn fake_starter(requests: Arc<AtomicU32>, seen: Arc<Mutex<Option<u32>>>) -> da_core::ScreenStarter {
    Arc::new(move |opts, mut sink: Sink| {
        *seen.lock().unwrap() = Some(opts.fps);
        sink(StreamEvent::Info(StreamInfo {
            width: 1920,
            height: 1080,
            codec: Codec::H264,
            displays: 1,
        }));
        sink(StreamEvent::Frame(EncodedFrame {
            keyframe: true,
            pts_us: 0,
            data: vec![0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65, 0],
        }));
        Ok(Box::new(FakeScreen {
            sink: Arc::new(Mutex::new(sink)),
            requests: requests.clone(),
        }) as Box<dyn ScreenSource>)
    })
}

async fn next(view: &mut da_core::RemoteView) -> ViewMsg {
    timeout(Duration::from_secs(5), view.next())
        .await
        .expect("timed out waiting for the host")
        .expect("stream closed")
}

#[tokio::test]
async fn refused_until_the_host_allows_remote_control() {
    let (_host, addr, viewer) = paired().await;
    let err = viewer
        .open_remote(addr, StartRequest::default())
        .await
        .err()
        .expect("should be refused");
    assert!(
        err.to_string().contains("does not allow"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn streams_frames_and_answers_keyframe_requests() {
    let (host, addr, viewer) = paired().await;
    allow_remote(&host, &viewer);
    let requests = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(None));
    host.set_screen_starter(Some(fake_starter(requests.clone(), seen.clone())));
    let mut events = host.subscribe();

    let mut view = viewer
        .open_remote(
            addr,
            StartRequest {
                fps: 30,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    match next(&mut view).await {
        ViewMsg::Info(i) => assert_eq!((i.width, i.height), (1920, 1080)),
        other => panic!("expected info, got {other:?}"),
    }
    match next(&mut view).await {
        ViewMsg::Video(f) => assert!(f.keyframe),
        other => panic!("expected a frame, got {other:?}"),
    }
    assert_eq!(*seen.lock().unwrap(), Some(30));

    view.control().request_keyframe();
    match next(&mut view).await {
        ViewMsg::Video(f) => assert_eq!(f.pts_us, 1),
        other => panic!("expected a frame, got {other:?}"),
    }
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    // The host's UI hears about the session starting and ending.
    let active = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Event::RemoteViewed { active, .. }) = events.recv().await {
                return active;
            }
        }
    })
    .await
    .unwrap();
    assert!(active);
    drop(view);
    let ended = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Event::RemoteViewed { active: false, .. }) = events.recv().await {
                return true;
            }
        }
    })
    .await
    .unwrap();
    assert!(ended);
}
