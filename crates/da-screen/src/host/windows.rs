//! Windows host: Windows.Graphics.Capture -> GPU colour conversion ->
//! hardware H.264 encoder (NVENC / AMF / Quick Sync through Media
//! Foundation). Frames stay on the GPU until the encoder hands back the
//! compressed bytes.

use super::{HostOptions, Sink, StreamEvent};
use crate::win::{self, VideoProcessor};
use crate::wire::{Codec, EncodedFrame, StreamInfo};
use crate::DisplayInfo;
use anyhow::{anyhow, bail, Context, Result};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::core::{Interface, VARIANT};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

/// NV12 textures handed to the encoder in rotation, so it can still be
/// reading one while we fill the next.
const POOL: usize = 4;

pub(super) struct Host {
    bounds: Option<crate::DisplayBounds>,
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Host {
    pub(super) fn start(opts: HostOptions, mut sink: Sink) -> Result<Self> {
        let mons = monitors();
        let bounds = mons
            .get(opts.display as usize)
            .or_else(|| mons.first())
            .map(|m| m.bounds);
        let stop = Arc::new(AtomicBool::new(false));
        let keyframe = Arc::new(AtomicBool::new(true));
        let (stop2, key2) = (stop.clone(), keyframe.clone());
        let thread = std::thread::Builder::new()
            .name("da-screen-host".into())
            .spawn(move || {
                if let Err(e) = run(&opts, &mut sink, &stop2, &key2) {
                    tracing::warn!("screen capture stopped: {e:#}");
                    sink(StreamEvent::Error(format!("{e:#}")));
                }
            })?;
        Ok(Host {
            bounds,
            stop,
            keyframe,
            thread: Some(thread),
        })
    }

    pub(super) fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }

    pub(super) fn bounds(&self) -> Option<crate::DisplayBounds> {
        self.bounds
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Monitor {
    handle: HMONITOR,
    primary: bool,
    width: u32,
    height: u32,
    bounds: crate::DisplayBounds,
}

fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn each(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = &mut *(data.0 as *mut Vec<Monitor>);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(m, &mut info).as_bool() {
            let r = info.rcMonitor;
            list.push(Monitor {
                handle: m,
                primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
                width: (r.right - r.left) as u32,
                height: (r.bottom - r.top) as u32,
                bounds: crate::DisplayBounds {
                    x: f64::from(r.left),
                    y: f64::from(r.top),
                    width: f64::from(r.right - r.left),
                    height: f64::from(r.bottom - r.top),
                },
            });
        }
        TRUE
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(each),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    // Main display first, then the rest in Windows' order.
    list.sort_by_key(|m| !m.primary);
    list
}

pub(super) fn displays() -> Vec<DisplayInfo> {
    monitors()
        .iter()
        .enumerate()
        .map(|(i, m)| DisplayInfo {
            index: i as u32,
            width: m.width,
            height: m.height,
        })
        .collect()
}

pub(super) fn capture_permission(_prompt: bool) -> bool {
    true
}

fn run(
    opts: &HostOptions,
    sink: &mut Sink,
    stop: &AtomicBool,
    keyframe: &AtomicBool,
) -> Result<()> {
    win::startup()?;
    let mons = monitors();
    let mon = mons
        .get(opts.display as usize)
        .or_else(|| mons.first())
        .ok_or_else(|| anyhow!("this computer has no display to share"))?;
    let (device, ctx) = win::create_device()?;

    // Windows.Graphics.Capture on the chosen monitor, with the cursor.
    let item: GraphicsCaptureItem = unsafe {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        interop.CreateForMonitor(mon.handle)?
    };
    let d3d: IDirect3DDevice = unsafe {
        let dxgi: IDXGIDevice = device.cast()?;
        CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?
    };
    let mut size = item.Size()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &d3d,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )?;
    let (tx, rx) = channel::<()>();
    pool.FrameArrived(&TypedEventHandler::new(move |_, _| {
        let _ = tx.send(());
        Ok(())
    }))?;
    let session = pool.CreateCaptureSession(&item)?;
    let _ = session.SetIsCursorCaptureEnabled(true);
    // Windows 11 can drop the yellow border; older builds just keep it.
    let _ = session.SetIsBorderRequired(false);
    session.StartCapture()?;

    let displays = mons.len() as u32;
    let display = if (opts.display as usize) < mons.len() {
        opts.display
    } else {
        0
    };
    let mut enc = Encoder::new(&device, &ctx, size, opts)?;
    sink(StreamEvent::Info(StreamInfo {
        width: enc.width,
        height: enc.height,
        codec: Codec::H264,
        displays,
        display,
    }));

    let started = Instant::now();
    let interval = Duration::from_secs_f64(1.0 / opts.fps as f64);
    let mut last_submit = Instant::now() - interval;
    let mut have_new = false; // a captured picture not yet encoded

    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(if have_new { 1 } else { 10 })) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Take the newest frame; older ones are dropped.
        let mut latest = None;
        while let Ok(f) = pool.TryGetNextFrame() {
            latest = Some(f);
        }
        if let Some(frame) = latest {
            let content = frame.ContentSize()?;
            if content.Width != size.Width || content.Height != size.Height {
                // Resolution changed: rebuild everything at the new size.
                size = content;
                pool.Recreate(&d3d, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size)?;
                enc = Encoder::new(&device, &ctx, size, opts)?;
                sink(StreamEvent::Info(StreamInfo {
                    width: enc.width,
                    height: enc.height,
                    codec: Codec::H264,
                    displays,
                    display,
                }));
                keyframe.store(true, Ordering::Relaxed);
                continue;
            }
            let tex: ID3D11Texture2D = unsafe {
                frame
                    .Surface()?
                    .cast::<IDirect3DDxgiInterfaceAccess>()?
                    .GetInterface()?
            };
            unsafe { ctx.CopyResource(&enc.bgra, &tex) };
            let _ = frame.Close();
            have_new = true;
        }

        enc.poll(sink, started)?;
        let force = keyframe.load(Ordering::Relaxed);
        let due = last_submit.elapsed() >= interval;
        // A keyframe request with a still screen re-encodes the last picture.
        if (have_new || force) && due && enc.ready() {
            if force {
                keyframe.store(false, Ordering::Relaxed);
            }
            enc.submit(have_new, force, started)?;
            have_new = false;
            last_submit = Instant::now();
        }
    }
    let _ = session.Close();
    let _ = pool.Close();
    enc.finish();
    Ok(())
}

struct Encoder {
    width: u32,
    height: u32,
    fps: u32,
    /// Our copy of the captured frame (BGRA).
    bgra: ID3D11Texture2D,
    vp: VideoProcessor,
    input_view: ID3D11VideoProcessorInputView,
    nv12: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    /// Slot holding the most recent converted picture.
    last: Option<usize>,
    mft: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: Option<ICodecAPI>,
    in_id: u32,
    out_id: u32,
    /// Input requests from the encoder not yet answered.
    need: u32,
    provides_samples: bool,
    out_size: u32,
}

/// Largest pictures to try, biggest first. Hardware H.264 encoders top out
/// around 4096 pixels wide (some lower), so wider screens (5120x1440
/// ultrawides, for one) are scaled down until the encoder accepts them.
const SIZE_LIMITS: [(u32, u32); 4] = [(4096, 4096), (3840, 2160), (2560, 1600), (1920, 1200)];

impl Encoder {
    fn new(
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        size: SizeInt32,
        opts: &HostOptions,
    ) -> Result<Self> {
        let full = (size.Width.max(2) as u32, size.Height.max(2) as u32);
        let wanted = opts.target_size(full);
        let mut last_err = None;
        let mut tried = Vec::new();
        for limit in SIZE_LIMITS {
            let out = super::fit_within(wanted, limit);
            if tried.contains(&out) {
                continue;
            }
            tried.push(out);
            match Self::with_size(device, ctx, full, out, opts) {
                Ok(e) => return Ok(e),
                Err(e) => {
                    tracing::warn!(
                        width = out.0,
                        height = out.1,
                        "encoder refused this size, trying smaller: {e:#}"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no video encoder size worked")))
    }

    fn with_size(
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        (cw, ch): (u32, u32),
        (width, height): (u32, u32),
        opts: &HostOptions,
    ) -> Result<Self> {
        let bgra = win::texture(
            device,
            cw,
            ch,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
        )?;
        let vp = VideoProcessor::new(device, ctx, (cw, ch), (width, height), opts.fps, true)?;
        let input_view = vp.input_view(&bgra, 0)?;
        let mut nv12 = Vec::with_capacity(POOL);
        for _ in 0..POOL {
            let t = win::texture(
                device,
                width,
                height,
                DXGI_FORMAT_NV12,
                D3D11_BIND_RENDER_TARGET,
            )?;
            let v = vp.output_view(&t)?;
            nv12.push((t, v));
        }

        let mft = hardware_h264_encoder()?;
        let (in_id, out_id) = win::stream_ids(&mft);
        unsafe {
            let attrs = mft.GetAttributes()?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            let manager = win::device_manager(device)?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .context("the video encoder can't use this graphics device")?;

            let codec = mft.cast::<ICodecAPI>().ok();
            if let Some(api) = &codec {
                let bps = opts.bitrate_kbps.saturating_mul(1000);
                win::codec_set(api, &CODECAPI_AVLowLatencyMode, VARIANT::from(true));
                win::codec_set(
                    api,
                    &CODECAPI_AVEncCommonRateControlMode,
                    VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
                );
                win::codec_set(api, &CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bps));
                win::codec_set(
                    api,
                    &CODECAPI_AVEncMPVDefaultBPictureCount,
                    VARIANT::from(0u32),
                );
                // A keyframe every 30 s heals anything odd; others on request.
                win::codec_set(api, &CODECAPI_AVEncMPVGOPSize, VARIANT::from(opts.fps * 30));
            }

            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, opts.bitrate_kbps.saturating_mul(1000))?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, win::pack(width, height))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, win::pack(opts.fps, 1))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, win::pack(1, 1))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            mft.SetOutputType(out_id, &out, 0)
                .context("the video encoder refused the output format")?;

            let inp = MFCreateMediaType()?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            inp.SetUINT64(&MF_MT_FRAME_SIZE, win::pack(width, height))?;
            inp.SetUINT64(&MF_MT_FRAME_RATE, win::pack(opts.fps, 1))?;
            inp.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, win::pack(1, 1))?;
            inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(in_id, &inp, 0)
                .context("the video encoder refused the input format")?;

            let info = mft.GetOutputStreamInfo(out_id)?;
            let provides_samples = info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                    as u32
                != 0;
            let out_size = info.cbSize.max(width * height * 2);

            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let events: IMFMediaEventGenerator = mft.cast()?;
            tracing::info!(width, height, fps = opts.fps, "screen encoder ready");
            Ok(Encoder {
                width,
                height,
                fps: opts.fps,
                bgra,
                vp,
                input_view,
                nv12,
                next: 0,
                last: None,
                mft,
                events,
                codec,
                in_id,
                out_id,
                need: 0,
                provides_samples,
                out_size,
            })
        }
    }

    fn ready(&self) -> bool {
        self.need > 0
    }

    /// Handle the encoder's events: count input requests, collect output.
    fn poll(&mut self, sink: &mut Sink, started: Instant) -> Result<()> {
        loop {
            let ev = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(ev) => ev,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(e) => return Err(e).context("video encoder failed"),
            };
            let kind = unsafe { ev.GetType()? };
            if kind == METransformNeedInput.0 as u32 {
                self.need += 1;
            } else if kind == METransformHaveOutput.0 as u32 {
                if let Some(f) = self.take_output(started)? {
                    sink(StreamEvent::Frame(f));
                }
            }
        }
    }

    fn take_output(&mut self, started: Instant) -> Result<Option<EncodedFrame>> {
        unsafe {
            let sample = if self.provides_samples {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.out_size)?)?;
                Some(s)
            };
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.out_id,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut out, &mut status);
            let sample = ManuallyDrop::take(&mut out[0].pSample);
            ManuallyDrop::drop(&mut out[0].pEvents);
            match r {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    let t = self.mft.GetOutputAvailableType(self.out_id, 0)?;
                    self.mft.SetOutputType(self.out_id, &t, 0)?;
                    return Ok(None);
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) => return Err(e).context("video encoder output failed"),
            }
            let Some(sample) = sample else {
                return Ok(None);
            };
            let data = win::sample_bytes(&sample)?;
            let keyframe = sample
                .GetUINT32(&MFSampleExtension_CleanPoint)
                .map(|v| v != 0)
                .unwrap_or_else(|_| crate::h264::is_keyframe(&data));
            Ok(Some(EncodedFrame {
                keyframe,
                pts_us: started.elapsed().as_micros() as u64,
                data,
            }))
        }
    }

    /// Feed one picture: convert the captured frame if there's a new one,
    /// otherwise re-send the last one (for a keyframe request).
    fn submit(&mut self, new_picture: bool, force_key: bool, started: Instant) -> Result<()> {
        let slot = if new_picture || self.last.is_none() {
            let slot = self.next;
            self.next = (self.next + 1) % self.nv12.len();
            let full = RECT {
                left: 0,
                top: 0,
                right: self.width as i32,
                bottom: self.height as i32,
            };
            self.vp
                .blt(&self.input_view, &self.nv12[slot].1, full, full)?;
            self.last = Some(slot);
            slot
        } else {
            self.last.unwrap_or(0)
        };
        if force_key {
            if let Some(api) = &self.codec {
                win::codec_set(api, &CODECAPI_AVEncVideoForceKeyFrame, VARIANT::from(1u32));
            }
        }
        unsafe {
            let buf =
                MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &self.nv12[slot].0, 0, false)?;
            if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
                if let Ok(len) = b2.GetContiguousLength() {
                    let _ = buf.SetCurrentLength(len);
                }
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buf)?;
            // Media Foundation time is in 100 ns units.
            sample.SetSampleTime(started.elapsed().as_nanos() as i64 / 100)?;
            sample.SetSampleDuration(10_000_000 / self.fps.max(1) as i64)?;
            self.mft
                .ProcessInput(self.in_id, &sample, 0)
                .context("video encoder input failed")?;
        }
        self.need -= 1;
        Ok(())
    }

    fn finish(&self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
    }
}

/// The first hardware H.264 encoder that takes NV12 (the GPU's own, so
/// NVENC, AMF or Quick Sync).
fn hardware_h264_encoder() -> Result<IMFTransform> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut list,
            &mut count,
        )?;
        if list.is_null() || count == 0 {
            bail!("no hardware H.264 encoder found on this graphics card");
        }
        let activates = std::slice::from_raw_parts_mut(list, count as usize);
        let mut found = None;
        let mut last_err = None;
        for a in activates.iter_mut() {
            if let (None, Some(act)) = (&found, a.as_ref()) {
                match act.ActivateObject::<IMFTransform>() {
                    Ok(t) => found = Some(t),
                    Err(e) => last_err = Some(e),
                }
            }
            drop(a.take());
        }
        CoTaskMemFree(Some(list as *const _));
        match (found, last_err) {
            (Some(t), _) => Ok(t),
            (None, Some(e)) => Err(e).context("the hardware H.264 encoder did not start"),
            (None, None) => bail!("no hardware H.264 encoder found"),
        }
    }
}
