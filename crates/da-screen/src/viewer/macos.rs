//! macOS viewer: an `AVSampleBufferDisplayLayer` added on top of the
//! window's view. It decodes H.264 in hardware and, with the
//! "display immediately" attachment on each frame, shows it as soon as it
//! is decoded. The layer resizes with the window and keeps the picture's
//! shape (black bars).
//!
//! The layer's own enqueue/flush are deprecated in favour of
//! `sampleBufferRenderer` (macOS 14+); they still work and keep older
//! macOS versions supported.
#![allow(deprecated)]

use crate::h264;
use crate::wire::EncodedFrame;
use anyhow::{anyhow, bail, Result};
use objc2::rc::Retained;
use objc2_app_kit::NSView;
use objc2_av_foundation::{
    AVLayerVideoGravityResizeAspect, AVQueuedSampleBufferRenderingStatus,
    AVSampleBufferDisplayLayer,
};
use objc2_core_foundation::{kCFBooleanTrue, CFMutableDictionary, CFRetained, CFString};
use objc2_core_media::{
    kCMBlockBufferAssureMemoryNowFlag, kCMSampleAttachmentKey_DisplayImmediately, kCMTimeInvalid,
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
};
use objc2_quartz_core::CAAutoresizingMask;
use raw_window_handle::RawWindowHandle;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

struct Shared {
    keyframe: AtomicBool,
    size: Mutex<Option<(u32, u32)>>,
}

/// The layer is only enqueued to and flushed from our worker thread, which
/// AVFoundation allows from any thread.
struct Layer(Retained<AVSampleBufferDisplayLayer>);
unsafe impl Send for Layer {}

pub(super) struct Presenter {
    tx: Mutex<Sender<EncodedFrame>>,
    shared: Arc<Shared>,
    layer: Layer,
}

impl Presenter {
    pub(super) unsafe fn new(window: RawWindowHandle) -> Result<Self> {
        let RawWindowHandle::AppKit(h) = window else {
            bail!("not a macOS window");
        };
        let view: &NSView = h.ns_view.cast::<NSView>().as_ref();
        view.setWantsLayer(true);
        let host = view
            .layer()
            .ok_or_else(|| anyhow!("the viewer window has no layer"))?;
        let layer = AVSampleBufferDisplayLayer::new();
        if let Some(gravity) = AVLayerVideoGravityResizeAspect {
            layer.setVideoGravity(gravity);
        }
        layer.setFrame(host.bounds());
        layer.setAutoresizingMask(
            CAAutoresizingMask::LayerWidthSizable | CAAutoresizingMask::LayerHeightSizable,
        );
        host.addSublayer(&layer);

        let shared = Arc::new(Shared {
            keyframe: AtomicBool::new(false),
            size: Mutex::new(None),
        });
        let (tx, rx) = channel::<EncodedFrame>();
        let worker_layer = Layer(layer.clone());
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("da-screen-viewer".into())
            .spawn(move || {
                let layer = worker_layer;
                let mut format: Option<CFRetained<CMFormatDescription>> = None;
                let mut need_key = true;
                while let Ok(f) = rx.recv() {
                    if layer.0.status() == AVQueuedSampleBufferRenderingStatus::Failed {
                        tracing::warn!("viewer decode failed; waiting for a keyframe");
                        layer.0.flush();
                        need_key = true;
                    }
                    if f.keyframe {
                        if let Some((sps, pps)) = h264::parameter_sets(&f.data) {
                            match format_description(&sps, &pps) {
                                Ok(d) => format = Some(d),
                                Err(e) => tracing::warn!("bad stream format: {e:#}"),
                            }
                        }
                    }
                    if need_key {
                        if !f.keyframe || format.is_none() {
                            sh.keyframe.store(true, Ordering::Relaxed);
                            continue;
                        }
                        need_key = false;
                    }
                    let Some(fmt) = &format else { continue };
                    match sample_buffer(fmt, &f) {
                        Ok(sb) => layer.0.enqueueSampleBuffer(&sb),
                        Err(e) => {
                            tracing::debug!("could not queue a frame: {e:#}");
                            need_key = true;
                            sh.keyframe.store(true, Ordering::Relaxed);
                        }
                    }
                }
            })?;
        Ok(Presenter {
            tx: Mutex::new(tx),
            shared,
            layer: Layer(layer),
        })
    }

    pub(super) fn push(&self, frame: EncodedFrame) {
        let _ = self.tx.lock().unwrap().send(frame);
    }

    pub(super) fn take_keyframe_request(&self) -> bool {
        self.shared.keyframe.swap(false, Ordering::Relaxed)
    }

    pub(super) fn set_video_size(&self, width: u32, height: u32) {
        *self.shared.size.lock().unwrap() = Some((width, height));
    }

    pub(super) fn video_size(&self) -> Option<(u32, u32)> {
        *self.shared.size.lock().unwrap()
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        // Closing the channel ends the worker. The layer goes when the
        // window does; flush so it lets go of decoded frames now.
        unsafe { self.layer.0.flush() };
    }
}

unsafe fn format_description(sps: &[u8], pps: &[u8]) -> Result<CFRetained<CMFormatDescription>> {
    let mut ptrs = [
        NonNull::new_unchecked(sps.as_ptr() as *mut u8),
        NonNull::new_unchecked(pps.as_ptr() as *mut u8),
    ];
    let mut sizes = [sps.len(), pps.len()];
    let mut out: *const CMFormatDescription = std::ptr::null();
    let s = CMVideoFormatDescriptionCreateFromH264ParameterSets(
        None,
        2,
        NonNull::new_unchecked(ptrs.as_mut_ptr()),
        NonNull::new_unchecked(sizes.as_mut_ptr()),
        4,
        NonNull::from(&mut out),
    );
    if s != 0 || out.is_null() {
        bail!("CoreMedia rejected the stream parameters ({s})");
    }
    Ok(CFRetained::from_raw(NonNull::new_unchecked(out as *mut _)))
}

unsafe fn sample_buffer(
    fmt: &CMFormatDescription,
    f: &EncodedFrame,
) -> Result<CFRetained<CMSampleBuffer>> {
    let avcc = h264::annexb_to_avcc(&f.data);
    if avcc.is_empty() {
        bail!("empty frame");
    }
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    let s = CMBlockBuffer::create_with_memory_block(
        None,
        std::ptr::null_mut(),
        avcc.len(),
        None,
        std::ptr::null(),
        0,
        avcc.len(),
        kCMBlockBufferAssureMemoryNowFlag,
        NonNull::from(&mut block),
    );
    if s != 0 || block.is_null() {
        bail!("no memory for a frame ({s})");
    }
    let block = CFRetained::from_raw(NonNull::new_unchecked(block));
    let s = CMBlockBuffer::replace_data_bytes(
        NonNull::new_unchecked(avcc.as_ptr() as *mut c_void),
        &block,
        0,
        avcc.len(),
    );
    if s != 0 {
        bail!("could not fill a frame ({s})");
    }
    let timing = CMSampleTimingInfo {
        duration: kCMTimeInvalid,
        presentationTimeStamp: CMTime {
            value: f.pts_us as i64,
            timescale: 1_000_000,
            flags: CMTimeFlags(1),
            epoch: 0,
        },
        decodeTimeStamp: kCMTimeInvalid,
    };
    let size = avcc.len();
    let mut sb: *mut CMSampleBuffer = std::ptr::null_mut();
    let s = CMSampleBuffer::create_ready(
        None,
        Some(&block),
        Some(fmt),
        1,
        1,
        &timing,
        1,
        &size,
        NonNull::from(&mut sb),
    );
    if s != 0 || sb.is_null() {
        bail!("could not wrap a frame ({s})");
    }
    let sb = CFRetained::from_raw(NonNull::new_unchecked(sb));
    if let Some(arr) = sb.sample_attachments_array(true) {
        if arr.count() > 0 {
            // The attachments array holds mutable dictionaries.
            let dict = arr.value_at_index(0) as *mut CFMutableDictionary;
            if let (Some(dict), Some(t)) = (dict.as_ref(), kCFBooleanTrue) {
                let key: &CFString = kCMSampleAttachmentKey_DisplayImmediately;
                CFMutableDictionary::set_value(
                    Some(dict),
                    key as *const CFString as *const c_void,
                    t as *const _ as *const c_void,
                );
            }
        }
    }
    Ok(sb)
}
