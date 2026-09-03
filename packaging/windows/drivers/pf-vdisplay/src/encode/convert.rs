//! The `windows` 0.58 → 0.62 bridge and the backend input [`Targets`]: one set of textures per
//! pool slot, in whatever format the opened backend reads, plus the converter that fills them
//! from a BGRA or FP16 source. [`Targets::pass`] is the fused pass the drain worker runs in the
//! acquire window; [`Targets::frame`] wraps a filled slot as the `CapturedFrame` `submit` takes.
//!
//! A blended pointer costs the window nothing: the pass then only copies the source into a
//! slot-sized RGB scratch, and `frame` (the encode thread) draws the cursor quad and runs the
//! converter from there. NVENC's BGRA slot is its own scratch, so that kind copies once either
//! way; the converter kinds pay one extra full-frame copy while the client draws no pointer.
//!
//! Every COM object the backends see is a [`bridge`]d `QueryInterface` of the driver's own
//! 0.58 object, so the two crates never wrap each other's pointer.

use std::sync::Arc;

use pf_encode_win::convert::{BgraToYuvPlanes, CursorBlendPass, HdrP010Converter, VideoConverter};
use pf_frame::dxgi::{D3d11Frame, PyroFrameShare};
use pf_frame::{CapturedFrame, CursorOverlay, FramePayload, PixelFormat, Provenance};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::Interface;
use windows62::Win32::Foundation::{CloseHandle, HANDLE};
use windows62::Win32::Graphics::Direct3D11 as d3d;
use windows62::Win32::Graphics::Dxgi::Common as dxgi;
use windows62::core::{Interface as _, PCWSTR};

use crate::cursor_cell::CursorImage;
use crate::direct_3d_device::Direct3DDevice;

/// A driver-domain failure: a small code for the reply and a stage tag; the log has the rest.
pub type Fail = (i32, &'static str);

type Tex = d3d::ID3D11Texture2D;
type Rtv = d3d::ID3D11RenderTargetView;
type Srv = d3d::ID3D11ShaderResourceView;

/// The pooled device's adapter, as the backends want it named: the LUID for NVENC/AMF/QSV,
/// the PCI ids for PyroWave (LUIDs are invalid in session 0).
#[derive(Clone, Copy, Debug)]
pub struct AdapterId {
    pub luid: LUID,
    pub vendor_id: u32,
    pub device_id: u32,
}

impl AdapterId {
    /// Read the adapter behind `device` once.
    pub fn of(device: &Direct3DDevice) -> Option<Self> {
        // SAFETY: plain queries on the live pooled device; each result is checked before use.
        let desc = unsafe {
            device
                .device
                .cast::<IDXGIDevice>()
                .ok()?
                .GetAdapter()
                .ok()?
                .GetDesc()
                .ok()?
        };
        Some(Self {
            luid: desc.AdapterLuid,
            vendor_id: desc.VendorId,
            device_id: desc.DeviceId,
        })
    }

    /// The LUID in the backends' `windows` version.
    pub fn luid62(&self) -> windows62::Win32::Foundation::LUID {
        windows62::Win32::Foundation::LUID {
            LowPart: self.luid.LowPart,
            HighPart: self.luid.HighPart,
        }
    }
}

/// A `windows` 0.62 view of a 0.58 COM object: a real `QueryInterface`, so the result owns
/// its own reference and neither crate ever wraps the other's pointer.
pub fn bridge<T: windows62::core::Interface>(obj: &impl Interface) -> Result<T, Fail> {
    let raw = obj.as_raw();
    // SAFETY: `raw` is the live COM pointer `obj` owns for the duration of this call;
    // `from_raw_borrowed` takes no reference of its own and `cast` AddRefs through QI.
    let unk = unsafe { windows62::core::IUnknown::from_raw_borrowed(&raw) };
    unk.ok_or((-8, "bridge"))?.cast::<T>().map_err(|e| {
        dbglog!("[pf-vd] encode: 0.58→0.62 bridge QI failed: {e:?}");
        (-8, "bridge")
    })
}

/// One default-usage texture on the bridged device with the given bind and misc flags.
pub fn make_tex62(
    dev: &d3d::ID3D11Device,
    (w, h): (u32, u32),
    format: dxgi::DXGI_FORMAT,
    bind: u32,
    misc: u32,
) -> Result<Tex, Fail> {
    let desc = d3d::D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: dxgi::DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: d3d::D3D11_USAGE_DEFAULT,
        BindFlags: bind,
        CPUAccessFlags: 0,
        MiscFlags: misc,
    };
    let mut t: Option<Tex> = None;
    // SAFETY: `desc` is a fully-initialized local; `t` a valid out-param checked below.
    let hr = unsafe { dev.CreateTexture2D(&desc, None, Some(&mut t)) };
    match (hr, t) {
        (Ok(()), Some(t)) => Ok(t),
        (r, _) => {
            dbglog!("[pf-vd] encode: CreateTexture2D({format:?}) failed: {r:?}");
            Err((-2, "pool"))
        }
    }
}

fn rtv(dev: &d3d::ID3D11Device, t: &Tex) -> Result<Rtv, Fail> {
    let mut v = None;
    // SAFETY: `t` is a live render-target texture on `dev`; `v` a valid out-param.
    unsafe { dev.CreateRenderTargetView(t, None, Some(&mut v)) }.map_err(|_| (-2, "rtv"))?;
    v.ok_or((-2, "rtv"))
}

fn srv(dev: &d3d::ID3D11Device, t: &Tex) -> Result<Srv, Fail> {
    let mut v = None;
    // SAFETY: `t` is a live shader-resource texture on `dev`; `v` a valid out-param.
    unsafe { dev.CreateShaderResourceView(t, None, Some(&mut v)) }.map_err(|_| (-2, "srv"))?;
    v.ok_or((-2, "srv"))
}

/// A shared D3D11 fence, its NT handle and the context that signals it — PyroWave's
/// cross-device ordering, one per target set. The handle closes with this value; the encoder
/// holds its own duplicate.
pub struct SharedFence {
    pub fence: d3d::ID3D11Fence,
    pub ctx4: d3d::ID3D11DeviceContext4,
    pub handle: HANDLE,
}

// SAFETY: the NT handle is a process-wide token this value alone closes; the COM objects are
// agile. Every use is serialized by the pool's state mutex.
unsafe impl Send for SharedFence {}
// SAFETY: as above — a shared reference hands out only by-value copies of the handle.
unsafe impl Sync for SharedFence {}

impl SharedFence {
    pub fn new(dev: &d3d::ID3D11Device, ctx: &d3d::ID3D11DeviceContext) -> Result<Self, Fail> {
        let fail = |what: &str, e: windows62::core::Error| {
            dbglog!("[pf-vd] encode: {what} failed: {e:?}");
            (-2, "fence")
        };
        let dev5: d3d::ID3D11Device5 = dev.cast().map_err(|e| fail("ID3D11Device5", e))?;
        let ctx4: d3d::ID3D11DeviceContext4 =
            ctx.cast().map_err(|e| fail("ID3D11DeviceContext4", e))?;
        let mut fence: Option<d3d::ID3D11Fence> = None;
        // SAFETY: `?`-checked calls on live interfaces; `fence` is a valid out-param checked
        // below. GENERIC_ALL (0x1000_0000) is the access the host hands pyrowave's import.
        let handle = unsafe {
            dev5.CreateFence(0, d3d::D3D11_FENCE_FLAG_SHARED, &mut fence)
                .map_err(|e| fail("CreateFence", e))?;
            fence
                .as_ref()
                .ok_or((-2, "fence"))?
                .CreateSharedHandle(None, 0x1000_0000, PCWSTR::null())
                .map_err(|e| fail("Fence CreateSharedHandle", e))?
        };
        Ok(Self {
            fence: fence.ok_or((-2, "fence"))?,
            ctx4,
            handle,
        })
    }
}

impl Drop for SharedFence {
    fn drop(&mut self) {
        // SAFETY: the NT handle `new` minted; the encoder holds its own duplicate.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// What a backend reads per frame, decided by the backend that opened and the session's HDR
/// and chroma flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// NVENC reads the BGRA slot as-is.
    Bgra,
    /// Video-engine BGRA→NV12 (AMF, QSV, the NVENC colour A/B).
    Nv12,
    /// Shader FP16 scRGB→P010 PQ (every HDR session but PyroWave).
    P010,
    /// BGRA/FP16→Y + CbCr shareable planes plus one shared fence, as the host feeds PyroWave.
    Planar { hdr: bool, chroma444: bool },
}

impl InputKind {
    /// The `PixelFormat` label the frame carries into `submit`.
    pub fn pixel_format(self) -> PixelFormat {
        match self {
            Self::Bgra => PixelFormat::Bgra,
            Self::Nv12 | Self::Planar { hdr: false, .. } => PixelFormat::Nv12,
            Self::P010 | Self::Planar { hdr: true, .. } => PixelFormat::P010,
        }
    }

    /// The source the pass reads: FP16 under advanced colour, BGRA otherwise.
    pub fn source_format(self) -> dxgi::DXGI_FORMAT {
        match self {
            Self::P010 | Self::Planar { hdr: true, .. } => dxgi::DXGI_FORMAT_R16G16B16A16_FLOAT,
            _ => dxgi::DXGI_FORMAT_B8G8R8A8_UNORM,
        }
    }
}

enum Planes {
    Bgra(Vec<Tex>),
    Nv12 {
        conv: VideoConverter,
        out: Vec<Tex>,
    },
    P010 {
        conv: HdrP010Converter,
        out: Vec<(Tex, Rtv, Rtv)>,
    },
    Planar {
        conv: BgraToYuvPlanes,
        y: Vec<(Tex, Rtv)>,
        cbcr: Vec<(Tex, Rtv)>,
        fence: SharedFence,
        fence_value: u64,
    },
}

/// The per-slot input targets for one backend, built once from the slot count.
pub struct Targets {
    kind: InputKind,
    dev: d3d::ID3D11Device,
    ctx: d3d::ID3D11DeviceContext,
    width: u32,
    height: u32,
    planes: Planes,
    /// Per slot: the source copy a deferred pass left for [`Self::frame`], in the source
    /// format with render-target and shader-resource binds. Made on first use.
    rgb: Vec<Option<(Tex, Srv)>>,
    /// Per slot: `rgb` holds the frame and the converter has not run yet.
    deferred: Vec<bool>,
    /// The cursor quad, built on first use; `None` after a build failure, logged once.
    blend: Option<CursorBlendPass>,
    blend_failed: bool,
}

impl Targets {
    pub fn new(
        kind: InputKind,
        dev: &d3d::ID3D11Device,
        ctx: &d3d::ID3D11DeviceContext,
        (w, h): (u32, u32),
        slots: usize,
    ) -> Result<Self, Fail> {
        let convert_err = |e: anyhow::Error| {
            dbglog!("[pf-vd] encode: converter build failed: {e:#}");
            (-2, "convert")
        };
        let rt = d3d::D3D11_BIND_RENDER_TARGET.0 as u32;
        let planes = match kind {
            InputKind::Bgra => {
                let bind = rt | d3d::D3D11_BIND_SHADER_RESOURCE.0 as u32;
                let slots = (0..slots)
                    .map(|_| make_tex62(dev, (w, h), dxgi::DXGI_FORMAT_B8G8R8A8_UNORM, bind, 0))
                    .collect::<Result<_, _>>()?;
                Planes::Bgra(slots)
            }
            InputKind::Nv12 => {
                let conv = VideoConverter::new(dev, ctx, w, h, false).map_err(convert_err)?;
                let out = (0..slots)
                    .map(|_| make_tex62(dev, (w, h), dxgi::DXGI_FORMAT_NV12, rt, 0))
                    .collect::<Result<_, _>>()?;
                Planes::Nv12 { conv, out }
            }
            InputKind::P010 => {
                let conv = HdrP010Converter::new(dev, w, h).map_err(convert_err)?;
                let mut out = Vec::with_capacity(slots);
                for _ in 0..slots {
                    let t = make_tex62(dev, (w, h), dxgi::DXGI_FORMAT_P010, rt, 0)?;
                    let y = HdrP010Converter::plane_rtv(dev, &t, dxgi::DXGI_FORMAT_R16_UNORM)
                        .map_err(convert_err)?;
                    let uv = HdrP010Converter::plane_rtv(dev, &t, dxgi::DXGI_FORMAT_R16G16_UNORM)
                        .map_err(convert_err)?;
                    out.push((t, y, uv));
                }
                Planes::P010 { conv, out }
            }
            InputKind::Planar { hdr, chroma444 } => {
                let conv = BgraToYuvPlanes::new(dev, hdr, chroma444).map_err(convert_err)?;
                let shared = (d3d::D3D11_RESOURCE_MISC_SHARED.0
                    | d3d::D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
                    as u32;
                let (yf, cf) = if hdr {
                    (dxgi::DXGI_FORMAT_R16_UNORM, dxgi::DXGI_FORMAT_R16G16_UNORM)
                } else {
                    (dxgi::DXGI_FORMAT_R8_UNORM, dxgi::DXGI_FORMAT_R8G8_UNORM)
                };
                let chroma = if chroma444 { (w, h) } else { (w / 2, h / 2) };
                let mut y = Vec::with_capacity(slots);
                let mut cbcr = Vec::with_capacity(slots);
                for _ in 0..slots {
                    let yt = make_tex62(dev, (w, h), yf, rt, shared)?;
                    let ct = make_tex62(dev, chroma, cf, rt, shared)?;
                    y.push((yt.clone(), rtv(dev, &yt)?));
                    cbcr.push((ct.clone(), rtv(dev, &ct)?));
                }
                let fence = SharedFence::new(dev, ctx)?;
                Planes::Planar {
                    conv,
                    y,
                    cbcr,
                    fence,
                    fence_value: 0,
                }
            }
        };
        Ok(Self {
            kind,
            dev: dev.clone(),
            ctx: ctx.clone(),
            width: w,
            height: h,
            planes,
            rgb: (0..slots).map(|_| None).collect(),
            deferred: vec![false; slots],
            blend: None,
            blend_failed: false,
        })
    }

    /// One GPU pass from `src` (BGRA or FP16, the pool's size) into slot `i`: a copy for BGRA,
    /// the video engine for NV12, a draw for P010 and the planar pair. With `defer` the
    /// converter kinds copy into the slot's RGB scratch instead and convert in [`Self::frame`],
    /// after the cursor blend. `src` needs no bind flags beyond what the shader kinds read
    /// through an SRV created per call.
    pub fn pass(&mut self, src: &Tex, i: usize, defer: bool) -> Result<(), Fail> {
        self.deferred[i] = false;
        if let Planes::Bgra(slots) = &self.planes {
            // SAFETY: `src` and the slot are live same-size textures on the same device, whose
            // immediate context is multithread-protected (`Direct3DDevice`).
            unsafe { self.ctx.CopyResource(&slots[i], src) };
            return Ok(());
        }
        if !defer {
            return self.convert(src, None, i);
        }
        if self.rgb[i].is_none() {
            let bind = (d3d::D3D11_BIND_RENDER_TARGET.0 | d3d::D3D11_BIND_SHADER_RESOURCE.0) as u32;
            let t = make_tex62(
                &self.dev,
                (self.width, self.height),
                self.kind.source_format(),
                bind,
                0,
            )?;
            let v = srv(&self.dev, &t)?;
            self.rgb[i] = Some((t, v));
        }
        let (scratch, _) = self.rgb[i].as_ref().ok_or((-2, "scratch"))?;
        // SAFETY: as the BGRA copy — same size and format by construction, same device.
        unsafe { self.ctx.CopyResource(scratch, src) };
        self.deferred[i] = true;
        Ok(())
    }

    /// The converter for slot `i` from `src`; `view` is its cached SRV, else one is made.
    fn convert(&self, src: &Tex, view: Option<&Srv>, i: usize) -> Result<(), Fail> {
        let convert_err = |e: anyhow::Error| {
            dbglog!("[pf-vd] encode: convert failed: {e:#}");
            (-2, "convert")
        };
        let view = || match view {
            Some(v) => Ok(v.clone()),
            None => srv(&self.dev, src),
        };
        match &self.planes {
            Planes::Bgra(_) => {}
            Planes::Nv12 { conv, out } => conv.convert(src, &out[i]).map_err(convert_err)?,
            Planes::P010 { conv, out } => conv
                .convert(
                    &self.ctx,
                    &view()?,
                    &out[i].1,
                    &out[i].2,
                    self.width,
                    self.height,
                )
                .map_err(convert_err)?,
            Planes::Planar { conv, y, cbcr, .. } => conv
                .convert(
                    &self.ctx,
                    &view()?,
                    &y[i].1,
                    &cbcr[i].1,
                    self.width,
                    self.height,
                )
                .map_err(convert_err)?,
        }
        Ok(())
    }

    /// Draw `cursor` over slot `i`'s RGB image — the BGRA slot itself, or the deferred scratch.
    /// A failure loses the pointer, never the frame, and is logged once.
    fn blend(&mut self, i: usize, cursor: &CursorImage, scale: f32) {
        let fp16 = self.kind.source_format() == dxgi::DXGI_FORMAT_R16G16B16A16_FLOAT;
        let dst = match &self.planes {
            Planes::Bgra(slots) => slots[i].clone(),
            _ if self.deferred[i] => match &self.rgb[i] {
                Some((t, _)) => t.clone(),
                None => return,
            },
            _ => return,
        };
        if self.blend.is_none() && !self.blend_failed {
            match CursorBlendPass::new(&self.dev) {
                Ok(p) => self.blend = Some(p),
                Err(e) => {
                    self.blend_failed = true;
                    dbglog!("[pf-vd] encode: cursor blend pass failed to build: {e:#}");
                }
            }
        }
        let Some(pass) = self.blend.as_mut() else {
            return;
        };
        let overlay = CursorOverlay {
            x: cursor.x,
            y: cursor.y,
            w: cursor.w,
            h: cursor.h,
            rgba: Arc::clone(&cursor.rgba),
            serial: u64::from(cursor.serial),
            hot_x: cursor.hot_x,
            hot_y: cursor.hot_y,
            visible: true,
        };
        let linear_scale = if fp16 { scale } else { 0.0 };
        if let Err(e) = pass.blend(&self.dev, &self.ctx, &dst, &overlay, linear_scale)
            && !self.blend_failed
        {
            self.blend_failed = true;
            dbglog!("[pf-vd] encode: cursor blend draw failed: {e:#}");
        }
    }

    /// Spike S6: wrap the acquired surface itself as the frame `submit` takes — no pass, no
    /// slot, and no pointer, because there is no driver-owned image to draw one on. Only the
    /// BGRA kind reaches this; every other kind's converter has to run first.
    pub fn direct_frame(&self, src: &Tex, pts_ns: u64) -> CapturedFrame {
        CapturedFrame {
            width: self.width,
            height: self.height,
            pts_ns,
            format: self.kind.pixel_format(),
            payload: FramePayload::D3d11(D3d11Frame {
                texture: src.clone(),
                device: self.dev.clone(),
                pyro: None,
            }),
            cursor: None,
            provenance: Provenance::UNTRACKED,
        }
    }

    /// Wrap filled slot `i` as the frame `submit` takes, after the cursor blend and the
    /// converter a deferred pass left for this thread. The planar pair signals its fence here,
    /// so the Vulkan wait orders after the pass however far apart the two threads ran.
    pub fn frame(
        &mut self,
        i: usize,
        pts_ns: u64,
        cursor: Option<(CursorImage, f32)>,
    ) -> Result<CapturedFrame, Fail> {
        if let Some((image, scale)) = cursor {
            self.blend(i, &image, scale);
        }
        if self.deferred[i] {
            self.deferred[i] = false;
            let (t, v) = self.rgb[i].clone().ok_or((-2, "scratch"))?;
            self.convert(&t, Some(&v), i)?;
        }
        let (texture, pyro) = match &mut self.planes {
            Planes::Bgra(slots) => (slots[i].clone(), None),
            Planes::Nv12 { out, .. } => (out[i].clone(), None),
            Planes::P010 { out, .. } => (out[i].0.clone(), None),
            Planes::Planar {
                y,
                cbcr,
                fence,
                fence_value,
                ..
            } => {
                *fence_value += 1;
                // SAFETY: `fence` is the live shared fence on this context's device; `Flush`
                // submits the queued convert + signal so the Vulkan wait can resolve.
                unsafe {
                    fence
                        .ctx4
                        .Signal(&fence.fence, *fence_value)
                        .map_err(|_| (-2, "fence"))?;
                    self.ctx.Flush();
                }
                let share = PyroFrameShare {
                    cbcr: cbcr[i].0.clone(),
                    fence_handle: Some(fence.handle.0 as isize),
                    fence_value: *fence_value,
                    ring_gen: 1,
                };
                (y[i].0.clone(), Some(share))
            }
        };
        Ok(CapturedFrame {
            width: self.width,
            height: self.height,
            pts_ns,
            format: self.kind.pixel_format(),
            payload: FramePayload::D3d11(D3d11Frame {
                texture,
                device: self.dev.clone(),
                pyro,
            }),
            cursor: None,
            provenance: Provenance::UNTRACKED,
        })
    }
}
