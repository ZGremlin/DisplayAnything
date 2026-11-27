//! Remote screen control end to end on loopback, with a fake screen in
//! place of the real capture and encoder.

use da_core::da_input::{Frame, InputEvent};
use da_core::da_screen::host::Sink;
use da_core::da_screen::wire::StartRequest;
use da_core::da_screen::{Codec, DisplayBounds, EncodedFrame, StreamEvent, StreamInfo, ViewMsg};
use da_core::{
    Config, Event, Identity, InputTarget, Node, Permissions, ScreenSource, TrustStore, TrustedPeer,
};
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
        input_takeover: true,
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
    fn bounds(&self) -> Option<DisplayBounds> {
        Some(DisplayBounds {
            x: 100.0,
            y: 50.0,
            width: 1001.0,
            height: 501.0,
        })
    }

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
        let (w, h) = opts.target_size((1920, 1080));
        sink(StreamEvent::Info(StreamInfo {
            width: w,
            height: h,
            codec: Codec::H264,
            displays: 2,
            display: opts.display,
            audio: false,
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

/// Records what the host would replay.
#[derive(Default)]
struct Recorder {
    log: Mutex<Vec<String>>,
}

impl InputTarget for Recorder {
    fn begin(&self, _: &TrustedPeer, _: tokio::sync::mpsc::UnboundedSender<Frame>) {
        self.log.lock().unwrap().push("begin".into());
    }
    fn frame(&self, _: &TrustedPeer, frame: Frame) -> Option<Frame> {
        self.log.lock().unwrap().push(format!("{frame:?}"));
        None
    }
    fn end(&self, _: &TrustedPeer) {
        self.log.lock().unwrap().push("end".into());
    }
    fn pointer_at(&self, _: &TrustedPeer, x: f64, y: f64) {
        self.log.lock().unwrap().push(format!("at {x} {y}"));
    }
}

#[tokio::test]
async fn viewer_input_reaches_the_host_and_pauses_sharing() {
    let (host, addr, viewer) = paired().await;
    allow_remote(&host, &viewer);
    let rec = Arc::new(Recorder::default());
    host.set_input_target(Some(rec.clone()));
    host.set_screen_starter(Some(fake_starter(
        Arc::new(AtomicU32::new(0)),
        Arc::new(Mutex::new(None)),
    )));

    let mut view = viewer
        .open_remote(addr, StartRequest::default())
        .await
        .unwrap();
    next(&mut view).await; // info: the session is running
    assert!(host.being_viewed());

    // Shared keyboard and mouse is refused while the screen is controlled.
    let err = viewer
        .open_input(addr)
        .await
        .err()
        .expect("should be refused");
    assert!(err.to_string().contains("paused"), "unexpected: {err:#}");

    let control = view.control();
    control.send_input(InputEvent::Key {
        usage: 4,
        down: true,
    });
    control.pointer(65535, 0);
    control.pointer(0, 65535);
    let want = [
        "begin".to_string(),
        format!(
            "{:?}",
            Frame::Input(InputEvent::Key {
                usage: 4,
                down: true
            })
        ),
        "at 1100 50".into(),
        "at 100 550".into(),
    ];
    timeout(Duration::from_secs(5), async {
        while rec.log.lock().unwrap().len() < want.len() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("input did not arrive");
    assert_eq!(rec.log.lock().unwrap()[..], want[..]);

    drop(view);
    timeout(Duration::from_secs(5), async {
        while host.being_viewed()
            || rec.log.lock().unwrap().last().map(String::as_str) != Some("end")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session did not end");
}

#[tokio::test]
async fn viewer_can_switch_monitors() {
    let (host, addr, viewer) = paired().await;
    allow_remote(&host, &viewer);
    host.set_screen_starter(Some(fake_starter(
        Arc::new(AtomicU32::new(0)),
        Arc::new(Mutex::new(None)),
    )));
    let mut view = viewer
        .open_remote(addr, StartRequest::default())
        .await
        .unwrap();
    match next(&mut view).await {
        ViewMsg::Info(i) => assert_eq!((i.display, i.displays), (0, 2)),
        other => panic!("expected info, got {other:?}"),
    }
    view.control().select_display(1);
    // Frames from the old monitor may still be in flight; then the new one.
    let info = timeout(Duration::from_secs(5), async {
        loop {
            if let ViewMsg::Info(i) = next(&mut view).await {
                return i;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(info.display, 1);
}

#[tokio::test]
async fn viewer_can_change_size_and_quality() {
    let (host, addr, viewer) = paired().await;
    allow_remote(&host, &viewer);
    let seen = Arc::new(Mutex::new(None));
    host.set_screen_starter(Some(fake_starter(Arc::new(AtomicU32::new(0)), seen)));
    let mut view = viewer
        .open_remote(
            addr,
            StartRequest {
                max_width: 1280,
                max_height: 1280,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    match next(&mut view).await {
        ViewMsg::Info(i) => assert_eq!((i.width, i.height), (1280, 720)),
        other => panic!("expected info, got {other:?}"),
    }
    view.control()
        .configure(da_core::da_screen::wire::StreamSettings {
            max_width: 0,
            max_height: 0,
            bitrate_kbps: 80_000,
        });
    let info = timeout(Duration::from_secs(5), async {
        loop {
            if let ViewMsg::Info(i) = next(&mut view).await {
                return i;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!((info.width, info.height), (1920, 1080));
}

/// A fake sound source: sends its format and one chunk when started, and
/// counts how many are running.
fn fake_audio(running: Arc<AtomicU32>, starts: Arc<AtomicU32>) -> da_core::AudioStarter {
    use da_core::da_screen::audio::AudioEvent;
    use da_core::da_screen::wire::{AudioChunk, AudioFormat};
    struct Running(Arc<AtomicU32>);
    impl Drop for Running {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    Arc::new(move |mut sink| {
        let n = starts.fetch_add(1, Ordering::SeqCst) + 1;
        running.fetch_add(1, Ordering::SeqCst);
        sink(AudioEvent::Format(AudioFormat {
            sample_rate: 48_000,
            channels: 2,
        }));
        sink(AudioEvent::Chunk(AudioChunk {
            pts_us: 0,
            data: vec![n as u8; 8],
        }));
        Ok(Box::new(Running(running.clone())) as Box<dyn Send>)
    })
}

/// The next sound message, skipping video.
async fn next_audio(view: &mut da_core::RemoteView) -> ViewMsg {
    loop {
        if let m @ (ViewMsg::AudioInfo(_) | ViewMsg::Audio(_)) = next(view).await {
            return m;
        }
    }
}

#[tokio::test]
async fn sound_comes_only_when_asked_and_can_be_switched() {
    let (host, addr, viewer) = paired().await;
    allow_remote(&host, &viewer);
    host.set_screen_starter(Some(fake_starter(
        Arc::new(AtomicU32::new(0)),
        Arc::new(Mutex::new(None)),
    )));
    let running = Arc::new(AtomicU32::new(0));
    let starts = Arc::new(AtomicU32::new(0));
    host.set_audio_starter(Some(fake_audio(running.clone(), starts.clone())));

    // Sound off: none is recorded, but the host says it could.
    let mut view = viewer
        .open_remote(addr, StartRequest::default())
        .await
        .unwrap();
    match next(&mut view).await {
        ViewMsg::Info(i) => assert!(i.audio),
        other => panic!("expected info, got {other:?}"),
    }
    assert_eq!(starts.load(Ordering::SeqCst), 0);

    // Switched on mid-session: format, then sound.
    view.control().set_audio(true);
    match next_audio(&mut view).await {
        ViewMsg::AudioInfo(f) => assert_eq!((f.sample_rate, f.channels), (48_000, 2)),
        other => panic!("expected the sound format, got {other:?}"),
    }
    match next_audio(&mut view).await {
        ViewMsg::Audio(a) => assert_eq!(a.data, vec![1; 8]),
        other => panic!("expected sound, got {other:?}"),
    }
    assert_eq!(running.load(Ordering::SeqCst), 1);

    // Off again: the host stops recording.
    view.control().set_audio(false);
    timeout(Duration::from_secs(5), async {
        while running.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("sound capture did not stop");
    drop(view);

    // Asked for at the start: sound right away, stopped when the view ends.
    let mut view = viewer
        .open_remote(
            addr,
            StartRequest {
                audio: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(matches!(next_audio(&mut view).await, ViewMsg::AudioInfo(_)));
    assert!(matches!(next_audio(&mut view).await, ViewMsg::Audio(_)));
    drop(view);
    timeout(Duration::from_secs(5), async {
        while running.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("sound capture did not stop with the session");
}
