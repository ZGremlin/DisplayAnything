//! macOS: record the system's sound with ScreenCaptureKit (macOS 13+). It
//! is a stream of its own, separate from the picture, so changing monitor
//! or resolution doesn't interrupt the sound. ScreenCaptureKit always
//! wants a display to film, so this stream asks for a tiny, slow picture
//! and ignores it. Needs the Screen Recording permission, like the
//! picture. Our own process is left out so nothing echoes.

use super::{AudioEvent, AudioSink, Chunker};
use crate::host::macos::{describe, shareable_content, SendBox};
use anyhow::{anyhow, bail, Result};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_audio_types::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, AudioBuffer,
    AudioStreamBasicDescription,
};
use objc2_core_graphics::CGPreflightScreenCaptureAccess;
use objc2_core_media::{
    CMAudioFormatDescriptionGetStreamBasicDescription, CMBlockBuffer, CMSampleBuffer, CMTime,
    CMTimeFlags,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType,
};
use std::sync::mpsc::channel;
use std::sync::Mutex;
use std::time::Duration;

const RATE: u32 = 48_000;

pub(super) struct Capture {
    stream: SendBox<Retained<SCStream>>,
    _output: SendBox<Retained<AudioOutput>>,
    _queue: DispatchRetained<DispatchQueue>,
}

impl Capture {
    pub(super) fn start(mut sink: AudioSink) -> Result<Self> {
        if !CGPreflightScreenCaptureAccess() {
            bail!(
                "this Mac needs the Screen Recording permission to share its sound \
                 (System Settings > Privacy & Security > Screen Recording)"
            );
        }
        let content = shareable_content()?;
        let displays = unsafe { content.0.displays() };
        let display = displays
            .iter()
            .next()
            .ok_or_else(|| anyhow!("this Mac has no display to record sound with"))?;
        let chunker = Chunker::new(RATE);
        sink(AudioEvent::Format(chunker.format()));
        let stream = unsafe {
            let filter = SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            );
            let config = SCStreamConfiguration::new();
            // The picture is required but unused: keep it tiny and rare.
            config.setWidth(2);
            config.setHeight(2);
            config.setMinimumFrameInterval(CMTime {
                value: 1,
                timescale: 1,
                flags: CMTimeFlags(1),
                epoch: 0,
            });
            config.setShowsCursor(false);
            config.setCapturesAudio(true);
            config.setSampleRate(RATE as isize);
            config.setChannelCount(2);
            config.setExcludesCurrentProcessAudio(true);
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                None,
            )
        };
        let output = AudioOutput::new(sink, chunker);
        let queue = DispatchQueue::new("com.displayanything.audio", None);
        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Audio,
                    Some(&queue),
                )
                .map_err(|e| anyhow!("could not record this Mac's sound: {}", describe(&e)))?;
        }
        let (tx, rx) = channel::<Option<String>>();
        let block = RcBlock::new(move |err: *mut NSError| {
            let _ = tx.send(unsafe { err.as_ref() }.map(describe));
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&block)) };
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(None) => {}
            Ok(Some(e)) => bail!("sound capture did not start: {e}"),
            Err(_) => bail!("sound capture did not start in time"),
        }
        tracing::info!("sharing this Mac's sound");
        Ok(Capture {
            stream: SendBox(stream),
            _output: SendBox(output),
            _queue: queue,
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let (tx, rx) = channel::<()>();
        let block = RcBlock::new(move |_err: *mut NSError| {
            let _ = tx.send(());
        });
        unsafe { self.stream.0.stopCaptureWithCompletionHandler(Some(&block)) };
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
}

struct OutputIvars {
    sink: Mutex<(AudioSink, Chunker)>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "DAAudioStreamOutput"]
    #[ivars = OutputIvars]
    struct AudioOutput;

    unsafe impl NSObjectProtocol for AudioOutput {}

    unsafe impl SCStreamOutput for AudioOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Audio {
                return;
            }
            let mut g = self.ivars().sink.lock().unwrap();
            let (sink, chunker) = &mut *g;
            if let Err(e) = read_sample(sample, chunker, sink) {
                tracing::debug!("could not read a sound sample: {e:#}");
            }
        }
    }
);

impl AudioOutput {
    fn new(sink: AudioSink, chunker: Chunker) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars {
            sink: Mutex::new((sink, chunker)),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// Room for up to 8 channel buffers (ScreenCaptureKit gives one per
/// channel, as 32-bit float).
#[repr(C)]
struct BufferList {
    count: u32,
    buffers: [AudioBuffer; 8],
}

unsafe fn read_sample(
    sample: &CMSampleBuffer,
    chunker: &mut Chunker,
    sink: &mut AudioSink,
) -> Result<()> {
    let frames = sample.num_samples().max(0) as usize;
    if frames == 0 {
        return Ok(());
    }
    let fmt = sample
        .format_description()
        .ok_or_else(|| anyhow!("sound sample has no format"))?;
    let asbd: &AudioStreamBasicDescription =
        CMAudioFormatDescriptionGetStreamBasicDescription(&fmt)
            .as_ref()
            .ok_or_else(|| anyhow!("sound sample is not audio"))?;
    if asbd.mFormatFlags & kAudioFormatFlagIsFloat == 0 || asbd.mBitsPerChannel != 32 {
        bail!("unexpected sound format (flags {:#x})", asbd.mFormatFlags);
    }
    let mut list = BufferList {
        count: 0,
        buffers: [AudioBuffer {
            mNumberChannels: 0,
            mDataByteSize: 0,
            mData: std::ptr::null_mut(),
        }; 8],
    };
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    let status = sample.audio_buffer_list_with_retained_block_buffer(
        std::ptr::null_mut(),
        &mut list as *mut BufferList as *mut _,
        std::mem::size_of::<BufferList>(),
        None,
        None,
        0,
        &mut block,
    );
    if status != 0 {
        bail!("could not read the sound buffers ({status})");
    }
    // Keeps the sample memory alive while we read it.
    let _block =
        std::ptr::NonNull::new(block).map(|b| objc2_core_foundation::CFRetained::from_raw(b));
    let bufs = &list.buffers[..(list.count as usize).min(8)];
    let mut emit = |c| sink(AudioEvent::Chunk(c));
    if asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0 {
        // One buffer per channel: take the first two (mono plays on both).
        let plane = |b: &AudioBuffer| -> &[f32] {
            let n = (b.mDataByteSize as usize / 4).min(frames);
            if b.mData.is_null() {
                &[]
            } else {
                std::slice::from_raw_parts(b.mData as *const f32, n)
            }
        };
        let Some(first) = bufs.first() else {
            return Ok(());
        };
        let l = plane(first);
        let r = bufs.get(1).map_or(l, plane);
        for i in 0..l.len().min(r.len()) {
            chunker.push(l[i], r[i], &mut emit);
        }
    } else if let Some(b) = bufs.first() {
        if !b.mData.is_null() {
            let channels = (b.mNumberChannels as usize).max(1);
            let n = b.mDataByteSize as usize / 4;
            let s = std::slice::from_raw_parts(b.mData as *const f32, n);
            chunker.push_interleaved(s.iter().copied(), channels, emit);
        }
    }
    Ok(())
}
