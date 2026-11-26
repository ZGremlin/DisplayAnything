import AVFoundation
import Foundation

/// Receives frames from the Rust core (on its background threads) and feeds
/// them to the hardware decoder through the video view.
final class StreamSink: FrameSink, @unchecked Sendable {
    /// Called on the main thread with the stream's state changes.
    var onState: ((StreamState) -> Void)?
    private let view: StreamView

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
