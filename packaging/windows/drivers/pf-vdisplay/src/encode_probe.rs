//! Spike S5 (`--features encode-probe`, design/windows-video-plane-overhaul.md §3): the
//! encoder backends opened and driven INSIDE WUDFHost. `IOCTL_ENCODE_PROBE_ARM` parks a
//! request; the drain worker then copies each acquired surface into a three-slot pool
//! ([`offer`]: one `CopyResource`, one `SetEvent`) and the `pf-vd-probe` thread does the rest —
//! converts, submits, polls, files the AUs. `IOCTL_ENCODE_PROBE_STATUS` reads the tally.
//!
//! The pool lives on the pooled `windows` 0.58 device; the backends speak `windows` 0.62, so
//! every COM object they see is a [`bridge`]d `QueryInterface` of the same underlying object.
//! Never shippable — see the feature note in Cargo.toml.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pf_driver_proto::control::{EncodeProbeReply, EncodeProbeRequest};
use pf_encode_win::convert::{BgraToYuvPlanes, VideoConverter};
use pf_encode_win::{ChromaFormat, Codec, EncodedFrame, Encoder};
use pf_frame::dxgi::{D3d11Frame, PyroFrameShare};
use pf_frame::{CapturedFrame, FramePayload, PixelFormat, Provenance};
use wdk_sys::NTSTATUS;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};
use windows::core::Interface;
use windows62::Win32::Graphics::Direct3D11 as d3d;
use windows62::Win32::Graphics::Dxgi::Common as dxgi;
use windows62::core::{Interface as _, PCWSTR};

use crate::direct_3d_device::{Direct3DDevice, pooled_device};
use crate::registry::lock;
use crate::worker::Worker;
use crate::{STATUS_INVALID_PARAMETER, STATUS_SUCCESS};

/// Backend names in the order the addresses below pin them, for the load-time log.
pub fn backends_linked() -> &'static [&'static str] {
    &["nvenc", "amf", "qsv", "pyrowave", "convert"]
}

// `nvidia-video-codec-sdk` (feature `ci-check`) links no import library, and the DLL still pulls
// its `EncodeAPI` object, which names these two entry points. The NVENC backend resolves both
// from `nvEncodeAPI64.dll` at runtime and never calls these; they only satisfy the linker.
// Not `pub`: internal linkage only, nothing is exported from the DLL.
#[unsafe(no_mangle)]
extern "C" fn NvEncodeAPICreateInstance(_list: *mut core::ffi::c_void) -> u32 {
    // NV_ENC_ERR_NO_ENCODE_DEVICE
    1
}

#[unsafe(no_mangle)]
extern "C" fn NvEncodeAPIGetMaxSupportedVersion(_version: *mut u32) -> u32 {
    // NV_ENC_ERR_NO_ENCODE_DEVICE
    1
}

const STATUS_UNSUCCESSFUL: NTSTATUS = 0xC000_0001u32 as NTSTATUS;
const STATUS_DEVICE_BUSY: NTSTATUS = 0x8000_0011u32 as NTSTATUS;

const ST_ARMED: u32 = 1;
const ST_RUNNING: u32 = 2;
const ST_DONE: u32 = 3;
const ST_FAILED: u32 = 4;

/// Three slots so the drain worker can bank two frames while one encodes (host OUT_RING).
const SLOTS: usize = 3;
/// Submits allowed ahead of the oldest AU — the host's pipeline depth.
const MAX_INFLIGHT: usize = 2;
const BACKENDS: [&str; 4] = ["nvenc", "amf", "qsv", "pyrowave"];
const CODECS: [&str; 4] = ["h264", "hevc", "av1", "pyrowave"];

type Fail = (i32, &'static str);

/// What the first acquired surface told the hook: the pool's shape and the GPU behind it.
#[derive(Clone, Copy)]
struct Primed {
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    luid: LUID,
    vendor_id: u32,
    device_id: u32,
}

enum Ring {
    /// No surface seen yet: the next one is described, not copied.
    Priming,
    /// Described; the probe thread is building the pool.
    Described(Primed),
    Live {
        slots: Vec<ID3D11Texture2D>,
        free: Vec<usize>,
        /// `(slot, PresentDisplayQPCTime)` in acquire order.
        full: VecDeque<(usize, u64)>,
    },
}

/// What the drain hook and the probe thread share. Dropped by whichever holder is last, so
/// the event closes only after any hook call still holding an `Arc` has returned.
struct Shared {
    target_id: u32,
    /// Auto-reset, signalled once per copied frame.
    event: isize,
    ring: Mutex<Ring>,
    /// Pool device epoch: a hook on a different (recreated) device skips the copy.
    device_epoch: AtomicU32,
    drops: AtomicU32,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: our own event, created in `arm`; this is its sole close.
        unsafe {
            let _ = CloseHandle(HANDLE(self.event as *mut _));
        }
    }
}

struct Probe {
    reply: EncodeProbeReply,
    worker: Option<Worker>,
}

const ZERO_REPLY: EncodeProbeReply = EncodeProbeReply {
    state: 0,
    backend_opened: 0,
    frames_submitted: 0,
    aus: 0,
    bytes: 0,
    open_us: 0,
    first_au_us: 0,
    mean_submit_to_au_us: 0,
    max_submit_to_au_us: 0,
    drops: 0,
    error: 0,
    name: [0; 32],
};

static PROBE: Mutex<Probe> = Mutex::new(Probe {
    reply: ZERO_REPLY,
    worker: None,
});
/// The hook's handle to the run; `None` between runs.
static SHARED: Mutex<Option<Arc<Shared>>> = Mutex::new(None);
/// Fast path for the drain hook: one relaxed load per frame while no run is armed.
static ARMED: AtomicBool = AtomicBool::new(false);

/// `IOCTL_ENCODE_PROBE_ARM`: start a run. `STATUS_DEVICE_BUSY` while one is going.
pub fn arm(req: &EncodeProbeRequest) -> NTSTATUS {
    let mut p = lock(&PROBE);
    if matches!(p.reply.state, ST_ARMED | ST_RUNNING) {
        return STATUS_DEVICE_BUSY;
    }
    let valid = (1..=4).contains(&req.backend)
        && (1..=4).contains(&req.codec)
        && (req.backend == 4) == (req.codec == 4);
    if !valid {
        return STATUS_INVALID_PARAMETER;
    }
    // The previous run's thread has exited (its state is done/failed): the join is immediate.
    drop(p.worker.take());
    // SAFETY: plain event creation — auto-reset, unsignalled, unnamed, no security descriptor.
    let Ok(event) = (unsafe { CreateEventW(None, false, false, None) }) else {
        return STATUS_UNSUCCESSFUL;
    };
    let shared = Arc::new(Shared {
        target_id: req.target_id,
        event: event.0 as isize,
        ring: Mutex::new(Ring::Priming),
        device_epoch: AtomicU32::new(0),
        drops: AtomicU32::new(0),
    });
    let req = *req;
    let thread_shared = shared.clone();
    let Some(worker) = Worker::spawn("pf-vd-probe", move |stop| run(stop, req, thread_shared))
    else {
        return STATUS_UNSUCCESSFUL;
    };
    p.reply = ZERO_REPLY;
    p.reply.state = ST_ARMED;
    p.worker = Some(worker);
    *lock(&SHARED) = Some(shared);
    ARMED.store(true, Ordering::Release);
    dbglog!(
        "[pf-vd] probe: armed backend={} codec={} input={} frames={} target={}",
        BACKENDS[req.backend as usize - 1],
        CODECS[req.codec as usize - 1],
        req.input,
        req.frames,
        req.target_id
    );
    STATUS_SUCCESS
}

/// `IOCTL_ENCODE_PROBE_STATUS`: the tally so far.
pub fn status() -> EncodeProbeReply {
    let mut reply = lock(&PROBE).reply;
    if let Some(s) = lock(&SHARED).as_ref() {
        reply.drops = s.drops.load(Ordering::Relaxed);
    }
    reply
}

/// The drain worker's hook, per acquired surface: a `CopyResource` into a free pool slot and
/// a `SetEvent`, or a counted drop. Never blocks — every lock is a `try_lock`.
pub fn offer(device: &Direct3DDevice, tex: &ID3D11Texture2D, display_qpc: u64, target_id: u32) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let Some(shared) = SHARED.try_lock().ok().and_then(|s| s.clone()) else {
        return;
    };
    if shared.target_id != 0 && shared.target_id != target_id {
        return;
    }
    let Ok(mut ring) = shared.ring.try_lock() else {
        shared.drops.fetch_add(1, Ordering::Relaxed);
        return;
    };
    match &mut *ring {
        Ring::Priming => {
            if let Some(p) = describe(device, tex) {
                shared.device_epoch.store(device.epoch(), Ordering::Release);
                *ring = Ring::Described(p);
                signal(&shared);
            }
        }
        Ring::Described(_) => {}
        Ring::Live { slots, free, full } => {
            if shared.device_epoch.load(Ordering::Acquire) != device.epoch() {
                return;
            }
            let Some(i) = free.pop() else {
                shared.drops.fetch_add(1, Ordering::Relaxed);
                return;
            };
            // SAFETY: `tex` is the live acquired surface and `slots[i]` a same-size, same-format
            // pool texture on the same pooled device, whose immediate context is
            // multithread-protected (`Direct3DDevice`).
            unsafe { device.device_context.CopyResource(&slots[i], tex) };
            full.push_back((i, display_qpc));
            signal(&shared);
        }
    }
}

fn signal(shared: &Shared) {
    // SAFETY: the event lives as long as `shared`, which the caller holds an `Arc` of.
    unsafe {
        let _ = SetEvent(HANDLE(shared.event as *mut _));
    }
}

/// The surface's shape plus the pooled device's adapter identity, read once at priming.
fn describe(device: &Direct3DDevice, tex: &ID3D11Texture2D) -> Option<Primed> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `tex` is the live acquired surface; `desc` is a valid local out-param.
    unsafe { tex.GetDesc(&mut desc) };
    // SAFETY: plain queries on the live pooled device; each result is checked before use.
    let adapter = unsafe {
        device
            .device
            .cast::<IDXGIDevice>()
            .ok()?
            .GetAdapter()
            .ok()?
            .GetDesc()
            .ok()?
    };
    Some(Primed {
        width: desc.Width,
        height: desc.Height,
        format: desc.Format,
        luid: adapter.AdapterLuid,
        vendor_id: adapter.VendorId,
        device_id: adapter.DeviceId,
    })
}

/// A `windows` 0.62 view of a 0.58 COM object: a real `QueryInterface`, so the result owns
/// its own reference and neither crate ever wraps the other's pointer.
fn bridge<T: windows62::core::Interface>(obj: &impl Interface) -> Result<T, Fail> {
    let raw = obj.as_raw();
    // SAFETY: `raw` is the live COM pointer `obj` owns for the duration of this call;
    // `from_raw_borrowed` takes no reference of its own and `cast` AddRefs through QI.
    let unk = unsafe { windows62::core::IUnknown::from_raw_borrowed(&raw) };
    unk.ok_or((-8, "bridge"))?.cast::<T>().map_err(|e| {
        dbglog!("[pf-vd] probe: 0.58→0.62 bridge QI failed: {e:?}");
        (-8, "bridge")
    })
}

/// The probe thread. Everything after the hook's copy happens here; the outcome lands in
/// [`PROBE`] and the ring handle is released so the hook goes back to one atomic load.
fn run(stop: HANDLE, req: EncodeProbeRequest, shared: Arc<Shared>) {
    let outcome = drive(stop, &req, &shared);
    ARMED.store(false, Ordering::Release);
    let released = lock(&SHARED).take();
    drop(released);
    let mut p = lock(&PROBE);
    p.reply.drops = shared.drops.load(Ordering::Relaxed);
    match outcome {
        Ok(()) => {
            p.reply.state = ST_DONE;
            let r = &p.reply;
            dbglog!(
                "[pf-vd] probe: done frames={} aus={} bytes={} open_us={} first_au_us={} mean_us={} max_us={} drops={}",
                r.frames_submitted,
                r.aus,
                r.bytes,
                r.open_us,
                r.first_au_us,
                r.mean_submit_to_au_us,
                r.max_submit_to_au_us,
                shared.drops.load(Ordering::Relaxed)
            );
        }
        Err((code, name)) => {
            p.reply.state = ST_FAILED;
            p.reply.error = code;
            p.reply.name = [0; 32];
            let n = name.len().min(32);
            p.reply.name[..n].copy_from_slice(&name.as_bytes()[..n]);
            dbglog!("[pf-vd] probe: FAILED {name} ({code})");
        }
    }
}

fn drive(stop: HANDLE, req: &EncodeProbeRequest, shared: &Arc<Shared>) -> Result<(), Fail> {
    let primed = wait_primed(stop, shared)?;
    dbglog!(
        "[pf-vd] probe: primed {}x{} fmt={:?} luid={:08x}:{:08x} gpu={:04x}:{:04x}",
        primed.width,
        primed.height,
        primed.format,
        primed.luid.HighPart,
        primed.luid.LowPart,
        primed.vendor_id,
        primed.device_id
    );
    if primed.format != DXGI_FORMAT_B8G8R8A8_UNORM {
        return Err((-4, "fmt"));
    }
    let dev = pooled_device(primed.luid).ok_or((-5, "device"))?;
    shared.device_epoch.store(dev.epoch(), Ordering::Release);
    let (w, h) = (primed.width, primed.height);
    let slots = (0..SLOTS)
        .map(|_| make_slot(&dev, w, h, primed.format))
        .collect::<Result<Vec<_>, _>>()?;
    let dev62: d3d::ID3D11Device = bridge(&dev.device)?;
    let ctx62: d3d::ID3D11DeviceContext = bridge(&dev.device_context)?;
    let slots62 = slots
        .iter()
        .map(|s| bridge::<d3d::ID3D11Texture2D>(s))
        .collect::<Result<Vec<_>, _>>()?;
    let mut input = Input::new(req, &dev62, &ctx62, slots62, w, h)?;

    let t0 = Instant::now();
    let mut enc = open_backend(req, w, h, &primed)?;
    let open_us = t0.elapsed().as_micros() as u32;
    dbglog!("[pf-vd] probe: backend open OK in {open_us} us");
    {
        let mut p = lock(&PROBE);
        p.reply.backend_opened = 1;
        p.reply.open_us = open_us;
        p.reply.state = ST_RUNNING;
    }
    *lock(&shared.ring) = Ring::Live {
        slots,
        free: (0..SLOTS).collect(),
        full: VecDeque::new(),
    };

    let path = std::env::temp_dir().join(format!(
        "pfvd-probe-{}-{}.bin",
        BACKENDS[req.backend as usize - 1],
        CODECS[req.codec as usize - 1]
    ));
    let file = std::fs::File::create(&path).map_err(|_| (-6, "file"))?;
    let mut sink = Sink {
        shared: shared.clone(),
        file: std::io::BufWriter::new(file),
        inflight: VecDeque::new(),
        aus: 0,
        lat_sum: 0,
    };
    let frames = if req.frames == 0 { 300 } else { req.frames };
    let qpc_hz = qpc_frequency();
    for n in 0..frames {
        let (slot, qpc) = wait_frame(stop, shared)?;
        let pts_ns = qpc_to_ns(if qpc == 0 { qpc_now() } else { qpc }, qpc_hz);
        let frame = input.prepare(slot, pts_ns)?;
        let submitted = Instant::now();
        enc.submit(&frame).map_err(|e| {
            dbglog!("[pf-vd] probe: submit #{n} failed: {e:#}");
            (-1, "submit")
        })?;
        sink.inflight.push_back((slot, submitted));
        lock(&PROBE).reply.frames_submitted = n + 1;
        sink.collect(enc.as_mut(), MAX_INFLIGHT)?;
    }
    if let Err(e) = enc.flush() {
        dbglog!("[pf-vd] probe: flush failed: {e:#}");
    }
    // A backend that keeps its last AU past flush is a note, not a failed run.
    if let Err((_, why)) = sink.collect(enc.as_mut(), 1) {
        dbglog!(
            "[pf-vd] probe: drain ended {why} with {} AU(s) owed",
            sink.inflight.len()
        );
    }
    drop(enc);
    let _ = sink.file.flush();
    dbglog!("[pf-vd] probe: AUs written to {}", path.display());
    Ok(())
}

/// The AU side of the run: matches AUs to submits in order, frees their pool slots, files
/// the bytes and keeps the reply's counters current.
struct Sink {
    shared: Arc<Shared>,
    file: std::io::BufWriter<std::fs::File>,
    /// `(slot, submit time)` of every frame whose AU is still owed.
    inflight: VecDeque<(usize, Instant)>,
    aus: u32,
    lat_sum: u64,
}

impl Sink {
    /// Poll until fewer than `keep` frames are in flight. A backend that owes an AU and
    /// produces none for 2 s is a stall.
    fn collect(&mut self, enc: &mut dyn Encoder, keep: usize) -> Result<(), Fail> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if self.inflight.len() < keep {
                return Ok(());
            }
            match enc.poll() {
                Ok(Some(au)) => self.on_au(au)?,
                Ok(None) => {
                    if Instant::now() > deadline {
                        return Err((-3, "stalled"));
                    }
                    std::thread::sleep(Duration::from_micros(200));
                }
                Err(e) => {
                    dbglog!("[pf-vd] probe: poll failed: {e:#}");
                    return Err((-1, "poll"));
                }
            }
        }
    }

    fn on_au(&mut self, au: EncodedFrame) -> Result<(), Fail> {
        let latency_us = match self.inflight.pop_front() {
            Some((slot, submitted)) => {
                if let Ring::Live { free, .. } = &mut *lock(&self.shared.ring) {
                    free.push(slot);
                }
                submitted.elapsed().as_micros() as u32
            }
            None => 0,
        };
        let len = au.data.len() as u32;
        self.file
            .write_all(&len.to_le_bytes())
            .and_then(|()| self.file.write_all(&au.pts_ns.to_le_bytes()))
            .and_then(|()| self.file.write_all(&[u8::from(au.keyframe)]))
            .and_then(|()| self.file.write_all(&au.data))
            .map_err(|_| (-6, "file"))?;
        self.aus += 1;
        let mut p = lock(&PROBE);
        p.reply.aus = self.aus;
        p.reply.bytes += u64::from(len);
        if self.aus == 1 {
            p.reply.first_au_us = latency_us;
            dbglog!(
                "[pf-vd] probe: first AU after {latency_us} us ({len} bytes, keyframe={})",
                au.keyframe
            );
        } else {
            self.lat_sum += u64::from(latency_us);
            p.reply.mean_submit_to_au_us = (self.lat_sum / u64::from(self.aus - 1)) as u32;
            p.reply.max_submit_to_au_us = p.reply.max_submit_to_au_us.max(latency_us);
        }
        Ok(())
    }
}

/// Wait for the hook to describe the first surface (10 s: DWM composes a fresh monitor at
/// arrival, but a stale ARM on a monitor nobody drains must still end).
fn wait_primed(stop: HANDLE, shared: &Shared) -> Result<Primed, Fail> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ring::Described(p) = &*lock(&shared.ring) {
            return Ok(*p);
        }
        wait(stop, shared, deadline, "prime")?;
    }
}

/// The oldest full slot; 5 s without one is a starved desktop, not a slow encoder.
fn wait_frame(stop: HANDLE, shared: &Shared) -> Result<(usize, u64), Fail> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ring::Live { full, .. } = &mut *lock(&shared.ring)
            && let Some(f) = full.pop_front()
        {
            return Ok(f);
        }
        wait(stop, shared, deadline, "starved")?;
    }
}

/// One bounded wait on `{stop, frame event}`. The event is auto-reset, so a signal raised
/// while nobody waits latches for the next call — a queue checked before each wait loses none.
fn wait(
    stop: HANDLE,
    shared: &Shared,
    deadline: Instant,
    starved: &'static str,
) -> Result<(), Fail> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err((-3, starved));
    }
    let ms = left.as_millis().min(1000) as u32;
    let handles = [stop, HANDLE(shared.event as *mut _)];
    // SAFETY: `stop` is the worker's stop event, alive until the worker joins this thread;
    // the frame event lives as long as `shared`.
    let waited = unsafe { WaitForMultipleObjects(&handles, false, ms) };
    if waited == WAIT_OBJECT_0 {
        return Err((-7, "stop"));
    }
    Ok(())
}

fn make_slot(
    dev: &Direct3DDevice,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
) -> Result<ID3D11Texture2D, Fail> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        // SRV for the planar shader pass; RT is what NVENC registers against.
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut t: Option<ID3D11Texture2D> = None;
    // SAFETY: `desc` is a fully-initialized local; `t` a valid out-param checked below.
    let hr = unsafe { dev.device.CreateTexture2D(&desc, None, Some(&mut t)) };
    match (hr, t) {
        (Ok(()), Some(t)) => Ok(t),
        (r, _) => {
            dbglog!("[pf-vd] probe: pool slot CreateTexture2D failed: {r:?}");
            Err((-2, "pool"))
        }
    }
}

/// One `windows` 0.62 texture on the bridged device (the converter targets).
fn make_tex62(
    dev: &d3d::ID3D11Device,
    w: u32,
    h: u32,
    format: dxgi::DXGI_FORMAT,
    misc: u32,
) -> Result<d3d::ID3D11Texture2D, Fail> {
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
        BindFlags: d3d::D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: misc,
    };
    let mut t: Option<d3d::ID3D11Texture2D> = None;
    // SAFETY: `desc` is a fully-initialized local; `t` a valid out-param checked below.
    let hr = unsafe { dev.CreateTexture2D(&desc, None, Some(&mut t)) };
    match (hr, t) {
        (Ok(()), Some(t)) => Ok(t),
        (r, _) => {
            dbglog!("[pf-vd] probe: target CreateTexture2D({format:?}) failed: {r:?}");
            Err((-2, "pool"))
        }
    }
}

fn rtv(
    dev: &d3d::ID3D11Device,
    t: &d3d::ID3D11Texture2D,
) -> Result<d3d::ID3D11RenderTargetView, Fail> {
    let mut v = None;
    // SAFETY: `t` is a live render-target texture on `dev`; `v` a valid out-param.
    unsafe { dev.CreateRenderTargetView(t, None, Some(&mut v)) }.map_err(|_| (-2, "rtv"))?;
    v.ok_or((-2, "rtv"))
}

fn srv(
    dev: &d3d::ID3D11Device,
    t: &d3d::ID3D11Texture2D,
) -> Result<d3d::ID3D11ShaderResourceView, Fail> {
    let mut v = None;
    // SAFETY: `t` is a live shader-resource texture on `dev`; `v` a valid out-param.
    unsafe { dev.CreateShaderResourceView(t, None, Some(&mut v)) }.map_err(|_| (-2, "srv"))?;
    v.ok_or((-2, "srv"))
}

/// The per-frame input each backend wants, built once from the pool.
enum Kind {
    /// NVENC reads the BGRA slot as-is.
    Bgra,
    /// Video-engine BGRA→NV12 into a per-slot target (AMF, QSV, the NVENC A/B).
    Nv12 {
        conv: VideoConverter,
        out: Vec<d3d::ID3D11Texture2D>,
    },
    /// BGRA→Y + CbCr shareable planes plus one shared fence, as the host feeds PyroWave.
    Planar {
        conv: BgraToYuvPlanes,
        srv: Vec<d3d::ID3D11ShaderResourceView>,
        y: Vec<(d3d::ID3D11Texture2D, d3d::ID3D11RenderTargetView)>,
        cbcr: Vec<(d3d::ID3D11Texture2D, d3d::ID3D11RenderTargetView)>,
        fence: d3d::ID3D11Fence,
        ctx4: d3d::ID3D11DeviceContext4,
        /// NT handle the encoder duplicates on its first frame; closed with this value.
        fence_handle: windows62::Win32::Foundation::HANDLE,
        fence_value: u64,
    },
}

struct Input {
    kind: Kind,
    dev: d3d::ID3D11Device,
    ctx: d3d::ID3D11DeviceContext,
    slots: Vec<d3d::ID3D11Texture2D>,
    width: u32,
    height: u32,
}

impl Input {
    fn new(
        req: &EncodeProbeRequest,
        dev: &d3d::ID3D11Device,
        ctx: &d3d::ID3D11DeviceContext,
        slots: Vec<d3d::ID3D11Texture2D>,
        w: u32,
        h: u32,
    ) -> Result<Self, Fail> {
        let convert_err = |e: anyhow::Error| {
            dbglog!("[pf-vd] probe: converter build failed: {e:#}");
            (-2, "convert")
        };
        let kind = if req.backend == 4 {
            let conv = BgraToYuvPlanes::new(dev, false, false).map_err(convert_err)?;
            let shared = (d3d::D3D11_RESOURCE_MISC_SHARED.0
                | d3d::D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32;
            let mut srvs = Vec::new();
            let mut y = Vec::new();
            let mut cbcr = Vec::new();
            for s in &slots {
                srvs.push(srv(dev, s)?);
                let yt = make_tex62(dev, w, h, dxgi::DXGI_FORMAT_R8_UNORM, shared)?;
                let ct = make_tex62(dev, w / 2, h / 2, dxgi::DXGI_FORMAT_R8G8_UNORM, shared)?;
                y.push((yt.clone(), rtv(dev, &yt)?));
                cbcr.push((ct.clone(), rtv(dev, &ct)?));
            }
            let (fence, fence_handle, ctx4) = shared_fence(dev, ctx)?;
            Kind::Planar {
                conv,
                srv: srvs,
                y,
                cbcr,
                fence,
                ctx4,
                fence_handle,
                fence_value: 0,
            }
        } else if req.backend == 1 && req.input == 0 {
            Kind::Bgra
        } else {
            let conv = VideoConverter::new(dev, ctx, w, h, false).map_err(convert_err)?;
            let out = (0..slots.len())
                .map(|_| make_tex62(dev, w, h, dxgi::DXGI_FORMAT_NV12, 0))
                .collect::<Result<Vec<_>, _>>()?;
            Kind::Nv12 { conv, out }
        };
        Ok(Self {
            kind,
            dev: dev.clone(),
            ctx: ctx.clone(),
            slots,
            width: w,
            height: h,
        })
    }

    /// Convert slot `i` into the backend's input and wrap it as the frame `submit` takes.
    fn prepare(&mut self, i: usize, pts_ns: u64) -> Result<CapturedFrame, Fail> {
        let convert_err = |e: anyhow::Error| {
            dbglog!("[pf-vd] probe: convert failed: {e:#}");
            (-2, "convert")
        };
        let (texture, format, pyro) = match &mut self.kind {
            Kind::Bgra => (self.slots[i].clone(), PixelFormat::Bgra, None),
            Kind::Nv12 { conv, out } => {
                conv.convert(&self.slots[i], &out[i]).map_err(convert_err)?;
                (out[i].clone(), PixelFormat::Nv12, None)
            }
            Kind::Planar {
                conv,
                srv,
                y,
                cbcr,
                fence,
                ctx4,
                fence_handle,
                fence_value,
            } => {
                conv.convert(
                    &self.ctx,
                    &srv[i],
                    &y[i].1,
                    &cbcr[i].1,
                    self.width,
                    self.height,
                )
                .map_err(convert_err)?;
                *fence_value += 1;
                // SAFETY: `fence` is the live shared fence on this context's device; `Flush`
                // submits the queued convert + signal so the Vulkan wait can resolve.
                unsafe {
                    ctx4.Signal(&*fence, *fence_value)
                        .map_err(|_| (-2, "fence"))?;
                    self.ctx.Flush();
                }
                let share = PyroFrameShare {
                    cbcr: cbcr[i].0.clone(),
                    fence_handle: Some(fence_handle.0 as isize),
                    fence_value: *fence_value,
                    ring_gen: 1,
                };
                (y[i].0.clone(), PixelFormat::Nv12, Some(share))
            }
        };
        Ok(CapturedFrame {
            width: self.width,
            height: self.height,
            pts_ns,
            format,
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

impl Drop for Input {
    fn drop(&mut self) {
        if let Kind::Planar { fence_handle, .. } = &self.kind {
            // SAFETY: the NT handle `shared_fence` minted; the encoder holds its own duplicate.
            unsafe {
                let _ = windows62::Win32::Foundation::CloseHandle(*fence_handle);
            }
        }
    }
}

/// A shared D3D11 fence, its NT handle, and the context that signals it (`idd_push.rs`'s
/// `pyro_fence_signal`, once).
fn shared_fence(
    dev: &d3d::ID3D11Device,
    ctx: &d3d::ID3D11DeviceContext,
) -> Result<
    (
        d3d::ID3D11Fence,
        windows62::Win32::Foundation::HANDLE,
        d3d::ID3D11DeviceContext4,
    ),
    Fail,
> {
    let fail = |what: &str, e: windows62::core::Error| {
        dbglog!("[pf-vd] probe: {what} failed: {e:?}");
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
    Ok((fence.ok_or((-2, "fence"))?, handle, ctx4))
}

fn open_backend(
    req: &EncodeProbeRequest,
    w: u32,
    h: u32,
    primed: &Primed,
) -> Result<Box<dyn Encoder>, Fail> {
    // Implicit Vulkan layers (overlays, our pf-vkhdr-layer) hang in session 0, where there is no
    // desktop to hook, and the encoder's private instance wants none of them. The loader-wide
    // knob needs a 1.3.234+ loader; each manifest's own `disable_environment` works on any.

    // SAFETY: WUDFHost is this driver's own process (`ProcessSharingDisabled`); Windows'
    // SetEnvironmentVariable is thread-safe and nothing here parses the environment concurrently.
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
    let codec = match req.codec {
        1 => Codec::H264,
        2 => Codec::H265,
        3 => Codec::Av1,
        _ => Codec::PyroWave,
    };
    let fps = if req.fps == 0 { 60 } else { req.fps };
    let bps = u64::from(if req.bitrate_kbps == 0 {
        20_000
    } else {
        req.bitrate_kbps
    }) * 1000;
    let luid = Some(windows62::Win32::Foundation::LUID {
        LowPart: primed.luid.LowPart,
        HighPart: primed.luid.HighPart,
    });
    let format = if req.backend == 1 && req.input == 0 {
        PixelFormat::Bgra
    } else {
        PixelFormat::Nv12
    };
    let chroma = ChromaFormat::Yuv420;
    let opened: anyhow::Result<Box<dyn Encoder>> = match req.backend {
        1 => pf_encode_win::nvenc::NvencD3d11Encoder::open(
            codec, format, w, h, fps, bps, 8, chroma, 1, luid,
        )
        .map(|e| Box::new(e) as Box<dyn Encoder>),
        2 => pf_encode_win::amf::AmfEncoder::open(codec, format, w, h, fps, bps, 8, chroma, luid)
            .map(|e| Box::new(e) as Box<dyn Encoder>),
        3 => pf_encode_win::qsv::QsvEncoder::open(codec, format, w, h, fps, bps, 8, chroma, luid)
            .map(|e| Box::new(e) as Box<dyn Encoder>),
        _ => pf_encode_win::pyrowave::PyroWaveEncoder::open(
            w,
            h,
            fps,
            bps,
            chroma,
            8,
            primed.vendor_id,
            primed.device_id,
        )
        .map(|e| Box::new(e) as Box<dyn Encoder>),
    };
    opened.map_err(|e| {
        dbglog!("[pf-vd] probe: backend open FAILED: {e:#}");
        (-1, "open")
    })
}

fn qpc_now() -> u64 {
    let mut qpc = 0i64;
    // SAFETY: plain FFI; `qpc` is a valid local out-param.
    let _ = unsafe { QueryPerformanceCounter(&mut qpc) };
    qpc as u64
}

fn qpc_frequency() -> u64 {
    let mut hz = 0i64;
    // SAFETY: plain FFI; `hz` is a valid local out-param.
    let _ = unsafe { QueryPerformanceFrequency(&mut hz) };
    (hz as u64).max(1)
}

fn qpc_to_ns(qpc: u64, hz: u64) -> u64 {
    (u128::from(qpc) * 1_000_000_000 / u128::from(hz)) as u64
}
