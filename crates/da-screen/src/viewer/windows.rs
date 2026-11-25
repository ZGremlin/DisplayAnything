//! Windows viewer: Microsoft's H.264 decoder in DXVA mode (decodes on the
//! GPU into Direct3D textures), then the GPU video processor scales and
//! converts each frame straight into a flip-model swap chain on the window.
//! Present uses sync interval 0 with tearing allowed, so a frame is on
//! screen as soon as it decodes.

use crate::win::{self, VideoProcessor};
use crate::wire::EncodedFrame;
use anyhow::{anyhow, bail, Context, Result};
use raw_window_handle::RawWindowHandle;
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

struct Shared {
    keyframe: AtomicBool,
    size: Mutex<Option<(u32, u32)>>,
    stop: AtomicBool,
}

pub(super) struct Presenter {
    tx: Mutex<Sender<EncodedFrame>>,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Presenter {
    pub(super) unsafe fn new(window: RawWindowHandle) -> Result<Self> {
        let RawWindowHandle::Win32(h) = window else {
            bail!("not a Windows window");
        };
        let hwnd = h.hwnd.get();
        let shared = Arc::new(Shared {
            keyframe: AtomicBool::new(false),
            size: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let (tx, rx) = channel::<EncodedFrame>();
        let (ready_tx, ready_rx) = channel::<Result<()>>();
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("da-screen-viewer".into())
            .spawn(move || {
                let mut r = match Renderer::new(HWND(hwnd as *mut _), sh.clone()) {
                    Ok(r) => {
                        let _ = ready_tx.send(Ok(()));
                        r
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                while !sh.stop.load(Ordering::Relaxed) {
                    match rx.recv_timeout(Duration::from_millis(30)) {
                        Ok(f) => {
                            // Skip to the newest frames if we fell behind,
                            // but every frame still has to be decoded.
                            if let Err(e) = r.decode(f) {
                                tracing::warn!("decode failed: {e:#}");
                                r.reset();
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    if let Err(e) = r.check_resize() {
                        tracing::debug!("resize failed: {e:#}");
                    }
                }
            })?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("the viewer thread stopped"))??;
        Ok(Presenter {
            tx: Mutex::new(tx),
            shared,
            thread: Some(thread),
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
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

type SizedProcessor = (VideoProcessor, (u32, u32), (u32, u32));

struct Renderer {
    hwnd: HWND,
    shared: Arc<Shared>,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    tearing: bool,
    backbuffer: (u32, u32),
    decoder: IMFTransform,
    in_id: u32,
    out_id: u32,
    /// Waiting for a keyframe before decoding (start, or after an error).
    need_key: bool,
    last_key_request: Option<Instant>,
    /// Video processor with the input and output sizes it was made for.
    vp: Option<SizedProcessor>,
    views: HashMap<(usize, u32), ID3D11VideoProcessorInputView>,
    /// Last decoded picture, redrawn when the window is resized.
    last: Option<(ID3D11Texture2D, u32)>,
}

impl Renderer {
    fn new(hwnd: HWND, shared: Arc<Shared>) -> Result<Self> {
        win::startup()?;
        let (device, ctx) = win::create_device()?;
        let (swap, tearing) = create_swap_chain(&device, hwnd)?;
        let backbuffer = client_size(hwnd);
        let (decoder, in_id, out_id) = create_decoder(&device)?;
        Ok(Renderer {
            hwnd,
            shared,
            device,
            ctx,
            swap,
            tearing,
            backbuffer,
            decoder,
            in_id,
            out_id,
            need_key: true,
            last_key_request: None,
            vp: None,
            views: HashMap::new(),
            last: None,
        })
    }

    fn ask_keyframe(&mut self) {
        // At most twice a second, so a burst of errors is one request.
        if self
            .last_key_request
            .is_none_or(|t| t.elapsed() > Duration::from_millis(500))
        {
            self.shared.keyframe.store(true, Ordering::Relaxed);
            self.last_key_request = Some(Instant::now());
        }
    }

    /// Start over after a decode error: flush and wait for a keyframe.
    fn reset(&mut self) {
        unsafe {
            let _ = self.decoder.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        self.need_key = true;
        self.ask_keyframe();
    }

    fn decode(&mut self, f: EncodedFrame) -> Result<()> {
        if self.need_key {
            if !f.keyframe {
                self.ask_keyframe();
                return Ok(());
            }
            self.need_key = false;
        }
        let sample = win::memory_sample(&f.data)?;
        unsafe {
            sample.SetSampleTime(f.pts_us as i64 * 10)?;
            if f.keyframe {
                let _ = sample.SetUINT32(&MFSampleExtension_CleanPoint, 1);
            }
            loop {
                match self.decoder.ProcessInput(self.in_id, &sample, 0) {
                    Ok(()) => break,
                    Err(e) if e.code() == MF_E_NOTACCEPTING => self.drain()?,
                    Err(e) => return Err(e).context("decoder rejected a frame"),
                }
            }
        }
        self.drain()
    }

    /// Take every finished picture out of the decoder and show the last.
    fn drain(&mut self) -> Result<()> {
        loop {
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.out_id,
                pSample: ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = unsafe { self.decoder.ProcessOutput(0, &mut out, &mut status) };
            let sample = unsafe { ManuallyDrop::take(&mut out[0].pSample) };
            unsafe { ManuallyDrop::drop(&mut out[0].pEvents) };
            match r {
                Ok(()) => {
                    if let Some(s) = sample {
                        let (tex, slice) = texture_of(&s)?;
                        self.last = Some((tex.clone(), slice));
                        self.draw(&tex, slice)?;
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    set_nv12_output(&self.decoder, self.out_id)?;
                    self.views.clear();
                    self.vp = None;
                }
                Err(e) => return Err(e).context("decoder output failed"),
            }
        }
    }

    fn check_resize(&mut self) -> Result<()> {
        let size = client_size(self.hwnd);
        if size == self.backbuffer || size.0 == 0 || size.1 == 0 {
            return Ok(());
        }
        let flags = if self.tearing {
            DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING
        } else {
            DXGI_SWAP_CHAIN_FLAG(0)
        };
        unsafe {
            self.ctx.ClearState();
            self.ctx.Flush();
            self.swap
                .ResizeBuffers(0, size.0, size.1, DXGI_FORMAT_UNKNOWN, flags)?;
        }
        self.backbuffer = size;
        self.vp = None;
        self.views.clear();
        if let Some((tex, slice)) = self.last.clone() {
            self.draw(&tex, slice)?;
        }
        Ok(())
    }

    fn draw(&mut self, tex: &ID3D11Texture2D, slice: u32) -> Result<()> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        let tex_size = (desc.Width, desc.Height);
        // The real picture size: decoders pad to 16 (1080 -> 1088).
        let pic = self
            .shared
            .size
            .lock()
            .unwrap()
            .map(|(w, h)| (w.min(tex_size.0), h.min(tex_size.1)))
            .unwrap_or(tex_size);
        let out = self.backbuffer;
        if out.0 == 0 || out.1 == 0 {
            return Ok(());
        }
        if self
            .vp
            .as_ref()
            .is_none_or(|(_, i, o)| *i != tex_size || *o != out)
        {
            self.views.clear();
            let vp = VideoProcessor::new(&self.device, &self.ctx, tex_size, out, 60, false)?;
            self.vp = Some((vp, tex_size, out));
        }
        let (vp, _, _) = self.vp.as_ref().unwrap();
        let key = (tex.as_raw() as usize, slice);
        let input = match self.views.entry(key) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(vp.input_view(tex, slice)?),
        };
        let back: ID3D11Texture2D = unsafe { self.swap.GetBuffer(0)? };
        let output = vp.output_view(&back)?;
        let (x, y, w, h) = super::fit(pic, out);
        vp.blt(
            input,
            &output,
            RECT {
                left: 0,
                top: 0,
                right: pic.0 as i32,
                bottom: pic.1 as i32,
            },
            RECT {
                left: x as i32,
                top: y as i32,
                right: (x + w) as i32,
                bottom: (y + h) as i32,
            },
        )?;
        drop(output);
        drop(back);
        let flags = if self.tearing {
            DXGI_PRESENT_ALLOW_TEARING
        } else {
            DXGI_PRESENT(0)
        };
        unsafe { self.swap.Present(0, flags).ok()? };
        Ok(())
    }
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    (
        (r.right - r.left).max(0) as u32,
        (r.bottom - r.top).max(0) as u32,
    )
}

fn create_swap_chain(device: &ID3D11Device, hwnd: HWND) -> Result<(IDXGISwapChain1, bool)> {
    unsafe {
        let dxgi: IDXGIDevice = device.cast()?;
        let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
        let mut tearing = windows::Win32::Foundation::BOOL(0);
        if let Ok(f5) = factory.cast::<IDXGIFactory5>() {
            let _ = f5.CheckFeatureSupport(
                DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                &mut tearing as *mut _ as *mut _,
                std::mem::size_of_val(&tearing) as u32,
            );
        }
        let tearing = tearing.as_bool();
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: 0,
            Height: 0,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: if tearing {
                DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32
            } else {
                0
            },
        };
        let swap = factory
            .CreateSwapChainForHwnd(device, hwnd, &desc, None, None)
            .context("could not draw into the viewer window")?;
        let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
        Ok((swap, tearing))
    }
}

fn create_decoder(device: &ID3D11Device) -> Result<(IMFTransform, u32, u32)> {
    unsafe {
        let dec: IMFTransform =
            CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)
                .context("the Windows H.264 decoder is missing")?;
        let attrs = dec.GetAttributes()?;
        let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
        if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
            bail!("the Windows H.264 decoder can't decode on this graphics card");
        }
        let manager = win::device_manager(device)?;
        dec.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
            .context("the H.264 decoder can't use this graphics device")?;
        if let Ok(api) = dec.cast::<ICodecAPI>() {
            win::codec_set(
                &api,
                &CODECAPI_AVLowLatencyMode,
                windows::core::VARIANT::from(true),
            );
        }
        let (in_id, out_id) = win::stream_ids(&dec);
        let inp = MFCreateMediaType()?;
        inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        dec.SetInputType(in_id, &inp, 0)?;
        set_nv12_output(&dec, out_id)?;
        dec.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        dec.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        Ok((dec, in_id, out_id))
    }
}

fn set_nv12_output(dec: &IMFTransform, out_id: u32) -> Result<()> {
    unsafe {
        let mut i = 0;
        while let Ok(t) = dec.GetOutputAvailableType(out_id, i) {
            if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                dec.SetOutputType(out_id, &t, 0)?;
                return Ok(());
            }
            i += 1;
        }
    }
    bail!("the H.264 decoder offers no NV12 output")
}

fn texture_of(sample: &IMFSample) -> Result<(ID3D11Texture2D, u32)> {
    unsafe {
        let buf = sample.GetBufferByIndex(0)?;
        let dxgi: IMFDXGIBuffer = buf
            .cast()
            .context("the decoder did not decode on the graphics card")?;
        let mut ptr = std::ptr::null_mut();
        dxgi.GetResource(&ID3D11Texture2D::IID, &mut ptr)?;
        let tex = ID3D11Texture2D::from_raw(ptr);
        Ok((tex, dxgi.GetSubresourceIndex()?))
    }
}
