//! The input channel end to end on loopback, with a recording target in
//! place of the OS injector.

use da_core::da_input::clipboard::Clip;
use da_core::da_input::{Frame, InputEvent, MouseButton};
use da_core::{Config, Event, Identity, InputTarget, Node, Permissions, TrustStore, TrustedPeer};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;

#[derive(Default)]
struct Recorder {
    frames: Mutex<Vec<Frame>>,
    ended: Mutex<u32>,
    clipboard: String,
}

impl InputTarget for Recorder {
    fn frame(&self, _peer: &TrustedPeer, frame: Frame) -> Option<Frame> {
        if frame == Frame::ClipboardRequest {
            return Some(Frame::ClipboardText(self.clipboard.clone()));
        }
        self.frames.lock().unwrap().push(frame);
        None
    }
    fn end(&self, _peer: &TrustedPeer) {
        *self.ended.lock().unwrap() += 1;
    }
}

async fn paired() -> (Arc<Node>, SocketAddr, Arc<Node>) {
    let mac = Node::new(
        Identity::generate("mac").unwrap(),
        TrustStore::in_memory(),
        Config::default(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(mac.clone().serve(listener));
    let pc = Node::new(
        Identity::generate("pc").unwrap(),
        TrustStore::in_memory(),
        Config::default(),
    )
    .unwrap();
    let mut rx = mac.subscribe();
    pc.pair(addr, || async move {
        loop {
            if let Ok(Event::PairCodeShown { code, .. }) = rx.recv().await {
                return Ok(code);
            }
        }
    })
    .await
    .unwrap();
    (mac, addr, pc)
}

async fn wait_for(rec: &Recorder, n: usize) {
    let t0 = Instant::now();
    while rec.frames.lock().unwrap().len() < n {
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "timed out waiting for {n} frames"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn events_arrive_in_order_and_clipboard_round_trips() {
    let (mac, addr, pc) = paired().await;
    let rec = Arc::new(Recorder {
        clipboard: "from the mac".into(),
        ..Default::default()
    });
    mac.set_input_target(Some(rec.clone()));

    let link = pc
        .open_input(addr)
        .await
        .expect("paired peer with permission can open input");
    let sent = vec![
        InputEvent::Key {
            usage: 0x04,
            down: true,
        },
        InputEvent::Key {
            usage: 0x04,
            down: false,
        },
        InputEvent::MouseMove { dx: 12, dy: -3 },
        InputEvent::Button {
            button: MouseButton::Left,
            down: true,
        },
        InputEvent::Button {
            button: MouseButton::Left,
            down: false,
        },
        InputEvent::Wheel { dx: 0, dy: -120 },
    ];
    for e in &sent {
        assert!(link.send_event(*e));
    }
    link.send(Frame::ClipboardText("from the pc".into()));
    wait_for(&rec, sent.len() + 1).await;
    let got = rec.frames.lock().unwrap().clone();
    let mut want: Vec<Frame> = sent.iter().map(|e| Frame::Input(*e)).collect();
    want.push(Frame::ClipboardText("from the pc".into()));
    assert_eq!(got, want);

    let text = link.request_clipboard(Duration::from_secs(2)).await;
    assert_eq!(text, Some(Clip::Text("from the mac".into())));

    // Dropping the link sends ReleaseAll and the target is told it ended.
    drop(link);
    let t0 = Instant::now();
    while *rec.ended.lock().unwrap() == 0 {
        assert!(t0.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        rec.frames.lock().unwrap().last(),
        Some(&Frame::Input(InputEvent::ReleaseAll))
    );
}

#[tokio::test]
async fn refused_without_permission_or_target() {
    let (mac, addr, pc) = paired().await;
    // No input target set on the Mac.
    let err = pc
        .open_input(addr)
        .await
        .err()
        .expect("refused without a target");
    assert!(err.to_string().contains("cannot receive"), "{err:#}");

    // Target set, but the Mac turns off input permission for the PC.
    mac.set_input_target(Some(Arc::new(Recorder::default())));
    let fp = pc.identity().fingerprint();
    mac.trust()
        .set_permissions(
            &fp,
            Permissions {
                input_takeover: false,
                ..Permissions::default()
            },
        )
        .unwrap();
    let err = pc
        .open_input(addr)
        .await
        .err()
        .expect("refused without permission");
    assert!(err.to_string().contains("does not allow"), "{err:#}");
}

#[tokio::test]
async fn unpaired_device_cannot_send_input() {
    let (mac, addr, _pc) = paired().await;
    mac.set_input_target(Some(Arc::new(Recorder::default())));
    let stranger = Node::new(
        Identity::generate("x").unwrap(),
        TrustStore::in_memory(),
        Config::default(),
    )
    .unwrap();
    assert!(stranger.open_input(addr).await.is_err());
}

struct Notifier(tokio::sync::mpsc::UnboundedSender<()>);

impl InputTarget for Notifier {
    fn frame(&self, _peer: &TrustedPeer, _frame: Frame) -> Option<Frame> {
        let _ = self.0.send(());
        None
    }
    fn end(&self, _peer: &TrustedPeer) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_latency_is_sub_millisecond() {
    let (mac, addr, pc) = paired().await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    mac.set_input_target(Some(Arc::new(Notifier(tx))));
    let link = pc.open_input(addr).await.unwrap();
    link.send_event(InputEvent::MouseMove { dx: 1, dy: 0 });
    rx.recv().await.unwrap(); // warm up
    let mut times: Vec<Duration> = Vec::new();
    for _ in 0..500 {
        let t0 = Instant::now();
        link.send_event(InputEvent::MouseMove { dx: 1, dy: 0 });
        rx.recv().await.unwrap();
        times.push(t0.elapsed());
    }
    times.sort();
    let median = times[times.len() / 2];
    let p99 = times[times.len() * 99 / 100];
    println!("input latency on loopback (capture queue to target, through TLS): median {median:?}, p99 {p99:?}");
    // Generous bound so a busy CI machine passes; typical is well under 0.2 ms.
    assert!(median < Duration::from_millis(1), "median {median:?}");
}

/// Bounces any Enter straight back as a Leave, like a target whose pointer
/// was pushed out over the edge.
#[derive(Default)]
struct Bouncer {
    back: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Frame>>>,
}

impl InputTarget for Bouncer {
    fn begin(&self, _peer: &TrustedPeer, back: tokio::sync::mpsc::UnboundedSender<Frame>) {
        *self.back.lock().unwrap() = Some(back);
    }
    fn frame(&self, _peer: &TrustedPeer, frame: Frame) -> Option<Frame> {
        if let Frame::Enter { edge, pos, .. } = frame {
            let back = self.back.lock().unwrap();
            let _ = back.as_ref().unwrap().send(Frame::Leave { edge, pos });
        }
        None
    }
    fn end(&self, _peer: &TrustedPeer) {}
}

#[tokio::test]
async fn pushing_out_over_the_edge_reaches_the_controller() {
    use da_core::da_input::Edge;
    let (mac, addr, pc) = paired().await;
    mac.set_input_target(Some(Arc::new(Bouncer::default())));
    let link = pc.open_input(addr).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    link.on_leave(move |pos| {
        let _ = tx.send(pos);
    });
    assert!(link.send(Frame::Enter {
        edge: Edge::Right,
        pos: 1234,
        warp: true
    }));
    let pos = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("leave arrives")
        .unwrap();
    assert_eq!(pos, 1234);
}
