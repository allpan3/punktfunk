//! **Media Foundation** hardware encoder (Windows, D3D11 NV12 input). The vendor-agnostic
//! rung between the native SDKs ([`super::nvenc`]/[`super::amf`]/[`super::qsv`]) and
//! software: every x64 vendor ships an H.264/HEVC MFT, so a missing `amfrt64.dll` no
//! longer ends the session — and on Adreno it is the only hardware encode path there is.
//!
//! Drives an **async hardware MFT** (`MFTEnum2`, filtered to the capture adapter's LUID)
//! through its event generator, pumped non-blocking (`MF_EVENT_FLAG_NO_WAIT`) from
//! `submit`/`poll`. No second thread: the driver's encode thread stays the only one
//! touching the MFT, and a wedged MFT costs a bounded wait, not a join.
//!
//! Input is a same-device NV12 texture ring — `CopySubresourceRegion` then
//! `MFCreateDXGISurfaceBuffer`. 8-bit 4:2:0 only: no P010, no 4:4:4, and no AV1 (every
//! field report on the Qualcomm AV1 MFT is a defect). `mfplat.dll` is an OS component,
//! so this backend carries **no cargo feature** — it is in every Windows build.
//! Evidence: `design/media-foundation-encoder.md`.

use super::{ChromaFormat, Codec, EncodedFrame, Encoder, EncoderCaps};
use anyhow::{anyhow, bail, Context, Result};
use pf_frame::{CapturedFrame, FramePayload, PixelFormat};
use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::ptr;
use std::time::{Duration, Instant};
use windows::core::{Interface, GUID};
use windows::Win32::Foundation::{LUID, S_OK};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{
    eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_High, eAVScenarioInfo_DisplayRemoting,
    CODECAPI_AVEncCommonBufferSize, CODECAPI_AVEncCommonMaxBitRate,
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncH264CABACEnable, CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, CODECAPI_AVScenarioInfo,
    ICodecAPI, IMF2DBuffer, IMFActivate, IMFAttributes, IMFDXGIDeviceManager,
    IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFShutdown, IMFTransform,
    METransformHaveOutput, METransformNeedInput, MFCreateAttributes, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateSample, MFMediaType_Video,
    MFSampleExtension_CleanPoint, MFStartup, MFTEnum2, MFT_FRIENDLY_NAME_Attribute,
    MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
    MFSTARTUP_LITE, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT,
    MF_E_NO_EVENTS_AVAILABLE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MF_SA_D3D11_AWARE, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VARIANT;

/// Input texture ring depth. The MFT holds a ref on the sample (and so the slot) until it
/// has consumed it; `submit` back-pressures at [`IN_FLIGHT_MAX`], well under this, so a
/// slot is never rewritten while the encoder still reads it.
const RING: usize = 6;

/// Frames the MFT may hold before `submit` blocks. `AVLowLatencyMode` promises one-in
/// one-out, so steady state is 1; this only bounds a stall.
const IN_FLIGHT_MAX: usize = 4;

/// Drain budget for a starved event pump. One frame's encode time over, far under the
/// session watchdog's ~2 s floor.
const BUSY_BUDGET: Duration = Duration::from_millis(200);

/// 100-ns ticks per second — MF's sample-time unit.
const HNS_PER_SEC: i64 = 10_000_000;

/// Output subtype. AV1 is refused: every field report on the Qualcomm AV1 MFT is a defect
/// (dropped frames, blockiness at low CBR), and no x64 vendor needs MF for AV1.
fn subtype(codec: Codec) -> Result<GUID> {
    match codec {
        Codec::H264 => Ok(MFVideoFormat_H264),
        Codec::H265 => Ok(MFVideoFormat_HEVC),
        Codec::Av1 => bail!(
            "Media Foundation AV1 is not enabled — the only hardware AV1 MFT in the field \
             (Qualcomm) is reported broken; use a native SDK backend for AV1"
        ),
        Codec::PyroWave => bail!("PyroWave never opens the Media Foundation backend"),
    }
}

/// Process-wide `MFStartup`. Never shut down: MF is refcounted per process and another
/// session's encoder may still hold it, so the teardown would be the bug, not the leak.
fn mf_startup() -> Result<()> {
    static ONCE: std::sync::OnceLock<std::result::Result<(), String>> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: plain FFI with the version constant the headers define; `MFSTARTUP_LITE`
        // skips the socket/network stack this process never uses.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE).map_err(|e| format!("MFStartup: {e}")) }
    })
    .clone()
    .map_err(|e| anyhow!(e))
}

/// Join the MTA on this thread. A hardware MFT must be created in a multi-threaded
/// apartment; `RPC_E_CHANGED_MODE` means the thread already picked one and is not ours to
/// change. Never paired with `CoUninitialize` — the encode thread outlives the encoder.
fn com_init_mta() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    DONE.with(|d| {
        if !d.replace(true) {
            // SAFETY: plain FFI. The returned HRESULT is deliberately dropped: a thread
            // already in an apartment keeps it, and the MFT open below reports the real
            // consequence if that apartment is the wrong one.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        }
    });
}

/// Hardware video-encoder MFTs on `adapter_luid` for `codec`, best first. Empty is the
/// honest answer for "this adapter has no MFT" — only a broken enumeration is an `Err`.
fn enumerate(codec: Codec, adapter_luid: Option<LUID>) -> Result<Vec<IMFActivate>> {
    let out_subtype = subtype(codec)?;
    mf_startup()?;
    com_init_mta();
    // SAFETY: every handle below is owned by this function. `MFTEnum2` writes a
    // CoTaskMemAlloc'd array of AddRef'd activates; each element is read out (taking that
    // reference) exactly once and the array is freed before return, on both paths.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1).context("MFCreateAttributes")?;
        let attrs = attrs.ok_or_else(|| anyhow!("MFCreateAttributes returned no attributes"))?;
        if let Some(luid) = adapter_luid {
            let mut bytes = [0u8; 8];
            bytes[..4].copy_from_slice(&luid.LowPart.to_le_bytes());
            bytes[4..].copy_from_slice(&luid.HighPart.to_le_bytes());
            attrs
                .SetBlob(&MFT_ENUM_ADAPTER_LUID, &bytes)
                .context("MFT_ENUM_ADAPTER_LUID")?;
        }
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: out_subtype,
        };
        let mut array: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count = 0u32;
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &attrs,
            &mut array,
            &mut count,
        )
        .context("MFTEnum2")?;
        let mut found = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            if let Some(a) = ptr::read(array.add(i)) {
                found.push(a);
            }
        }
        CoTaskMemFree(Some(array as *const std::ffi::c_void));
        Ok(found)
    }
}

/// `MFT_FRIENDLY_NAME_Attribute`, for the one line that says which vendor's MFT opened.
fn friendly_name(activate: &IMFActivate) -> String {
    // SAFETY: standard MF attribute read on a live activate; the buffer is sized from the
    // length the same object just reported and only the written prefix is decoded.
    unsafe {
        let Ok(len) = activate.GetStringLength(&MFT_FRIENDLY_NAME_Attribute) else {
            return "unknown MFT".into();
        };
        let mut buf = vec![0u16; len as usize + 1];
        match activate.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, None) {
            Ok(()) => String::from_utf16_lossy(&buf[..len as usize]),
            Err(_) => "unknown MFT".into(),
        }
    }
}

/// Everything one bring-up needs from the encoder's negotiated parameters.
#[derive(Clone, Copy)]
struct EncodeConfig {
    codec: Codec,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_bps: u64,
}

impl EncodeConfig {
    /// `MF_MT_AVG_BITRATE` and the codec-API rate properties are `u32` bits/s.
    fn bitrate_u32(&self) -> u32 {
        self.bitrate_bps.min(u64::from(u32::MAX)) as u32
    }
}

/// `ICodecAPI::SetValue`, advisory. An MFT that declines a property still encodes — every
/// vendor's optional set differs — so a failure is logged, never fatal. `value` must carry the
/// VARIANT type the property documents, and an integer literal is `i32` (`VT_I4`) where nearly
/// every codec property wants `VT_UI4` — write `1u32`. NVIDIA's MFT refuses a mistyped
/// B-picture count outright and ignores a mistyped force-keyframe silently.
fn set_advisory<T>(api: &ICodecAPI, key: &GUID, name: &str, value: T) -> bool
where
    T: Into<VARIANT> + Copy + std::fmt::Debug,
{
    let v = value.into();
    // SAFETY: `api` is live, `key` is a static GUID, and `v` is a by-value scalar VARIANT
    // that outlives the synchronous call (no allocation to release).
    let r = unsafe { api.SetValue(key, &v) };
    if let Err(e) = &r {
        tracing::debug!(property = name, value = ?value, error = %e, "MF encoder declined a codec-API property");
    }
    r.is_ok()
}

/// `ICodecAPI::IsSupported`, strictly. `S_FALSE` names a property the MFT knows and will not
/// honour, and windows-rs folds that into `Ok`; Microsoft's contract is to test for `S_OK`.
fn is_supported(api: &ICodecAPI, key: &GUID) -> bool {
    // SAFETY: `api` is live and `key` is a static GUID. The raw vtable call is the only way
    // to see `S_FALSE` — the wrapper's `Result` has already discarded it.
    let hr = unsafe { (Interface::vtable(api).IsSupported)(Interface::as_raw(api), key) };
    hr == S_OK
}

/// Both rate-control knobs in one place: the open path and `reconfigure_bitrate` must not
/// drift. `false` = the MFT refused the mean rate, so the caller rebuilds instead.
fn apply_bitrate(api: &ICodecAPI, cfg: &EncodeConfig) -> bool {
    let bps = cfg.bitrate_u32();
    let mean = set_advisory(api, &CODECAPI_AVEncCommonMeanBitRate, "MeanBitRate", bps);
    set_advisory(api, &CODECAPI_AVEncCommonMaxBitRate, "MaxBitRate", bps);
    // One frame of VBV: the tree's low-latency contract, same as the native backends.
    let vbv = (cfg.bitrate_bps / u64::from(cfg.fps.max(1))).min(u64::from(u32::MAX)) as u32;
    set_advisory(api, &CODECAPI_AVEncCommonBufferSize, "BufferSize", vbv);
    mean
}

/// The static property block, set before the media types (MS documents rate-control mode
/// and low latency as pre-type). Colour attributes are deliberately absent: no MFT writes
/// VUI from them and setting them crashes AMD's (Chromium `4c2d1fdf61`).
fn apply_static_properties(api: &ICodecAPI, cfg: &EncodeConfig, force_idr_ok: bool) {
    set_advisory(
        api,
        &CODECAPI_AVEncCommonRateControlMode,
        "RateControlMode",
        eAVEncCommonRateControlMode_CBR.0 as u32,
    );
    // VARIANT_BOOL on encoders — only the H.264 *decoder* takes VT_UI4 for low latency.
    // An MFT that checks the type declines a mistyped one and stays deeply pipelined.
    set_advisory(api, &CODECAPI_AVLowLatencyMode, "LowLatencyMode", true);
    // B-frames break FIFO pairing and add a frame of latency. Chromium forces 0 on
    // Qualcomm, where the MFT default was 1 and buggy.
    set_advisory(
        api,
        &CODECAPI_AVEncMPVDefaultBPictureCount,
        "BPictureCount",
        0u32,
    );
    set_advisory(
        api,
        &CODECAPI_AVScenarioInfo,
        "ScenarioInfo",
        eAVScenarioInfo_DisplayRemoting.0 as u32,
    );
    if cfg.codec == Codec::H264 {
        set_advisory(api, &CODECAPI_AVEncH264CABACEnable, "CABAC", true);
    }
    // Infinite GOP is the tree's contract: IDR on demand only. An MFT that cannot force an
    // IDR gets a periodic one instead, or a lost frame freezes the client forever.
    if force_idr_ok {
        if !set_advisory(api, &CODECAPI_AVEncMPVGOPSize, "GOPSize", u32::MAX) {
            set_advisory(
                api,
                &CODECAPI_AVEncMPVGOPSize,
                "GOPSize",
                cfg.fps.saturating_mul(60).max(60),
            );
        }
    } else {
        set_advisory(
            api,
            &CODECAPI_AVEncMPVGOPSize,
            "GOPSize",
            cfg.fps.saturating_mul(2).max(30),
        );
    }
}

/// `MF_MT_FRAME_SIZE` / `MF_MT_FRAME_RATE` / `MF_MT_PIXEL_ASPECT_RATIO` pack two `u32`s
/// into one `u64`, high word first. `MFSetAttributeSize` is a C++ inline, not an export.
const fn pack2(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

/// Output (bitstream) media type. Set before the input type — the MFT derives what input
/// it will accept from it.
fn output_type(cfg: &EncodeConfig) -> Result<IMFMediaType> {
    // SAFETY: `MFCreateMediaType` hands back an owned empty type; every setter below is a
    // plain attribute write on it with static GUID keys.
    unsafe {
        let t = MFCreateMediaType().context("MFCreateMediaType (output)")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype(cfg.codec)?)?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, cfg.bitrate_u32())?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack2(cfg.width, cfg.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack2(cfg.fps.max(1), 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack2(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        if cfg.codec == Codec::H264 {
            t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
        }
        Ok(t)
    }
}

/// Input (NV12) media type. 8-bit 4:2:0 is the whole input surface of this backend.
fn input_type(cfg: &EncodeConfig) -> Result<IMFMediaType> {
    // SAFETY: as `output_type` — owned empty media type, plain attribute writes.
    unsafe {
        let t = MFCreateMediaType().context("MFCreateMediaType (input)")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack2(cfg.width, cfg.height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack2(cfg.fps.max(1), 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack2(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Ok(t)
    }
}

/// H.264 SPS/PPS or HEVC VPS/SPS/PPS.
fn is_parameter_set(codec: Codec, nal: u8) -> bool {
    match codec {
        Codec::H264 => matches!(nal, 7 | 8),
        _ => matches!(nal, 32..=34),
    }
}

/// The AU's VPS/SPS/PPS run, or `None` when it carries none. An access-unit delimiter or
/// SEI ahead of the run is skipped rather than treated as its absence — that is the shape
/// Qualcomm's MFT emits, and it publishes the sequence header late and HEVC in-band only,
/// so an IDR without one is undecodable unless we prepend the cached run ourselves.
fn parameter_set_prefix(codec: Codec, au: &[u8]) -> Option<&[u8]> {
    let mut run: Option<usize> = None;
    let mut i = 2;
    while i + 1 < au.len() {
        if au[i - 2..i + 1] != [0, 0, 1] {
            i += 1;
            continue;
        }
        // `i` indexes the start code's last byte; the 4-byte form owns the zero before it.
        let header = i + 1;
        let code_start = if i >= 3 && au[i - 3] == 0 {
            i - 3
        } else {
            i - 2
        };
        let nal = match codec {
            Codec::H264 => au[header] & 0x1f,
            _ => (au[header] >> 1) & 0x3f,
        };
        match (is_parameter_set(codec, nal), run) {
            (true, None) => run = Some(code_start),
            (false, Some(start)) => return Some(&au[start..code_start]),
            _ => {}
        }
        i = header + 1;
    }
    run.map(|start| &au[start..])
}

/// One submitted frame, awaiting its AU. The MFT emits in submit order (B-frames are off),
/// so this pairs by position — MF puts no wire index on the output sample.
struct PendingMeta {
    pts_ns: u64,
}

/// Live MFT session. Field order is drop order: the transform releases before the device
/// manager and the ring textures it was reading.
struct Inner {
    mft: IMFTransform,
    /// The activation object that created `mft`: it owns the shutdown, so it outlives it.
    activate: IMFActivate,
    events: IMFMediaEventGenerator,
    /// `None` when the MFT exposes no `ICodecAPI` — then bitrate and GOP are whatever the
    /// output media type carried, and `reconfigure_bitrate` declines.
    codec_api: Option<ICodecAPI>,
    /// Held for the MFT's D3D binding; the transform AddRefs it, this keeps our own claim.
    _manager: Option<IMFDXGIDeviceManager>,
    _device: ID3D11Device,
    dctx: ID3D11DeviceContext,
    ring: Vec<ID3D11Texture2D>,
    next: usize,
    /// `METransformNeedInput` events not yet spent on a `ProcessInput`.
    need_input: u32,
    pending: VecDeque<PendingMeta>,
    ready: VecDeque<EncodedFrame>,
    /// Failure of the post-`ProcessInput` pump. That pump consumed the MFT's event, so nothing
    /// re-raises it; the next `poll` returns it and the reset ladder takes over.
    pump_err: Option<anyhow::Error>,
    /// VPS/SPS/PPS from the first IDR that carried them, prepended to any later IDR that
    /// does not. Empty until such an IDR is seen.
    param_sets: Vec<u8>,
    headers_warned: bool,
    frames_submitted: u64,
    first_au_logged: bool,
}

impl Drop for Inner {
    /// An async MFT must be shut down before its last release, and an activation object
    /// shuts down what it created. Releasing without either leaks the vendor MFT's worker
    /// threads and GPU allocations for the life of the driver process.
    fn drop(&mut self) {
        // SAFETY: teardown runs on the encode thread that drove the MFT. Each call is
        // synchronous and takes no arguments, and this is the documented last use of both.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(shutdown) = self.mft.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
            let _ = self.activate.ShutdownObject();
        }
    }
}

impl Inner {
    fn note_first_au(&mut self, au: &EncodedFrame) {
        if !self.first_au_logged {
            self.first_au_logged = true;
            tracing::info!(
                bytes = au.data.len(),
                keyframe = au.keyframe,
                "Media Foundation produced its first AU on this session"
            );
        }
    }
}

/// Drain one `METransformHaveOutput`: `ProcessOutput`, copy the bitstream out, pair it with
/// the oldest submitted frame.
fn process_output(inner: &mut Inner, codec: Codec) -> Result<EncodedFrame> {
    // The MFT has handed this frame over, so its entry is spent whatever the payload turns
    // out to be — a failed ProcessOutput included. Popping only on the success path would pair
    // every later AU with the wrong frame's timestamp for the rest of the session.
    let meta = inner.pending.pop_front();
    // SAFETY: the MFT is live on this thread and owes exactly one output per HaveOutput
    // event. `MFT_OUTPUT_DATA_BUFFER`'s `ManuallyDrop` members are reclaimed with
    // `ManuallyDrop::take` on every path, so the sample and the event collection the MFT
    // allocated are released exactly once.
    let sample = unsafe {
        let mut out = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(None),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let call = inner.mft.ProcessOutput(0, &mut out, &mut status);
        let sample: Option<IMFSample> = ManuallyDrop::take(&mut out[0].pSample);
        let _events = ManuallyDrop::take(&mut out[0].pEvents);
        call.context("IMFTransform::ProcessOutput")?;
        sample.ok_or_else(|| anyhow!("MFT signalled HaveOutput with no sample"))?
    };
    // SAFETY: `sample` is owned here. The `Lock`ed pointer is read only for the length the
    // same call reported, and the buffer is unlocked before it drops.
    let (data, keyframe) = unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .context("IMFSample::ConvertToContiguousBuffer")?;
        let mut p: *mut u8 = ptr::null_mut();
        let mut len = 0u32;
        buffer
            .Lock(&mut p, None, Some(&mut len))
            .context("IMFMediaBuffer::Lock")?;
        let data = if p.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(p, len as usize).to_vec()
        };
        let _ = buffer.Unlock();
        let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
        (data, keyframe)
    };
    if data.is_empty() {
        bail!("Media Foundation returned an empty access unit");
    }
    let data = if keyframe {
        repeat_parameter_sets(inner, codec, data)
    } else {
        data
    };
    Ok(EncodedFrame {
        data,
        pts_ns: meta.map_or(0, |m| m.pts_ns),
        keyframe,
        recovery_anchor: false,
        chunk_aligned: false,
    })
}

/// Cache the first IDR's parameter-set run and re-attach it to any later IDR that arrives
/// without one. A no-op on every MFT that already repeats them (all three x64 vendors).
fn repeat_parameter_sets(inner: &mut Inner, codec: Codec, au: Vec<u8>) -> Vec<u8> {
    if let Some(prefix) = parameter_set_prefix(codec, &au) {
        if inner.param_sets != prefix {
            inner.param_sets = prefix.to_vec();
        }
        return au;
    }
    if inner.param_sets.is_empty() {
        return au;
    }
    if !inner.headers_warned {
        inner.headers_warned = true;
        tracing::warn!(
            "this MFT does not repeat VPS/SPS/PPS on every IDR — prepending the cached \
             sequence header so a client that joins late can decode"
        );
    }
    let mut out = Vec::with_capacity(inner.param_sets.len() + au.len());
    out.extend_from_slice(&inner.param_sets);
    out.extend_from_slice(&au);
    out
}

/// Drain every queued MFT event. Non-blocking: `MF_E_NO_EVENTS_AVAILABLE` is the exit.
fn pump(inner: &mut Inner, codec: Codec) -> Result<()> {
    loop {
        // SAFETY: `events` is the live MFT's own generator on this thread; the NO_WAIT
        // flag makes this a poll, and the returned event is an owned interface.
        let event = unsafe { inner.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) };
        let event = match event {
            Ok(e) => e,
            Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
            Err(e) => bail!("IMFMediaEventGenerator::GetEvent: {e}"),
        };
        // SAFETY: plain accessor on the owned event.
        let kind = unsafe { event.GetType() }.unwrap_or(0) as i32;
        if kind == METransformNeedInput.0 {
            inner.need_input += 1;
        } else if kind == METransformHaveOutput.0 {
            let au = process_output(inner, codec)?;
            inner.ready.push_back(au);
        }
        // Drain-complete and format-change events need no action: the drain is observed
        // through `pending` emptying, and the output type is fixed for the session.
    }
}

/// Pump until `ready` holds or the budget expires. The one wait in this backend — both
/// back-pressure and input credit go through it, so neither can grow its own timeout.
fn wait_until(
    inner: &mut Inner,
    codec: Codec,
    what: &str,
    ready: impl Fn(&Inner) -> bool,
) -> Result<()> {
    let deadline = Instant::now() + BUSY_BUDGET;
    loop {
        pump(inner, codec)?;
        if ready(inner) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "Media Foundation {what} stalled for {} ms with {} frame(s) in flight — wedged \
                 (escalating to reset)",
                BUSY_BUDGET.as_millis(),
                inner.pending.len()
            );
        }
        std::thread::sleep(Duration::from_micros(250));
    }
}

pub struct MfEncoder {
    codec: Codec,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_bps: u64,
    /// Adapter the host picked. The session still binds to the first frame's device.
    adapter_luid: Option<LUID>,
    /// Lazy from the first frame's device; rebuilt on a capturer-device change.
    inner: Option<Inner>,
    bound_device: isize,
    force_kf: bool,
    /// The MFT answered `IsSupported(AVEncVideoForceKeyFrame)`. `false` buys a periodic
    /// GOP instead, because a stream with neither cannot recover from loss at all.
    force_idr_ok: bool,
    /// Resets with no AU since. At 2, drop `inner` instead of re-messaging a dead MFT.
    resets_without_output: u32,
}

// SAFETY: COM interfaces and D3D11 handles are not auto-`Send`. The session moves the
// encoder onto one encode thread and drives it there; the immediate context, the MFT, and
// its event generator are never touched from another thread.
unsafe impl Send for MfEncoder {}

impl MfEncoder {
    /// Open the MF backend. Fails when the adapter has no hardware MFT for `codec`, or
    /// when capture is not 8-bit NV12.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        codec: Codec,
        format: PixelFormat,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_bps: u64,
        bit_depth: u8,
        chroma: ChromaFormat,
        adapter_luid: Option<LUID>,
    ) -> Result<Self> {
        subtype(codec)?;
        // Depth follows delivered pixels, not negotiated depth ([`crate::ten_bit_input`]),
        // so a 10-bit-negotiated session over 8-bit capture encodes 8-bit rather than bailing.
        if crate::ten_bit_input(format, bit_depth) {
            bail!(
                "Media Foundation encode is 8-bit 4:2:0 only — no vendor's MFT accepts P010 \
                 (capturer delivered {format:?})"
            );
        }
        if format != PixelFormat::Nv12 {
            bail!(
                "Media Foundation needs the video-processor NV12 capture path; capturer \
                 delivered {format:?} (no readback path by design — zero-copy invariant)"
            );
        }
        if chroma.is_444() {
            tracing::warn!("no Media Foundation MFT encodes 4:4:4 — encoding 4:2:0");
        }
        if enumerate(codec, adapter_luid)?.is_empty() {
            bail!("no hardware Media Foundation encoder for {codec:?} on the selected adapter");
        }
        Ok(MfEncoder {
            codec,
            width,
            height,
            fps,
            bitrate_bps,
            adapter_luid,
            inner: None,
            bound_device: 0,
            force_kf: false,
            force_idr_ok: false,
            resets_without_output: 0,
        })
    }

    fn encode_config(&self) -> EncodeConfig {
        EncodeConfig {
            codec: self.codec,
            width: self.width,
            height: self.height,
            fps: self.fps,
            bitrate_bps: self.bitrate_bps,
        }
    }

    /// Bring the MFT up on the capturer's device: unlock async, bind D3D, negotiate types,
    /// apply the property block, start streaming.
    fn ensure_inner(&mut self, device: &ID3D11Device) -> Result<()> {
        let dev_raw = device.as_raw() as isize;
        if self.inner.is_some() && self.bound_device == dev_raw {
            return Ok(());
        }
        self.inner = None;
        self.bound_device = dev_raw;
        let cfg = self.encode_config();
        let activate = enumerate(self.codec, self.adapter_luid)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                anyhow!(
                    "no hardware Media Foundation encoder for {:?} on the selected adapter",
                    self.codec
                )
            })?;
        let name = friendly_name(&activate);
        let brought = || -> Result<_> {
            // SAFETY: the whole bring-up runs on this encode thread. Every interface is an
            // owned windows-rs wrapper released on drop; `device.as_raw()` and the device
            // manager's raw pointer are borrowed for the duration of the synchronous calls
            // that consume them (the MFT AddRefs the manager it is handed), and the ring
            // textures are created on and used from this one device.
            unsafe {
                let mft: IMFTransform = activate
                    .ActivateObject()
                    .context("IMFActivate::ActivateObject(IMFTransform)")?;
                let attrs = mft.GetAttributes().context("IMFTransform::GetAttributes")?;
                if attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 1 {
                    bail!("{name} is not an async MFT — this backend drives the event model only");
                }
                attrs
                    .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                    .context("MF_TRANSFORM_ASYNC_UNLOCK")?;
                let dctx = device
                    .GetImmediateContext()
                    .context("ID3D11Device immediate context")?;
                // MFT worker threads touch the device: protection must be on before the
                // manager reset, or the reset takes a device nobody else may use (QSV's
                // on-glass lesson, `qsv.rs`).
                if let Ok(mt) = dctx.cast::<ID3D11Multithread>() {
                    let _ = mt.SetMultithreadProtected(true);
                }
                let manager = if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0 {
                    let mut token = 0u32;
                    let mut mgr: Option<IMFDXGIDeviceManager> = None;
                    MFCreateDXGIDeviceManager(&mut token, &mut mgr)
                        .context("MFCreateDXGIDeviceManager")?;
                    let mgr =
                        mgr.ok_or_else(|| anyhow!("MFCreateDXGIDeviceManager returned none"))?;
                    mgr.ResetDevice(device, token)
                        .context("IMFDXGIDeviceManager::ResetDevice")?;
                    mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr.as_raw() as usize)
                        .context("MFT_MESSAGE_SET_D3D_MANAGER")?;
                    Some(mgr)
                } else {
                    // Without D3D awareness the MFT would want system-memory input, which the
                    // zero-copy contract has no path to produce.
                    bail!("{name} is not MF_SA_D3D11_AWARE — no D3D11 texture input path");
                };
                let codec_api: Option<ICodecAPI> = mft.cast().ok();
                let force_idr_ok = codec_api
                    .as_ref()
                    .is_some_and(|api| is_supported(api, &CODECAPI_AVEncVideoForceKeyFrame));
                if let Some(api) = codec_api.as_ref() {
                    apply_static_properties(api, &cfg, force_idr_ok);
                }
                mft.SetOutputType(0, &output_type(&cfg)?, 0)
                    .context("IMFTransform::SetOutputType")?;
                mft.SetInputType(0, &input_type(&cfg)?, 0)
                    .context("IMFTransform::SetInputType")?;
                // `process_output` always asks the MFT for its own sample. One that wants the
                // caller to allocate would fail every ProcessOutput instead, so refuse here and
                // let the driver's preference list move on to the next backend.
                let out_info = mft
                    .GetOutputStreamInfo(0)
                    .context("IMFTransform::GetOutputStreamInfo")?;
                let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
                    | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                    as u32;
                if out_info.dwFlags & provides == 0 {
                    bail!("{name} wants the caller to allocate output samples — unsupported here");
                }
                if let Some(api) = codec_api.as_ref() {
                    // Rate is dynamic: re-apply after the types so the MFT's own derivation
                    // from MF_MT_AVG_BITRATE cannot win.
                    apply_bitrate(api, &cfg);
                }
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: self.width,
                    Height: self.height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut ring = Vec::with_capacity(RING);
                for _ in 0..RING {
                    let mut t: Option<ID3D11Texture2D> = None;
                    device
                        .CreateTexture2D(&desc, None, Some(&mut t))
                        .context("CreateTexture2D (MF input ring)")?;
                    ring.push(t.context("MF input ring texture")?);
                }
                let events: IMFMediaEventGenerator = mft
                    .cast()
                    .context("MFT exposes no IMFMediaEventGenerator")?;
                mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .context("MFT_MESSAGE_NOTIFY_BEGIN_STREAMING")?;
                mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .context("MFT_MESSAGE_NOTIFY_START_OF_STREAM")?;
                Ok((mft, events, codec_api, manager, dctx, ring, force_idr_ok))
            }
        };
        let (mft, events, codec_api, manager, dctx, ring, force_idr_ok) = match brought() {
            Ok(t) => t,
            Err(e) => {
                // An activated MFT released without its shutdown leaks the vendor's worker
                // threads and GPU allocations for the process's life (see `Inner::drop`).
                // SAFETY: the activation object is live; this is its last use on this path.
                unsafe {
                    let _ = activate.ShutdownObject();
                }
                return Err(e);
            }
        };
        self.force_idr_ok = force_idr_ok;
        if !force_idr_ok {
            tracing::warn!(
                mft = %name,
                "this MFT declines on-demand IDR (AVEncVideoForceKeyFrame) — falling back to a \
                 periodic GOP, so loss recovery waits for the next scheduled keyframe"
            );
        }
        tracing::info!(
            mft = %name,
            codec = ?self.codec,
            width = self.width,
            height = self.height,
            fps = self.fps,
            force_idr = force_idr_ok,
            device = %format_args!("{:#x}", dev_raw as usize),
            "Media Foundation encode active (async MFT, zero-copy D3D11 NV12)"
        );
        self.inner = Some(Inner {
            mft,
            activate,
            events,
            codec_api,
            _manager: manager,
            _device: device.clone(),
            dctx,
            ring,
            next: 0,
            need_input: 0,
            pending: VecDeque::new(),
            ready: VecDeque::new(),
            pump_err: None,
            param_sets: Vec::new(),
            headers_warned: false,
            frames_submitted: 0,
            first_au_logged: false,
        });
        Ok(())
    }
}

impl Encoder for MfEncoder {
    fn submit(&mut self, captured: &CapturedFrame) -> Result<()> {
        anyhow::ensure!(
            captured.width == self.width && captured.height == self.height,
            "captured frame {}x{} != encoder {}x{}",
            captured.width,
            captured.height,
            self.width,
            self.height
        );
        let frame = match &captured.payload {
            FramePayload::D3d11(f) => f,
            FramePayload::Cpu(_) => {
                bail!("Media Foundation is D3D11-only; got a CPU frame (video processor lost?)")
            }
        };
        anyhow::ensure!(
            captured.format == PixelFormat::Nv12,
            "captured format {:?} != NV12 (capturer video-processor fallback mid-session — this \
             backend has no readback path)",
            captured.format
        );
        self.ensure_inner(&frame.device)?;
        let opening = self.inner.as_ref().is_none_or(|i| i.frames_submitted == 0);
        let forced = std::mem::take(&mut self.force_kf) || opening;
        let codec = self.codec;
        let fps = self.fps.max(1);
        let inner = self.inner.as_mut().expect("ensure_inner succeeded");
        // Back-pressure before input credit: an AU drained here frees the ring slot below.
        if inner.pending.len() >= IN_FLIGHT_MAX {
            wait_until(inner, codec, "output", |i| i.pending.len() < IN_FLIGHT_MAX)
                .inspect_err(|_| self.force_kf = true)?;
        }
        if inner.need_input == 0 {
            wait_until(inner, codec, "input credit", |i| i.need_input > 0)
                .inspect_err(|_| self.force_kf = true)?;
        }
        let slot = inner.next;
        inner.next = (inner.next + 1) % RING;
        // SAFETY: single encode thread against the live MFT. The ring texture is owned
        // here and only rewritten after back-pressure released it; the DXGI surface buffer
        // and sample are owned wrappers the MFT AddRefs for as long as it reads them, and
        // `ProcessInput` is only reached with a `METransformNeedInput` credit in hand.
        let submitted = unsafe {
            // `&ID3D11Texture2D` is already an `ID3D11Resource` param (windows-rs `CanInto`,
            // no QueryInterface), so the copy costs no COM round-trip per frame.
            inner.dctx.CopySubresourceRegion(
                &inner.ring[slot],
                0,
                0,
                0,
                0,
                &frame.texture,
                0,
                None,
            );
            let buffer =
                MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &inner.ring[slot], 0, false)
                    .context("MFCreateDXGISurfaceBuffer")?;
            // The DXGI buffer opens with length 0; MFTs that read it as a byte count
            // otherwise see an empty frame.
            if let Ok(len) = buffer
                .cast::<IMF2DBuffer>()
                .and_then(|b| b.GetContiguousLength())
            {
                let _ = buffer.SetCurrentLength(len);
            }
            let sample = MFCreateSample().context("MFCreateSample")?;
            sample.AddBuffer(&buffer).context("IMFSample::AddBuffer")?;
            sample.SetSampleTime((captured.pts_ns / 100) as i64)?;
            sample.SetSampleDuration(HNS_PER_SEC / i64::from(fps))?;
            if forced {
                if let Some(api) = inner.codec_api.as_ref() {
                    set_advisory(
                        api,
                        &CODECAPI_AVEncVideoForceKeyFrame,
                        "ForceKeyFrame",
                        1u32,
                    );
                }
            }
            inner.mft.ProcessInput(0, &sample, 0)
        };
        if let Err(e) = submitted {
            self.force_kf = true;
            bail!("IMFTransform::ProcessInput: {e}");
        }
        inner.need_input -= 1;
        inner.frames_submitted += 1;
        inner.pending.push_back(PendingMeta {
            pts_ns: captured.pts_ns,
        });
        // Collect whatever the MFT already finished; `poll` then has no wait to do. The MFT
        // owns the frame from here, so a failure must not report the submit as failed: the
        // driver would drop its in-flight entry and mis-stamp every later AU. The pump ate
        // the event that carried the failure, so stash it — `poll` is where it surfaces.
        if let Err(e) = pump(inner, codec) {
            tracing::debug!(error = %e, "MF event pump failed after the frame was accepted");
            inner.pump_err.get_or_insert(e);
        }
        Ok(())
    }

    fn request_keyframe(&mut self) {
        self.force_kf = true;
    }

    fn caps(&self) -> EncoderCaps {
        EncoderCaps {
            // The capturer composites; this backend never reads `frame.cursor`.
            blends_cursor: false,
            // No open-source client uses the LTR ICodecAPI set on any vendor's MFT, so the
            // slot planner has nothing to drive. Loss recovery is IDR.
            supports_rfi: false,
            chroma_444: false,
            intra_refresh: false,
            intra_refresh_recovery: false,
            intra_refresh_period: 0,
        }
    }

    /// Wait up to `min(3/4 frame interval, 12 ms)` for the oldest AU. Expiry is `Ok(None)`.
    /// A pump failure `submit` swallowed is raised here first — its event is already spent.
    fn poll(&mut self) -> Result<Option<EncodedFrame>> {
        let codec = self.codec;
        let budget = Duration::from_millis(u64::from((750 / self.fps.max(1)).clamp(1, 12)));
        let au = {
            let Some(inner) = self.inner.as_mut() else {
                return Ok(None);
            };
            if let Some(e) = inner.pump_err.take() {
                return Err(e);
            }
            let deadline = Instant::now() + budget;
            loop {
                pump(inner, codec)?;
                if let Some(au) = inner.ready.pop_front() {
                    inner.note_first_au(&au);
                    break Some(au);
                }
                if inner.pending.is_empty() || Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_micros(250));
            }
        };
        if au.is_some() {
            self.resets_without_output = 0;
        }
        Ok(au)
    }

    /// Stall recovery: flush and restart streaming in place. A second reset with no AU
    /// since drops the MFT so the next submit activates a fresh one.
    fn reset(&mut self) -> bool {
        self.force_kf = true;
        self.resets_without_output = self.resets_without_output.saturating_add(1);
        let Some(inner) = self.inner.as_mut() else {
            return true;
        };
        if self.resets_without_output >= 2 {
            tracing::warn!(
                resets = self.resets_without_output,
                "Media Foundation stall persisted across an in-place restart — dropping the MFT, \
                 reopening lazily (next submit)"
            );
            self.inner = None;
            self.bound_device = 0;
            return true;
        }
        // SAFETY: the MFT is live on this thread; flush + stream restart is the documented
        // recovery order, and each message is a synchronous no-argument call.
        let restarted = unsafe {
            inner
                .mft
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)
                .and_then(|()| {
                    inner
                        .mft
                        .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
                })
                .and_then(|()| {
                    inner
                        .mft
                        .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                })
                .and_then(|()| {
                    inner
                        .mft
                        .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                })
        };
        // The flush voided every in-flight frame and the queued events that named them.
        inner.pending.clear();
        inner.ready.clear();
        inner.need_input = 0;
        inner.frames_submitted = 0;
        inner.first_au_logged = false;
        if let Err(e) = restarted {
            tracing::warn!(error = %e, "Media Foundation in-place restart failed — dropping the MFT");
            self.inner = None;
            self.bound_device = 0;
        } else {
            tracing::info!("Media Foundation encoder restarted in place (flush + begin streaming)");
        }
        true
    }

    /// No-IDR ABR: re-set the mean/max rate on the live `ICodecAPI`. Every vendor
    /// documents these as dynamic; `false` sends the caller to a full rebuild.
    fn reconfigure_bitrate(&mut self, bps: u64) -> bool {
        let old = self.bitrate_bps;
        self.bitrate_bps = bps;
        let cfg = self.encode_config();
        let Some(inner) = self.inner.as_ref() else {
            return true; // Not open yet: the next bring-up carries the new rate.
        };
        let Some(api) = inner.codec_api.as_ref() else {
            self.bitrate_bps = old;
            return false;
        };
        if apply_bitrate(api, &cfg) {
            return true;
        }
        tracing::warn!(
            mbps = bps / 1_000_000,
            "the MFT declined the in-place bitrate retarget — falling back to a rebuild"
        );
        self.bitrate_bps = old;
        false
    }

    fn flush(&mut self) -> Result<()> {
        let codec = self.codec;
        let Some(inner) = self.inner.as_mut() else {
            return Ok(());
        };
        // SAFETY: the MFT is live on this thread; end-of-stream then drain is the
        // documented order, and both are synchronous no-argument messages.
        unsafe {
            inner
                .mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .and_then(|()| inner.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0))
                .context("Media Foundation drain")?;
        }
        // Owed AUs arrive as HaveOutput events; surface them through `poll`.
        let _ = wait_until(inner, codec, "drain", |i| i.pending.is_empty());
        // End-of-stream zeroed the MFT's input credit and it issues no more until a fresh
        // start-of-stream, so without this a later submit stalls out its whole budget.
        // SAFETY: the MFT is live on this thread; a synchronous no-argument message.
        unsafe {
            let _ = inner
                .mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }
        inner.need_input = 0;
        Ok(())
    }
}

/// Can this adapter encode `codec` in hardware through Media Foundation? Enumeration is
/// the probe: an MFT that lists the pair opens it.
pub fn probe_can_encode(codec: Codec, adapter_luid: Option<LUID>) -> bool {
    enumerate(codec, adapter_luid).is_ok_and(|m| !m.is_empty())
}

/// No MFT encodes 10-bit for us: the whole backend is 8-bit 4:2:0 by design.
pub fn probe_can_encode_10bit(_codec: Codec, _adapter_luid: Option<LUID>) -> bool {
    false
}

/// Does this adapter have **any** hardware encoder MFT? The resolution policy's question:
/// an unknown-vendor GPU (Adreno) with an MFT is a GPU backend, not the software rung.
pub fn probe_has_hardware_encoder(adapter_luid: Option<LUID>) -> bool {
    probe_can_encode(Codec::H264, adapter_luid) || probe_can_encode(Codec::H265, adapter_luid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Enumeration must not crash; no MFT → empty list or a clean error.
    #[test]
    fn mf_enumeration_smoke() {
        for codec in [Codec::H264, Codec::H265] {
            match enumerate(codec, None) {
                Ok(found) => {
                    let names: Vec<_> = found.iter().map(friendly_name).collect();
                    tracing::debug!(?codec, ?names, "Media Foundation encoder MFTs");
                }
                Err(e) => tracing::debug!(?codec, error = %format!("{e:#}"), "no MFT enumeration"),
            }
        }
    }

    /// Probe answers are booleans (no panic) with or without a hardware MFT.
    #[test]
    fn probe_smoke() {
        for codec in [Codec::H264, Codec::H265, Codec::Av1] {
            let can = probe_can_encode(codec, None);
            assert!(!probe_can_encode_10bit(codec, None));
            tracing::debug!(?codec, can, "MF probe");
        }
        // AV1 is refused at the subtype gate, whatever the box has.
        assert!(!probe_can_encode(Codec::Av1, None));
    }

    #[test]
    fn nal_classification_matches_the_two_codecs() {
        assert!(is_parameter_set(Codec::H264, 8));
        assert!(!is_parameter_set(Codec::H264, 5));
        assert!(is_parameter_set(Codec::H265, 34));
        assert!(!is_parameter_set(Codec::H265, 19));
    }

    /// The cached prefix must end exactly at the first slice NAL, start-code included.
    #[test]
    fn parameter_set_prefix_cuts_at_the_first_slice() {
        // SPS (0x67) + PPS (0x68) + IDR slice (0x65).
        let au = [
            0, 0, 0, 1, 0x67, 0xAA, //
            0, 0, 0, 1, 0x68, 0xBB, //
            0, 0, 0, 1, 0x65, 0xCC,
        ];
        let prefix = parameter_set_prefix(Codec::H264, &au).expect("SPS-led AU has a prefix");
        assert_eq!(prefix, &au[..12]);
        // An AU that opens with the slice has no prefix to cache.
        assert!(parameter_set_prefix(Codec::H264, &au[12..]).is_none());
        // Parameter sets with no slice after them are all prefix.
        assert_eq!(
            parameter_set_prefix(Codec::H264, &au[..12]).expect("all parameter sets"),
            &au[..12]
        );
    }

    /// An MFT that opens the AU with a delimiter still gets its header cached. That is
    /// the Qualcomm shape, and requiring the run to come first defeated it entirely.
    #[test]
    fn parameter_set_prefix_skips_a_leading_delimiter() {
        // AUD (0x09) + SPS (0x67) + PPS (0x68) + IDR slice (0x65).
        let au = [
            0, 0, 0, 1, 0x09, 0x10, //
            0, 0, 0, 1, 0x67, 0xAA, //
            0, 0, 0, 1, 0x68, 0xBB, //
            0, 0, 0, 1, 0x65, 0xCC,
        ];
        let prefix =
            parameter_set_prefix(Codec::H264, &au).expect("delimiter-led AU still has a prefix");
        assert_eq!(prefix, &au[6..18]);
        // SEI-led, parameter sets running to the end of the AU.
        assert_eq!(
            parameter_set_prefix(Codec::H264, &au[..18]).expect("delimiter then parameter sets"),
            &au[6..18]
        );
    }

    /// The adapter the device sits on, as `MFT_ENUM_ADAPTER_LUID` wants it.
    fn adapter_luid_of(device: &ID3D11Device) -> Option<LUID> {
        use windows::Win32::Graphics::Dxgi::IDXGIDevice;
        // SAFETY: standard COM navigation on a live device; every interface is an owned
        // windows-rs wrapper released on drop, and `GetDesc` fills a plain out-struct.
        unsafe {
            let dxgi: IDXGIDevice = device.cast().ok()?;
            Some(dxgi.GetAdapter().ok()?.GetDesc().ok()?.AdapterLuid)
        }
    }

    fn init_tracing() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("pf_encode_win=debug")
            .with_test_writer()
            .try_init();
    }

    struct AuMeta {
        keyframe: bool,
        annexb_start: bool,
        len: usize,
    }

    /// Live encode on whatever MFT this box has. `None` = skip. `on_frame` runs before
    /// each submit, so a test can retarget or force mid-stream.
    fn drive_live(
        codec: Codec,
        frames: u32,
        mut on_frame: impl FnMut(&mut MfEncoder, u32),
    ) -> Option<Vec<AuMeta>> {
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11CreateDevice, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
        };

        init_tracing();
        // SAFETY: self-contained harness owning every COM handle it creates;
        // `D3D11CreateDevice` fills `device` only on success, and the NV12 texture is
        // created on and used from that one device and thread.
        let (device, tex) = unsafe {
            let mut device = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE::default(),
                // The device manager refuses a device without this and the MFT then fails
                // SET_D3D_MANAGER with a bare E_FAIL — the driver's pooled device carries it
                // for the same reason.
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .expect("d3d11 device");
            let device: ID3D11Device = device.expect("device");
            let desc = D3D11_TEXTURE2D_DESC {
                Width: 640,
                Height: 480,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut t: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&desc, None, Some(&mut t))
                .expect("input texture");
            (device.clone(), t.expect("texture"))
        };
        // Bind the MFT to the device's OWN adapter, as a session does. A box with two vendors'
        // encoders hands `None` whichever the sort ranks first, and an MFT from one adapter
        // refuses a device from another with a bare E_FAIL at SET_D3D_MANAGER.
        let luid = adapter_luid_of(&device).expect("device LUID");
        if !probe_can_encode(codec, Some(luid)) {
            eprintln!("skipping: no hardware {codec:?} MFT on this box's render adapter");
            return None;
        }
        let mut enc = MfEncoder::open(
            codec,
            PixelFormat::Nv12,
            640,
            480,
            30,
            2_000_000,
            8,
            ChromaFormat::Yuv420,
            Some(luid),
        )
        .expect("open");
        let mut aus = Vec::new();
        let mut push = |au: EncodedFrame| {
            aus.push(AuMeta {
                keyframe: au.keyframe,
                annexb_start: au.data.starts_with(&[0, 0, 0, 1]) || au.data.starts_with(&[0, 0, 1]),
                len: au.data.len(),
            });
        };
        for i in 0..frames {
            on_frame(&mut enc, i);
            let frame = CapturedFrame {
                provenance: Default::default(),
                width: 640,
                height: 480,
                pts_ns: u64::from(i) * 33_333_333,
                format: PixelFormat::Nv12,
                payload: FramePayload::D3d11(pf_frame::dxgi::D3d11Frame {
                    texture: tex.clone(),
                    device: device.clone(),
                    pyro: None,
                }),
                cursor: None,
            };
            enc.submit(&frame).expect("submit");
            if let Some(au) = enc.poll().expect("poll") {
                push(au);
            }
        }
        enc.flush().expect("flush");
        while let Some(au) = enc.poll().expect("drain") {
            push(au);
        }
        Some(aus)
    }

    fn assert_stream_shape(aus: &[AuMeta], frames: u32) {
        assert!(
            aus.len() >= frames as usize - 5,
            "expected ~{frames} AUs, got {}",
            aus.len()
        );
        assert!(aus[0].keyframe, "first AU must be a keyframe");
        assert!(aus[0].len > 0);
        assert!(aus[0].annexb_start, "first AU is not Annex-B");
    }

    #[test]
    fn mf_encode_live_smoke() {
        let Some(aus) = drive_live(Codec::H264, 30, |_, _| {}) else {
            return;
        };
        assert_stream_shape(&aus, 30);
    }

    #[test]
    fn mf_live_hevc() {
        let Some(aus) = drive_live(Codec::H265, 30, |_, _| {}) else {
            return;
        };
        assert_stream_shape(&aus, 30);
    }

    /// Mid-stream `reconfigure_bitrate` must accept and must not emit a keyframe.
    #[test]
    fn mf_live_bitrate_retarget() {
        let mut accepted = false;
        let Some(aus) = drive_live(Codec::H264, 60, |enc, i| {
            if i == 30 {
                accepted = enc.reconfigure_bitrate(6_000_000);
            }
        }) else {
            return;
        };
        assert_stream_shape(&aus, 60);
        assert!(accepted, "the in-place bitrate retarget was declined");
        assert!(
            !aus[1..].iter().any(|x| x.keyframe),
            "the bitrate retarget emitted a keyframe"
        );
    }

    /// `request_keyframe` must produce an IDR where it was asked for — the one device
    /// question that decides whether loss recovery works at all (design §6, Q1).
    #[test]
    fn mf_live_force_idr() {
        let Some(aus) = drive_live(Codec::H264, 60, |enc, i| {
            if i == 30 {
                enc.request_keyframe();
            }
        }) else {
            return;
        };
        assert_stream_shape(&aus, 60);
        let forced: Vec<usize> = aus
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, a)| a.keyframe)
            .map(|(i, _)| i)
            .collect();
        assert!(
            forced.iter().any(|&i| (28..=34).contains(&i)),
            "no IDR near the forced frame (keyframes at {forced:?}) — this MFT ignores \
             AVEncVideoForceKeyFrame; loss recovery must fall back to a periodic GOP"
        );
    }
}
