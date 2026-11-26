import AVFoundation
import SwiftUI
import UIKit

/// A view whose layer is an AVSampleBufferDisplayLayer: H.264 is decoded in
/// hardware and shown as soon as it arrives ("display immediately"), with no
/// frame buffering in the app.
final class StreamView: UIView {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }

    private var displayLayer: AVSampleBufferDisplayLayer { layer as! AVSampleBufferDisplayLayer }
    private var formatDescription: CMVideoFormatDescription?
    private var lastFormat: (Data, Data)?
    /// Set when the decoder fails; the viewer asks the desktop for a keyframe.
    var needsKeyframe: (() -> Void)?

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .black
        displayLayer.videoGravity = .resizeAspect
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    func setFormat(sps: Data, pps: Data) {
        if let last = lastFormat, last.0 == sps, last.1 == pps { return }
        var description: CMVideoFormatDescription?
        let status = sps.withUnsafeBytes { spsRaw -> OSStatus in
            pps.withUnsafeBytes { ppsRaw -> OSStatus in
                let pointers: [UnsafePointer<UInt8>] = [
                    spsRaw.bindMemory(to: UInt8.self).baseAddress!,
                    ppsRaw.bindMemory(to: UInt8.self).baseAddress!,
                ]
                let sizes = [sps.count, pps.count]
                return pointers.withUnsafeBufferPointer { p in
                    sizes.withUnsafeBufferPointer { s in
                        CMVideoFormatDescriptionCreateFromH264ParameterSets(
                            allocator: kCFAllocatorDefault,
                            parameterSetCount: 2,
                            parameterSetPointers: p.baseAddress!,
                            parameterSetSizes: s.baseAddress!,
                            nalUnitHeaderLength: 4,
                            formatDescriptionOut: &description)
                    }
                }
            }
        }
        guard status == noErr else { return }
        formatDescription = description
        lastFormat = (sps, pps)
    }

    /// Called from the core's thread. The frame is AVCC: 4-byte lengths.
    func enqueue(avcc: Data, ptsUs: UInt64, keyframe: Bool) {
        guard let format = formatDescription else {
            needsKeyframe?()
            return
        }
        if displayLayer.status == .failed {
            displayLayer.flush()
            needsKeyframe?()
            return
        }
        let count = avcc.count
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: count,
            blockAllocator: nil, customBlockSource: nil, offsetToData: 0,
            dataLength: count, flags: kCMBlockBufferAssureMemoryNowFlag,
            blockBufferOut: &block) == noErr, let block else { return }
        let copied = avcc.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block,
                                          offsetIntoDestination: 0, dataLength: count)
        }
        guard copied == noErr else { return }

        var timing = CMSampleTimingInfo(
            duration: .invalid,
            presentationTimeStamp: CMTime(value: CMTimeValue(ptsUs), timescale: 1_000_000),
            decodeTimeStamp: .invalid)
        var size = count
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format,
            sampleCount: 1, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
            sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample) == noErr,
            let sample else { return }

        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true)
            as? [NSMutableDictionary], let first = attachments.first {
            first[kCMSampleAttachmentKey_DisplayImmediately as String] = true
            if !keyframe { first[kCMSampleAttachmentKey_NotSync as String] = true }
        }
        // The layer's enqueue is deprecated from iOS 18 in favour of its
        // sampleBufferRenderer, but works on every version this app supports.
        displayLayer.enqueue(sample)
    }

    func reset() {
        displayLayer.flushAndRemoveImage()
        formatDescription = nil
        lastFormat = nil
    }
}

/// SwiftUI wrapper. Keeps the view and hands it to the stream sink.
struct StreamLayerView: UIViewRepresentable {
    let onView: (StreamView) -> Void

    func makeUIView(context: Context) -> StreamView {
        let view = StreamView()
        onView(view)
        return view
    }

    func updateUIView(_ uiView: StreamView, context: Context) {}
}
