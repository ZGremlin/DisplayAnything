//! macOS host: ScreenCaptureKit hands us NV12 frames (IOSurface-backed, so
//! nothing is copied on the CPU), VideoToolbox encodes them in hardware
//! with low-latency rate control, and we convert the output to Annex B.
//!
//! Needs the Screen Recording permission.

use super::{HostOptions, Sink, StreamEvent};
use crate::h264;
use crate::wire::{Codec, EncodedFrame, StreamInfo};
use crate::DisplayInfo;
use anyhow::{anyhow, bail, Context, Result};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    kCGDisplayShowDuplicateLowResolutionModes, CGBeginDisplayConfiguration,
    CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
    CGConfigureDisplayWithDisplayMode, CGConfigureOption, CGDirectDisplayID, CGDisplayBounds,
    CGDisplayConfigRef, CGDisplayCopyAllDisplayModes, CGDisplayCopyDisplayMode, CGDisplayMode,
    CGError, CGGetActiveDisplayList, CGMainDisplayID, CGPreflightScreenCaptureAccess,
    CGRequestScreenCaptureAccess, CGRestorePermanentDisplayConfiguration,
};
use objc2_core_media::{
    kCMSampleAttachmentKey_NotSync, kCMTimeInvalid, kCMVideoCodecType_H264, CMSampleBuffer, CMTime,
    CMTimeFlags, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
};
use objc2_core_video::{kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, CVPixelBuffer};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType,
};
use objc2_video_toolbox::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_High_AutoLevel,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl, VTCompressionSession,
    VTEncodeInfoFlags, VTSessionSetProperty,
};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Everything the capture callback, the encoder callback and the keyframe
/// timer share.
struct State {
    sink: Mutex<Sink>,
    encoder: Mutex<Option<Encoder>>,
    keyframe: AtomicBool,
    started: Instant,
    /// The last captured picture and when it came, to re-send on a
    /// keyframe request while the screen is still.
    last: Mutex<Option<(CFRetained<CVPixelBuffer>, Instant)>>,
    stopped: AtomicBool,
}

// SAFETY: the CoreFoundation objects inside are only used behind mutexes,
// and VideoToolbox sessions and pixel buffers are thread-safe to retain and
// pass between threads.
unsafe impl Send for State {}
unsafe impl Sync for State {}

struct Encoder {
    session: CFRetained<VTCompressionSession>,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { self.session.invalidate() };
    }
}

pub(super) struct Host {
    bounds: Option<crate::DisplayBounds>,
    state: Arc<State>,
    stream: SendBox<Retained<SCStream>>,
    _output: SendBox<Retained<StreamOutput>>,
    _queue: DispatchRetained<DispatchQueue>,
    timer: Option<std::thread::JoinHandle<()>>,
    /// Puts the display's resolution back when the session ends (dropped
    /// after capture has stopped).
    _mode: Option<ModeGuard>,
    /// Keeps the display awake while it is being viewed (dropped last).
    _awake: Option<DisplayAwake>,
}

/// Lets Objective-C objects that are safe to message from any thread
/// (SCStream and our own output object) live in a Send struct.
struct SendBox<T>(T);
unsafe impl<T> Send for SendBox<T> {}
unsafe impl<T> Sync for SendBox<T> {}

impl Host {
    pub(super) fn start(opts: HostOptions, sink: Sink) -> Result<Self> {
        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess();
            bail!(
                "this Mac needs the Screen Recording permission for DisplayAnything \
                 (System Settings > Privacy & Security > Screen Recording)"
            );
        }
        // A Mac whose display has gone to sleep (or that was just woken
        // over the network, which leaves the display dark) lists no
        // screens to capture. Turn the display on first, as a key press
        // would, and give it a moment to come back.
        let awake = DisplayAwake::declare();
        let ids = wait_for_displays();
        let (display_index, wanted) = match ids.get(opts.display as usize) {
            Some(id) => (opts.display, *id),
            None => (0, CGMainDisplayID()),
        };
        // Switch the display to the resolution the viewer asked for, then
        // let it settle before looking at it.
        let mode = opts.max_size.and_then(|want| switch_mode(wanted, want));
        if mode.is_some() {
            std::thread::sleep(Duration::from_millis(1500));
        }
        let content = shareable_content_with_display()?;
        let displays = unsafe { content.0.displays() };
        let display: Retained<SCDisplay> = displays
            .iter()
            .find(|d| unsafe { d.displayID() } == wanted)
            .or_else(|| displays.iter().next())
            .ok_or_else(|| anyhow!("this Mac has no display to share"))?;
        let display_count = displays.count() as u32;

        // Capture at the panel's real pixel size, not its point size.
        let id = unsafe { display.displayID() };
        let (pw, ph) = pixel_size(id)
            .unwrap_or_else(|| unsafe { (display.width() as u32, display.height() as u32) });
        // VideoToolbox's H.264 encoder also tops out at 4096 wide.
        let (width, height) = super::fit_within(opts.target_size((pw, ph)), (4096, 4096));
        let b = CGDisplayBounds(id);
        let bounds = Some(crate::DisplayBounds {
            x: b.origin.x,
            y: b.origin.y,
            width: b.size.width,
            height: b.size.height,
        });

        let state = Arc::new(State {
            sink: Mutex::new(sink),
            encoder: Mutex::new(None),
            keyframe: AtomicBool::new(true),
            started: Instant::now(),
            last: Mutex::new(None),
            stopped: AtomicBool::new(false),
        });
        let encoder = Encoder::new(&state, width, height, &opts)?;
        *state.encoder.lock().unwrap() = Some(encoder);
        (state.sink.lock().unwrap())(StreamEvent::Info(StreamInfo {
            width,
            height,
            codec: Codec::H264,
            displays: display_count,
            display: display_index,
        }));

        let stream = unsafe {
            let filter = SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            );
            let config = SCStreamConfiguration::new();
            config.setWidth(width as usize);
            config.setHeight(height as usize);
            config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
            config.setMinimumFrameInterval(CMTime {
                value: 1,
                timescale: opts.fps as i32,
                flags: CMTimeFlags(1), // valid
                epoch: 0,
            });
            config.setShowsCursor(true);
            config.setQueueDepth(4);
            config.setCapturesAudio(false);
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                None,
            )
        };
        let output = StreamOutput::new(state.clone());
        let queue = DispatchQueue::new("com.displayanything.screen", None);
        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|e| anyhow!("could not capture the screen: {}", describe(&e)))?;
        }
        let (tx, rx) = channel::<Option<String>>();
        let block = RcBlock::new(move |err: *mut NSError| {
            let msg = unsafe { err.as_ref() }.map(describe);
            let _ = tx.send(msg);
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&block)) };
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(None) => {}
            Ok(Some(e)) => bail!("screen capture did not start: {e}"),
            Err(_) => bail!("screen capture did not start in time"),
        }
        tracing::info!(width, height, fps = opts.fps, "screen capture started");

        // Keyframe requests while nothing on screen changes: re-encode the
        // last picture (ScreenCaptureKit sends nothing for a still screen).
        let st = state.clone();
        let timer = std::thread::Builder::new()
            .name("da-screen-keyframe".into())
            .spawn(move || {
                while !st.stopped.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                    if !st.keyframe.load(Ordering::Relaxed) {
                        continue;
                    }
                    let last = st.last.lock().unwrap().clone();
                    if let Some((buf, at)) = last {
                        if at.elapsed() > Duration::from_millis(100) {
                            st.encode(&buf);
                        }
                    }
                }
            })?;

        Ok(Host {
            bounds,
            state,
            stream: SendBox(stream),
            _output: SendBox(output),
            _queue: queue,
            timer: Some(timer),
            _mode: mode,
            _awake: awake,
        })
    }

    pub(super) fn request_keyframe(&self) {
        self.state.keyframe.store(true, Ordering::Relaxed);
    }

    pub(super) fn bounds(&self) -> Option<crate::DisplayBounds> {
        self.bounds
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Relaxed);
        let (tx, rx) = channel::<()>();
        let block = RcBlock::new(move |_err: *mut NSError| {
            let _ = tx.send(());
        });
        unsafe { self.stream.0.stopCaptureWithCompletionHandler(Some(&block)) };
        let _ = rx.recv_timeout(Duration::from_secs(2));
        if let Some(t) = self.timer.take() {
            let _ = t.join();
        }
        // Invalidating the session waits for pending callbacks.
        self.state.encoder.lock().unwrap().take();
    }
}

impl State {
    fn encode(&self, buf: &CVPixelBuffer) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        let force = self.keyframe.swap(false, Ordering::Relaxed);
        let guard = self.encoder.lock().unwrap();
        let Some(enc) = guard.as_ref() else {
            return;
        };
        let pts = CMTime {
            value: self.started.elapsed().as_micros() as i64,
            timescale: 1_000_000,
            flags: CMTimeFlags(1),
            epoch: 0,
        };
        let props = force.then(|| {
            let key: &CFString = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
            CFDictionary::<CFString, CFType>::from_slices(&[key], &[CFBoolean::new(true).as_ref()])
        });
        let status = unsafe {
            enc.session.encode_frame(
                buf,
                pts,
                kCMTimeInvalid,
                props.as_deref().map(|d| d.as_opaque()),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            tracing::debug!(status, "VideoToolbox did not take a frame");
            if force {
                self.keyframe.store(true, Ordering::Relaxed);
            }
        }
    }
}

impl Encoder {
    fn new(state: &Arc<State>, width: u32, height: u32, opts: &HostOptions) -> Result<Self> {
        let spec_key: &CFString =
            unsafe { kVTVideoEncoderSpecification_EnableLowLatencyRateControl };
        let spec = CFDictionary::<CFString, CFType>::from_slices(
            &[spec_key],
            &[CFBoolean::new(true).as_ref()],
        );
        let mut out: *mut VTCompressionSession = std::ptr::null_mut();
        // The callback gets a weak-ish pointer to the shared state; the
        // session is invalidated (which waits for callbacks) before the
        // state can go away.
        let refcon = Arc::as_ptr(state) as *mut c_void;
        let status = unsafe {
            VTCompressionSession::create(
                None,
                width as i32,
                height as i32,
                kCMVideoCodecType_H264,
                Some(spec.as_opaque()),
                None,
                None,
                Some(on_encoded),
                refcon,
                NonNull::from(&mut out),
            )
        };
        if status != 0 || out.is_null() {
            bail!("the hardware video encoder did not start (VideoToolbox error {status})");
        }
        let session = unsafe { CFRetained::from_raw(NonNull::new_unchecked(out)) };
        let set = |key: &CFString, value: &CFType| {
            let s = unsafe { VTSessionSetProperty(&session, key, Some(value)) };
            if s != 0 {
                tracing::debug!(status = s, "encoder property not set");
            }
        };
        unsafe {
            set(
                kVTCompressionPropertyKey_RealTime,
                CFBoolean::new(true).as_ref(),
            );
            set(
                kVTCompressionPropertyKey_AllowFrameReordering,
                CFBoolean::new(false).as_ref(),
            );
            set(
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_High_AutoLevel.as_ref(),
            );
            set(
                kVTCompressionPropertyKey_AverageBitRate,
                CFNumber::new_i64(opts.bitrate_kbps as i64 * 1000).as_ref(),
            );
            set(
                kVTCompressionPropertyKey_ExpectedFrameRate,
                CFNumber::new_i32(opts.fps as i32).as_ref(),
            );
            set(
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                CFNumber::new_i32(opts.fps as i32 * 30).as_ref(),
            );
            let _ = session.prepare_to_encode_frames();
        }
        Ok(Encoder { session })
    }
}

/// VideoToolbox output: AVCC sample -> Annex B frame for the wire.
unsafe extern "C-unwind" fn on_encoded(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let Some(state) = (refcon as *const State).as_ref() else {
        return;
    };
    let Some(sample) = sample.as_ref() else {
        return;
    };
    if status != 0 {
        tracing::debug!(status, "VideoToolbox frame failed");
        return;
    }
    match annexb_from_sample(sample) {
        Ok(frame) => (state.sink.lock().unwrap())(StreamEvent::Frame(EncodedFrame {
            pts_us: state.started.elapsed().as_micros() as u64,
            ..frame
        })),
        Err(e) => tracing::debug!("could not read an encoded frame: {e:#}"),
    }
}

unsafe fn annexb_from_sample(sample: &CMSampleBuffer) -> Result<EncodedFrame> {
    let keyframe = is_sync(sample);
    let mut data = Vec::new();
    let mut len_size = 4usize;
    if let Some(fmt) = sample.format_description() {
        let mut count = 0usize;
        let mut nal_len: i32 = 4;
        let mut ptr: *const u8 = std::ptr::null();
        let mut size = 0usize;
        if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            &fmt,
            0,
            &mut ptr,
            &mut size,
            &mut count,
            &mut nal_len,
        ) == 0
        {
            len_size = nal_len.clamp(1, 4) as usize;
            if keyframe {
                for i in 0..count {
                    if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                        &fmt,
                        i,
                        &mut ptr,
                        &mut size,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    ) == 0
                        && !ptr.is_null()
                    {
                        h264::push_nal(&mut data, std::slice::from_raw_parts(ptr, size));
                    }
                }
            }
        }
    }
    let block = sample
        .data_buffer()
        .ok_or_else(|| anyhow!("encoded frame has no data"))?;
    let total = block.data_length();
    let mut bytes = vec![0u8; total];
    if total > 0 {
        let s = objc2_core_media::CMBlockBuffer::copy_data_bytes(
            &block,
            0,
            total,
            NonNull::new_unchecked(bytes.as_mut_ptr() as *mut c_void),
        );
        if s != 0 {
            bail!("could not copy encoded bytes ({s})");
        }
    }
    h264::avcc_to_annexb(&bytes, len_size, &mut data);
    Ok(EncodedFrame {
        keyframe,
        pts_us: 0,
        data,
    })
}

/// A sample is a sync (key) frame unless its attachments say NotSync.
unsafe fn is_sync(sample: &CMSampleBuffer) -> bool {
    let Some(arr) = sample.sample_attachments_array(false) else {
        return true;
    };
    if arr.count() == 0 {
        return true;
    }
    let dict = arr.value_at_index(0) as *const objc2_core_foundation::CFDictionary;
    let Some(dict) = dict.as_ref() else {
        return true;
    };
    let key: &CFString = kCMSampleAttachmentKey_NotSync;
    let v = dict.value(key as *const CFString as *const c_void) as *const CFBoolean;
    match v.as_ref() {
        Some(b) => !b.as_bool(),
        None => true,
    }
}

struct OutputIvars {
    state: Arc<State>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "DAScreenStreamOutput"]
    #[ivars = OutputIvars]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // Frames for an unchanged screen carry no picture.
            let Some(image) = sample.image_buffer() else {
                return;
            };
            let state = &self.ivars().state;
            *state.last.lock().unwrap() = Some((image.clone(), Instant::now()));
            state.encode(&image);
        }
    }
);

impl StreamOutput {
    fn new(state: Arc<State>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputIvars { state });
        unsafe { msg_send![super(this), init] }
    }
}

fn describe(e: &NSError) -> String {
    e.localizedDescription().to_string()
}

fn shareable_content() -> Result<SendBox<Retained<SCShareableContent>>> {
    let (tx, rx) = channel::<std::result::Result<SendBox<Retained<SCShareableContent>>, String>>();
    let block = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        let r = match unsafe { Retained::retain(content) } {
            Some(c) => Ok(SendBox(c)),
            None => Err(unsafe { err.as_ref() }
                .map(describe)
                .unwrap_or_else(|| "no screens available".into())),
        };
        let _ = tx.send(r);
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    rx.recv_timeout(Duration::from_secs(10))
        .context("macOS did not list the screens in time")?
        .map_err(|e| anyhow!("could not list the screens: {e}"))
}

/// How long a display that is waking up gets to appear.
const DISPLAY_WAKE_WAIT: Duration = Duration::from_secs(6);

/// Active displays, waiting briefly for a sleeping display to come back.
fn wait_for_displays() -> Vec<CGDirectDisplayID> {
    let until = Instant::now() + DISPLAY_WAKE_WAIT;
    loop {
        let ids = display_ids();
        if !ids.is_empty() || Instant::now() >= until {
            return ids;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The shareable screens, retried while a waking display isn't listed yet.
fn shareable_content_with_display() -> Result<SendBox<Retained<SCShareableContent>>> {
    let until = Instant::now() + DISPLAY_WAKE_WAIT;
    loop {
        let content = shareable_content()?;
        let any = unsafe { content.0.displays() }.count() > 0;
        if any || Instant::now() >= until {
            if !any {
                tracing::warn!("no display came back after waking it");
            }
            return Ok(content);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // IOKit/pwr_mgt/IOPMLib.h
    fn IOPMAssertionDeclareUserActivity(
        name: *const CFString,
        user_type: u32,
        assertion_id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(assertion_id: u32) -> i32;
}

/// kIOPMUserActiveLocal: activity as if from this Mac's own keyboard.
const IOPM_USER_ACTIVE_LOCAL: u32 = 0;

/// Turns the display on (like touching the keyboard) and keeps it from
/// idling off while the screen is being viewed. Released when dropped.
struct DisplayAwake(u32);

impl DisplayAwake {
    fn declare() -> Option<Self> {
        let name = CFString::from_static_str("DisplayAnything remote screen control");
        let mut id = 0u32;
        // SAFETY: `name` is a valid CFString for the call, and `id` is
        // written by IOKit on success.
        let r =
            unsafe { IOPMAssertionDeclareUserActivity(&*name, IOPM_USER_ACTIVE_LOCAL, &mut id) };
        if r != 0 {
            tracing::warn!(code = r, "could not wake the display");
            return None;
        }
        Some(DisplayAwake(id))
    }
}

impl Drop for DisplayAwake {
    fn drop(&mut self) {
        // SAFETY: releases the assertion created in declare().
        unsafe { IOPMAssertionRelease(self.0) };
    }
}

/// Active displays, main display first.
fn display_ids() -> Vec<CGDirectDisplayID> {
    let mut ids = [0 as CGDirectDisplayID; 16];
    let mut count = 0u32;
    unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    let mut list: Vec<_> = ids[..count as usize].to_vec();
    let main = CGMainDisplayID();
    list.sort_by_key(|&id| id != main);
    list
}

/// A display switched to another resolution for a session. Dropping it
/// switches back; macOS also reverts it if this app quits (the change is
/// made "for this app only").
struct ModeGuard;

impl Drop for ModeGuard {
    fn drop(&mut self) {
        CGRestorePermanentDisplayConfiguration();
        tracing::info!("restored the screen resolution");
    }
}

/// Switch display `id` to the mode that best fits `want` (in points, HiDPI
/// preferred so it stays sharp); None if it is already there or the switch
/// failed.
fn switch_mode(id: CGDirectDisplayID, want: (u32, u32)) -> Option<ModeGuard> {
    unsafe {
        let current = CGDisplayCopyDisplayMode(id)?;
        let key: &CFString = kCGDisplayShowDuplicateLowResolutionModes;
        let opts =
            CFDictionary::<CFString, CFType>::from_slices(&[key], &[CFBoolean::new(true).as_ref()]);
        let list = CGDisplayCopyAllDisplayModes(id, Some(opts.as_opaque()))?;
        let mut all: Vec<CFRetained<CGDisplayMode>> = Vec::new();
        for i in 0..list.count() {
            let p = list.value_at_index(i) as *mut CGDisplayMode;
            if let Some(p) = NonNull::new(p) {
                let m = CFRetained::retain(p);
                if CGDisplayMode::is_usable_for_desktop_gui(Some(&m)) {
                    all.push(m);
                }
            }
        }
        let as_mode = |m: &CGDisplayMode| super::Mode {
            width: CGDisplayMode::width(Some(m)) as u32,
            height: CGDisplayMode::height(Some(m)) as u32,
            hz: CGDisplayMode::refresh_rate(Some(m)).round() as u32,
            hidpi: CGDisplayMode::pixel_width(Some(m)) > CGDisplayMode::width(Some(m)),
        };
        let modes: Vec<super::Mode> = all.iter().map(|m| as_mode(m)).collect();
        let pick = super::pick_mode(&modes, as_mode(&current), want)?;
        let mode = &all[pick];
        let mut config: CGDisplayConfigRef = std::ptr::null_mut();
        if CGBeginDisplayConfiguration(&mut config) != CGError::Success {
            return None;
        }
        if CGConfigureDisplayWithDisplayMode(config, id, Some(mode), None) != CGError::Success
            || CGCompleteDisplayConfiguration(config, CGConfigureOption::ForAppOnly)
                != CGError::Success
        {
            CGCancelDisplayConfiguration(config);
            tracing::warn!("could not change the screen resolution");
            return None;
        }
        tracing::info!(
            width = modes[pick].width,
            height = modes[pick].height,
            hz = modes[pick].hz,
            hidpi = modes[pick].hidpi,
            "switched the screen resolution for remote control"
        );
        Some(ModeGuard)
    }
}

fn pixel_size(id: CGDirectDisplayID) -> Option<(u32, u32)> {
    let mode = CGDisplayCopyDisplayMode(id)?;
    let w = CGDisplayMode::pixel_width(Some(&mode)) as u32;
    let h = CGDisplayMode::pixel_height(Some(&mode)) as u32;
    (w > 0 && h > 0).then_some((w, h))
}

pub(super) fn displays() -> Vec<DisplayInfo> {
    display_ids()
        .into_iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let (width, height) = pixel_size(id)?;
            Some(DisplayInfo {
                index: i as u32,
                width,
                height,
            })
        })
        .collect()
}

pub(super) fn capture_permission(prompt: bool) -> bool {
    if CGPreflightScreenCaptureAccess() {
        return true;
    }
    if prompt {
        CGRequestScreenCaptureAccess();
    }
    false
}
