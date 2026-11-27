import AVFoundation
import Foundation

/// Receives frames from the Rust core (on its background threads) and feeds
/// them to the hardware decoder through the video view.
final class StreamSink: FrameSink, @unchecked Sendable {
    /// Called on the main thread with the stream's state changes.
    var onState: ((StreamState) -> Void)?
    /// Called on the main thread about once a second while frames arrive.
    var onStats: ((StreamStats) -> Void)?
    private let view: StreamView

    // Frames and bytes since the last report (only touched on the Rust
    // core's frame thread, which delivers frames one at a time).
    private var frames = 0
    private var bytes = 0
    private var windowStart = Date()

    init(view: StreamView) {
        self.view = view
    }

    func onInfo(width: UInt32, height: UInt32, displays: UInt32, display: UInt32) {
        let size = CGSize(width: Int(width), height: Int(height))
        DispatchQueue.main.async { self.onState?(.live(size: size, displays: Int(displays), display: Int(display))) }
    }

    func onFormat(sps: Data, pps: Data) {
        view.setFormat(sps: sps, pps: pps)
    }

    func onFrame(avcc: Data, ptsUs: UInt64, keyframe: Bool) {
        view.enqueue(avcc: avcc, ptsUs: ptsUs, keyframe: keyframe)
        frames += 1
        bytes += avcc.count
        let elapsed = Date().timeIntervalSince(windowStart)
        if elapsed >= 1 {
            let stats = StreamStats(fps: Double(frames) / elapsed, mbps: Double(bytes) * 8 / elapsed / 1_000_000)
            frames = 0
            bytes = 0
            windowStart = Date()
            DispatchQueue.main.async { self.onStats?(stats) }
        }
    }

    func onError(message: String) {
        DispatchQueue.main.async { self.onState?(.failed(message)) }
    }

    func onClosed() {
        DispatchQueue.main.async { self.onState?(.closed) }
    }
}

enum StreamState: Equatable {
    case connecting
    case live(size: CGSize, displays: Int, display: Int)
    case failed(String)
    case closed
}

/// How the stream is doing, for the stats overlay.
struct StreamStats: Equatable {
    var fps: Double
    var mbps: Double
}
