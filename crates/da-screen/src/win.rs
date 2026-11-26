//! Direct3D 11 and Media Foundation helpers shared by the Windows host and
//! viewer.

use anyhow::{anyhow, Context, Result};
use std::mem::ManuallyDrop;
use windows::core::{Interface, GUID};
use windows::Win32::Foundation::{BOOL, HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Start COM (multithreaded) and Media Foundation on this thread.
pub(crate) fn startup() -> Result<()> {
    unsafe {
        // S_FALSE (already started) is fine; a different apartment is too,
        // since we only use free-threaded objects.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).context("Media Foundation did not start")?;
    }
    Ok(())
}

/// A hardware Direct3D 11 device that video encode/decode can share
/// across threads.
pub(crate) fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut ctx = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .context("no Direct3D 11 graphics device")?;
    }
    let device = device.ok_or_else(|| anyhow!("no Direct3D device"))?;
    let ctx = ctx.ok_or_else(|| anyhow!("no Direct3D context"))?;
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
    }
    Ok((device, ctx))
}

/// A DXGI device manager so Media Foundation transforms use our GPU.
pub(crate) fn device_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager> {
    let mut token = 0u32;
    let mut manager = None;
    unsafe {
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or_else(|| anyhow!("no DXGI device manager"))?;
        manager.ResetDevice(device, token)?;
        Ok(manager)
    }
}

pub(crate) fn texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    bind: D3D11_BIND_FLAG,
) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    tex.ok_or_else(|| anyhow!("texture not created"))
}

/// Pack two u32s the way MF_MT_FRAME_SIZE and MF_MT_FRAME_RATE want.
pub(crate) fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

/// The GPU's video processor, used for colour conversion (BGRA <-> NV12)
/// and scaling. Each `blt` maps one input texture onto one output texture.
pub(crate) struct VideoProcessor {
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
}

/// Colour space bitfield (D3D11_VIDEO_PROCESSOR_COLOR_SPACE): full-range RGB.
const CS_RGB_FULL: u32 = 0;
/// BT.709 matrix (bit 2) with studio range (Nominal_Range = 1, bits 4-5).
const CS_YUV_709_STUDIO: u32 = (1 << 2) | (1 << 4);

impl VideoProcessor {
    pub(crate) fn new(
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        in_size: (u32, u32),
        out_size: (u32, u32),
        fps: u32,
        rgb_to_yuv: bool,
    ) -> Result<Self> {
        let vdev: ID3D11VideoDevice = device.cast().context("no video device")?;
        let vctx: ID3D11VideoContext = ctx.cast().context("no video context")?;
        let rate = DXGI_RATIONAL {
            Numerator: fps.max(1),
            Denominator: 1,
        };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: in_size.0,
            InputHeight: in_size.1,
            OutputFrameRate: rate,
            OutputWidth: out_size.0,
            OutputHeight: out_size.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        unsafe {
            let enumerator = vdev.CreateVideoProcessorEnumerator(&desc)?;
            let vp = vdev.CreateVideoProcessor(&enumerator, 0)?;
            let (input_cs, output_cs) = if rgb_to_yuv {
                (CS_RGB_FULL, CS_YUV_709_STUDIO)
            } else {
                (CS_YUV_709_STUDIO, CS_RGB_FULL)
            };
            vctx.VideoProcessorSetStreamColorSpace(
                &vp,
                0,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: input_cs,
                },
            );
            vctx.VideoProcessorSetOutputColorSpace(
                &vp,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: output_cs,
                },
            );
            vctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, BOOL::from(false));
            // Black bars around a picture scaled to fit.
            vctx.VideoProcessorSetOutputBackgroundColor(
                &vp,
                false,
                &D3D11_VIDEO_COLOR {
                    Anonymous: D3D11_VIDEO_COLOR_0 {
                        RGBA: D3D11_VIDEO_COLOR_RGBA {
                            R: 0.0,
                            G: 0.0,
                            B: 0.0,
                            A: 1.0,
                        },
                    },
                },
            );
            vctx.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            Ok(VideoProcessor {
                vdev,
                vctx,
                enumerator,
                vp,
            })
        }
    }

    pub(crate) fn output_view(
        &self,
        tex: &ID3D11Texture2D,
    ) -> Result<ID3D11VideoProcessorOutputView> {
        let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut view = None;
        unsafe {
            self.vdev.CreateVideoProcessorOutputView(
                tex,
                &self.enumerator,
                &desc,
                Some(&mut view),
            )?
        };
        view.ok_or_else(|| anyhow!("no output view"))
    }

    pub(crate) fn input_view(
        &self,
        tex: &ID3D11Texture2D,
        array_slice: u32,
    ) -> Result<ID3D11VideoProcessorInputView> {
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: array_slice,
                },
            },
        };
        let mut view = None;
        unsafe {
            self.vdev.CreateVideoProcessorInputView(
                tex,
                &self.enumerator,
                &desc,
                Some(&mut view),
            )?
        };
        view.ok_or_else(|| anyhow!("no input view"))
    }

    /// Draw `input` into `output`. `src` is the part of the input to use and
    /// `dst` where it lands in the output (the rest is filled black).
    pub(crate) fn blt(
        &self,
        input: &ID3D11VideoProcessorInputView,
        output: &ID3D11VideoProcessorOutputView,
        src: RECT,
        dst: RECT,
    ) -> Result<()> {
        unsafe {
            self.vctx
                .VideoProcessorSetStreamSourceRect(&self.vp, 0, true, Some(&src));
            self.vctx
                .VideoProcessorSetStreamDestRect(&self.vp, 0, true, Some(&dst));
            self.vctx
                .VideoProcessorSetOutputTargetRect(&self.vp, false, None);
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: ManuallyDrop::new(Some(input.clone())),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let mut streams = [stream];
            let r = self.vctx.VideoProcessorBlt(&self.vp, output, 0, &streams);
            ManuallyDrop::drop(&mut streams[0].pInputSurface);
            r?;
        }
        Ok(())
    }
}

/// Set a codec property, ignoring encoders that don't support it.
pub(crate) fn codec_set(api: &ICodecAPI, key: &GUID, value: windows::core::VARIANT) {
    unsafe {
        if let Err(e) = api.SetValue(key, &value) {
            tracing::debug!("codec property {key:?} not set: {e}");
        }
    }
}

/// Copy a sample's bytes out.
pub(crate) fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>> {
    unsafe {
        let buf = sample.ConvertToContiguousBuffer()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buf.Lock(&mut ptr, None, Some(&mut len))?;
        let out = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        let _ = buf.Unlock();
        Ok(out)
    }
}

/// A sample holding a copy of `bytes` in system memory.
pub(crate) fn memory_sample(bytes: &[u8]) -> Result<IMFSample> {
    unsafe {
        let buf = MFCreateMemoryBuffer(bytes.len() as u32)?;
        let mut ptr = std::ptr::null_mut();
        buf.Lock(&mut ptr, None, None)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        buf.Unlock()?;
        buf.SetCurrentLength(bytes.len() as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buf)?;
        Ok(sample)
    }
}

/// Stream ids of a one-in, one-out transform (most return E_NOTIMPL,
/// meaning 0 and 0).
pub(crate) fn stream_ids(t: &IMFTransform) -> (u32, u32) {
    let mut i = [0u32];
    let mut o = [0u32];
    unsafe {
        if t.GetStreamIDs(&mut i, &mut o).is_ok() {
            (i[0], o[0])
        } else {
            (0, 0)
        }
    }
}
