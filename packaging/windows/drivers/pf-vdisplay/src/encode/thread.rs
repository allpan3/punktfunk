//! The encode thread (`pf-vd-encode`): opens a backend inside WUDFHost, reports the outcome to
//! the `SET_ENCODE` caller, then feeds the encoder from the pool and publishes into the AU
//! section. One per [`EncodeSession`]; a wedged one is detached, never joined without a bound.
//!
//! [`open_backend`] is one `open` per [`OpenSpec::backend`]; [`EncodeThread`] walks the
//! request's preference list and reports what took. PyroWave's private Vulkan instance goes
//! through the box's implicit layers unless [`disable_implicit_vulkan_layers`] ran first:
//! overlays hang in session 0, where there is no desktop to hook.

use std::mem::offset_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Weak};
use std::time::Duration;

use pf_driver_proto::encode::DRV_STATUS_OPENED;
use pf_driver_proto::encode::au::{self, AuHeader};
use pf_driver_proto::encode::{self as wire, EncoderCapsWire, SetEncodeReply, SetEncodeRequest};
use pf_encode_win::{ChromaFormat, Codec, Encoder, EncoderCaps};
use pf_frame::HdrMeta;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use super::convert::{AdapterId, Fail, InputKind};
use super::drive::Drive;
use super::pool::Pool;
use super::section::{AuSection, EncodeSession};
use crate::direct_3d_device::Direct3DDevice;
use crate::monitor::Monitor;
use crate::worker::{Mmcss, Worker};

const BACKEND_NAMES: [&str; 4] = ["nvenc", "amf", "qsv", "pyrowave"];

/// A failed `SET_ENCODE` as the wire reply: `status` from the driver's domain, the stage tag
/// in `name`.
pub fn fail_reply(status: u32, (error, name): Fail) -> SetEncodeReply {
    let mut reply = SetEncodeReply {
        status,
        error,
        ..bytemuck::Zeroable::zeroed()
    };
    let n = name.len().min(32);
    reply.name[..n].copy_from_slice(&name.as_bytes()[..n]);
    reply
}

/// `pf_frame::HdrMeta` from its 28 `repr(C)` bytes.
pub fn hdr_meta(bytes: &[u8; 28]) -> HdrMeta {
    // SAFETY: `HdrMeta` is `repr(C)`, 28 bytes of plain integers with no invalid bit pattern;
    // `read_unaligned` copies them out of the request's byte array.
    unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<HdrMeta>()) }
}

/// The request's `open` spec for backend `backend` of its list.
pub fn spec_for(req: &SetEncodeRequest, backend: u32) -> Result<OpenSpec, Fail> {
    let (hdr, chroma444) = (req.hdr == 1, req.chroma == 1);
    let kind = match (backend, hdr) {
        (4, _) => InputKind::Planar { hdr, chroma444 },
        (_, true) => InputKind::P010,
        (1, false) => InputKind::Bgra,
        _ => InputKind::Nv12,
    };
    Ok(OpenSpec {
        backend,
        codec: codec_from_wire(req.codec).ok_or((-4, "codec"))?,
        kind,
        width: req.width,
        height: req.height,
        fps: req.fps.max(1),
        bitrate_bps: u64::from(req.bitrate_kbps) * 1000,
        bit_depth: if req.bit_depth >= 10 { 10 } else { 8 },
        chroma: if chroma444 {
            ChromaFormat::Yuv444
        } else {
            ChromaFormat::Yuv420
        },
    })
}

fn caps_wire(c: EncoderCaps) -> EncoderCapsWire {
    EncoderCapsWire {
        supports_rfi: u32::from(c.supports_rfi),
        chroma_444: u32::from(c.chroma_444),
        intra_refresh: u32::from(c.intra_refresh),
        intra_refresh_recovery: u32::from(c.intra_refresh_recovery),
        intra_refresh_period: c.intra_refresh_period,
        blends_cursor: u32::from(c.blends_cursor),
    }
}

/// Walk the request's backend list in order; the first that opens wins. `Err` is the last
/// failure as the wire reply — no silent fallback past the list.
fn open_listed(
    req: &SetEncodeRequest,
    adapter: &AdapterId,
) -> Result<(Box<dyn Encoder>, OpenSpec, SetEncodeReply), SetEncodeReply> {
    let mut last: Fail = (-1, "nobackend");
    for &backend in req.backends.iter().take_while(|&&b| b != 0) {
        let spec = match spec_for(req, backend) {
            Ok(s) => s,
            Err(f) => {
                last = f;
                continue;
            }
        };
        match open_backend(&spec, adapter) {
            Ok(mut enc) => {
                if req.wire_chunk_bytes != 0 {
                    enc.set_wire_chunking(req.wire_chunk_bytes as usize);
                }
                if req.hdr == 1 {
                    enc.set_hdr_meta(Some(hdr_meta(&req.hdr_meta)));
                }
                let applied = enc.applied_bitrate_bps().unwrap_or(spec.bitrate_bps);
                let mut reply = fail_reply(
                    wire::SET_ENCODE_OK,
                    (0, BACKEND_NAMES[backend as usize - 1]),
                );
                reply.backend_opened = backend;
                reply.caps = caps_wire(enc.caps());
                reply.applied_bitrate_kbps = (applied / 1000) as u32;
                return Ok((enc, spec, reply));
            }
            Err(f) => last = f,
        }
    }
    Err(fail_reply(wire::SET_ENCODE_NO_BACKEND, last))
}

/// What the thread runs with. The `opened` channel carries exactly one reply: the open's.
/// `monitor` is used during set-up only — a detached thread must not pin its monitor.
pub struct ThreadCtx {
    pub session: Arc<EncodeSession>,
    pub monitor: Weak<Monitor>,
    pub device: Arc<Direct3DDevice>,
    pub opened: SyncSender<SetEncodeReply>,
}

/// The running encode thread of one session.
pub struct EncodeThread {
    worker: Option<Worker>,
    /// Cleared on detach: the thread touches neither pool nor section once this is false.
    live: Arc<AtomicBool>,
}

impl EncodeThread {
    /// How long a stop waits before the thread is detached. A healthy thread is between two
    /// backend calls within one frame; ~250 ms is ten of them at 60 Hz.
    pub const STOP_BOUND: Duration = Duration::from_millis(250);

    /// Start the thread. `None` when the OS refused a thread or event; the caller replies
    /// [`wire::SET_ENCODE_THREAD`].
    pub fn spawn(ctx: ThreadCtx) -> Option<Self> {
        let live = Arc::new(AtomicBool::new(true));
        let thread_live = live.clone();
        let worker = Worker::spawn("pf-vd-encode", move |stop| run(stop, ctx, thread_live))?;
        Some(Self {
            worker: Some(worker),
            live,
        })
    }

    /// Stop within [`Self::STOP_BOUND`]; a thread that does not return is detached and counted
    /// in the section's `detached` word — the host's `DriverCycle` threshold reads it.
    pub fn stop(mut self, section: &AuSection) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        if !worker.stop_within(Self::STOP_BOUND) {
            self.live.store(false, Ordering::Release);
            let n = section.add_u32(offset_of!(AuHeader, detached), 1);
            dbglog!("[pf-vd] encode: thread detached (total {n})");
        }
    }
}

/// The thread body: open, build or reuse the monitor's pool, report, then drive until stopped.
/// The pool is reused — retained slot included — when it already fits this session's device,
/// size and input kind; anything else is a fresh pool installed on the monitor. The open line
/// names the frame path (`pool` or S6's `bypass`), so a comparison run can prove which it got.
fn run(stop: HANDLE, ctx: ThreadCtx, live: Arc<AtomicBool>) {
    let _mmcss = Mmcss::distribution("encode");
    let section = &ctx.session.section;
    let fail = |status, f| {
        let _ = ctx.opened.send(fail_reply(status, f));
    };
    let Some(adapter) = AdapterId::of(&ctx.device) else {
        return fail(wire::SET_ENCODE_NO_DEVICE, (-5, "adapter"));
    };
    let Some(monitor) = ctx.monitor.upgrade() else {
        return fail(wire::SET_ENCODE_NO_MONITOR, (-6, "gone"));
    };
    let (enc, spec, reply) = match open_listed(&ctx.session.request, &adapter) {
        Ok(x) => x,
        Err(reply) => {
            let _ = ctx.opened.send(reply);
            return;
        }
    };
    let size = (spec.width, spec.height);
    let reused = monitor
        .pool()
        .filter(|p| p.matches(&ctx.device, spec.kind, size));
    let pool = match reused {
        Some(p) => p,
        None => match Pool::build(
            &ctx.device,
            spec.kind,
            size,
            monitor.source_seq.clone(),
            monitor.cursor_cell(),
        ) {
            Ok(p) => {
                monitor.set_pool(p.clone());
                p
            }
            Err(f) => return fail(wire::SET_ENCODE_POOL, f),
        },
    };
    drop(monitor);
    dbglog!(
        "[pf-vd] encode: backend {} open {}x{} {:?} mode={} (target {})",
        reply.backend_opened,
        spec.width,
        spec.height,
        spec.kind,
        if pool.bypass() { "bypass" } else { "pool" },
        ctx.session.request.target_id
    );
    section.store_u32(offset_of!(AuHeader, driver_status), DRV_STATUS_OPENED);
    section.store_u32(
        offset_of!(AuHeader, driver_status_detail),
        reply.backend_opened,
    );
    section.store_u32(offset_of!(AuHeader, encoder_state), au::ENCODER_OPEN);
    if ctx.opened.send(reply).is_err() {
        // The caller gave up waiting: nothing will install this session.
        return;
    }
    Drive::new(enc, &pool, &ctx.session, stop, &live).run();
    if live.load(Ordering::Acquire) {
        section.store_u32(offset_of!(AuHeader, encoder_state), au::ENCODER_CLOSED);
    }
}

/// Everything one backend `open` takes, in the backends' own vocabulary.
#[derive(Clone, Copy, Debug)]
pub struct OpenSpec {
    /// 1 NVENC, 2 AMF, 3 QSV, 4 PyroWave.
    pub backend: u32,
    pub codec: Codec,
    pub kind: InputKind,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u64,
    pub bit_depth: u8,
    pub chroma: ChromaFormat,
}

/// The wire codec numbering (1 H264, 2 HEVC, 3 AV1, 4 PyroWave).
pub fn codec_from_wire(codec: u32) -> Option<Codec> {
    Some(match codec {
        1 => Codec::H264,
        2 => Codec::H265,
        3 => Codec::Av1,
        4 => Codec::PyroWave,
        _ => return None,
    })
}

/// Implicit Vulkan layers (overlays, our pf-vkhdr-layer) hang in session 0, and the encoder's
/// private instance wants none of them. The loader-wide knob needs a 1.3.234+ loader; each
/// manifest's own `disable_environment` works on any. Once per process.
pub fn disable_implicit_vulkan_layers() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: WUDFHost is this driver's own process (`ProcessSharingDisabled`); Windows'
        // SetEnvironmentVariable is thread-safe and nothing here parses the environment
        // concurrently.
        unsafe {
            for (k, v) in [
                ("VK_LOADER_LAYERS_DISABLE", "~implicit~"),
                ("DISABLE_RTSS_LAYER", "1"),
                ("DISABLE_PF_VKHDR", "1"),
                ("DISABLE_VK_LAYER_VALVE_steam_overlay_1", "1"),
                ("DISABLE_VK_LAYER_VALVE_steam_fossilize_1", "1"),
                ("EOS_OVERLAY_DISABLE_VULKAN_WIN64", "1"),
                ("DISABLE_GALAXY_OVERLAY", "1"),
            ] {
                std::env::set_var(k, v);
            }
        }
    });
}

/// One backend open on `adapter`. `Err` carries the stage tag; the backend's message is logged.
pub fn open_backend(spec: &OpenSpec, adapter: &AdapterId) -> Result<Box<dyn Encoder>, Fail> {
    let (w, h, fps, bps) = (spec.width, spec.height, spec.fps, spec.bitrate_bps);
    let (depth, chroma) = (spec.bit_depth, spec.chroma);
    let format = spec.kind.pixel_format();
    let luid = Some(adapter.luid62());
    let opened: anyhow::Result<Box<dyn Encoder>> = match spec.backend {
        1 => pf_encode_win::nvenc::NvencD3d11Encoder::open(
            spec.codec, format, w, h, fps, bps, depth, chroma, 1, luid,
        )
        .map(|e| Box::new(e) as Box<dyn Encoder>),
        2 => pf_encode_win::amf::AmfEncoder::open(
            spec.codec, format, w, h, fps, bps, depth, chroma, luid,
        )
        .map(|e| Box::new(e) as Box<dyn Encoder>),
        3 => pf_encode_win::qsv::QsvEncoder::open(
            spec.codec, format, w, h, fps, bps, depth, chroma, luid,
        )
        .map(|e| Box::new(e) as Box<dyn Encoder>),
        4 => {
            disable_implicit_vulkan_layers();
            pf_encode_win::pyrowave::PyroWaveEncoder::open(
                w,
                h,
                fps,
                bps,
                chroma,
                depth,
                adapter.vendor_id,
                adapter.device_id,
            )
            .map(|e| Box::new(e) as Box<dyn Encoder>)
        }
        _ => return Err((-1, "backend")),
    };
    opened.map_err(|e| {
        dbglog!(
            "[pf-vd] encode: backend {} open FAILED: {e:#}",
            spec.backend
        );
        (-1, "open")
    })
}

pub fn qpc_now() -> u64 {
    let mut qpc = 0i64;
    // SAFETY: plain FFI; `qpc` is a valid local out-param.
    let _ = unsafe { QueryPerformanceCounter(&mut qpc) };
    qpc as u64
}

pub fn qpc_frequency() -> u64 {
    let mut hz = 0i64;
    // SAFETY: plain FFI; `hz` is a valid local out-param.
    let _ = unsafe { QueryPerformanceFrequency(&mut hz) };
    (hz as u64).max(1)
}

pub fn qpc_to_ns(qpc: u64, hz: u64) -> u64 {
    (u128::from(qpc) * 1_000_000_000 / u128::from(hz)) as u64
}
