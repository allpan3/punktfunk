//! Live (`#[ignore]`-style, hardware-skipping) tests that pair a `pf_encode_win`
//! backend with something only this crate links: libavcodec's AMF encoder for
//! the native-vs-ffmpeg A/B, and pf-capture's real `HdrP010Converter` output
//! for the QSV ingest path. Kept here so `pf-encode-win` has no dev-dependency
//! on either.

#![allow(dead_code)]

use super::*;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1};

const VENDOR_AMD: u32 = 0x1002;
const VENDOR_INTEL: u32 = 0x8086;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("pf_encode=debug")
        .with_test_writer()
        .try_init();
}

/// First DXGI adapter of `vendor_id`. `None` = no such GPU — caller skips.
fn adapter_of(vendor_id: u32) -> Option<IDXGIAdapter1> {
    // SAFETY: probe owns every handle; factory/adapter COM or err. Drops with the wrapper.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        for i in 0.. {
            let adapter: IDXGIAdapter1 = factory.EnumAdapters1(i).ok()?;
            if adapter.GetDesc1().ok()?.VendorId == vendor_id {
                return Some(adapter);
            }
        }
        None
    }
}

/// D3D11 device on the AMD adapter. `None` = no AMD GPU — caller skips.
fn amd_d3d11_device() -> Option<ID3D11Device> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
    use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, D3D11_SDK_VERSION};
    let adapter = adapter_of(VENDOR_AMD)?;
    let mut device: Option<ID3D11Device> = None;
    // SAFETY: CreateDevice fills `device` only on success; owned COM, this thread.
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            Default::default(),
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .ok()?;
    }
    device
}

/// DEFAULT-usage NV12 texture (uninit GPU memory; content is irrelevant).
fn nv12_texture(device: &ID3D11Device, w: u32, h: u32) -> ID3D11Texture2D {
    use windows::Win32::Graphics::Direct3D11::D3D11_BIND_SHADER_RESOURCE;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    // SAFETY: CreateTexture2D fills the out-param only on success; owned COM, this thread.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.expect("NV12 texture");
    tex.expect("NV12 texture")
}

/// `p`-quantile of `samples` (µs), sorting in place. `0` when empty.
fn percentile(samples: &mut [u128], p: f64) -> u128 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let idx = (((samples.len() - 1) as f64) * p).round() as usize;
    samples[idx]
}

/// Pace at `1/fps` and return each frame's submit→AU wall-clock (µs), FIFO-paired. Unflushed
/// trailing frames are left unmeasured so every sample is a genuine paced submit→AU.
#[allow(clippy::too_many_arguments)]
fn drive_and_measure(
    enc: &mut dyn Encoder,
    device: &ID3D11Device,
    tex: &ID3D11Texture2D,
    w: u32,
    h: u32,
    fps: u32,
    fmt: PixelFormat,
    frames: usize,
) -> Vec<u128> {
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};
    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let mut pending: VecDeque<Instant> = VecDeque::new();
    let mut samples: Vec<u128> = Vec::new();
    let mut next = Instant::now();
    for i in 0..frames {
        if let Some(d) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
        next += interval;
        let frame = CapturedFrame {
            provenance: Default::default(),
            width: w,
            height: h,
            pts_ns: 1 + i as u64,
            format: fmt,
            payload: pf_frame::FramePayload::D3d11(pf_frame::dxgi::D3d11Frame {
                texture: tex.clone(),
                device: device.clone(),
                pyro: None,
            }),
            cursor: None,
        };
        let t = Instant::now();
        enc.submit(&frame).expect("bench submit");
        pending.push_back(t);
        while let Some(_au) = enc.poll().expect("bench poll") {
            let ts = pending.pop_front().expect("FIFO pairing");
            samples.push(ts.elapsed().as_micros());
        }
    }
    samples
}

/// Native vs libavcodec-AMF submit→AU A/B on the same paced NV12 input. Opt-in
/// (`PUNKTFUNK_AMF_BENCH=1`); gated on `amf-qsv`. Skips without the AMD runtime/GPU.
#[cfg(feature = "amf-qsv")]
#[test]
fn amf_latency_ab_bench() {
    if std::env::var("PUNKTFUNK_AMF_BENCH").as_deref() != Ok("1") {
        eprintln!("skipping: set PUNKTFUNK_AMF_BENCH=1 to run the native-vs-ffmpeg latency A/B");
        return;
    }
    let Some(device) = amd_d3d11_device() else {
        eprintln!("skipping: no AMD adapter on this box");
        return;
    };
    let (w, h, fps) = (1920u32, 1080u32, 60u32);
    let bitrate = 20_000_000u64;
    let frames = 180usize;
    let tex = nv12_texture(&device, w, h);

    let mut native = match amf::AmfEncoder::open(
        Codec::H265,
        PixelFormat::Nv12,
        w,
        h,
        fps,
        bitrate,
        8,
        ChromaFormat::Yuv420,
        pf_gpu::resolve_render_adapter_luid(),
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("skipping: native AMF open declined ({e:#})");
            return;
        }
    };
    let mut native_us = drive_and_measure(
        &mut native,
        &device,
        &tex,
        w,
        h,
        fps,
        PixelFormat::Nv12,
        frames,
    );
    drop(native);

    let mut ffmpeg = ffmpeg_win::FfmpegWinEncoder::open(
        ffmpeg_win::WinVendor::Amf,
        Codec::H265,
        PixelFormat::Nv12,
        w,
        h,
        fps,
        bitrate,
        8,
        ChromaFormat::Yuv420,
    )
    .expect("libavcodec AMF open");
    let mut ffmpeg_us = drive_and_measure(
        &mut ffmpeg,
        &device,
        &tex,
        w,
        h,
        fps,
        PixelFormat::Nv12,
        frames,
    );
    drop(ffmpeg);

    let iv = 1_000_000u128 / fps as u128;
    let (n50, n99, nc) = (
        percentile(&mut native_us, 0.50),
        percentile(&mut native_us, 0.99),
        native_us.len(),
    );
    let (f50, f99, fc) = (
        percentile(&mut ffmpeg_us, 0.50),
        percentile(&mut ffmpeg_us, 0.99),
        ffmpeg_us.len(),
    );
    eprintln!("=== native AMF vs libavcodec-AMF  encode_us A/B ===");
    eprintln!("mode: {w}x{h}@{fps} HEVC, {frames} paced frames, frame period {iv} us");
    eprintln!(
        "native (direct SDK) : p50={n50} us  p99={n99} us  ({nc} AUs)  = {:.2} frame periods",
        n50 as f64 / iv as f64
    );
    eprintln!(
        "ffmpeg (libavcodec) : p50={f50} us  p99={f99} us  ({fc} AUs)  = {:.2} frame periods",
        f50 as f64 / iv as f64
    );
    if n50 > 0 {
        eprintln!(
            "native p50 is {:.1}x lower than ffmpeg",
            f50 as f64 / n50 as f64
        );
    }
    assert!(
        n50 < f50,
        "native encode_us p50 ({n50}) must beat the libavcodec hold ({f50})"
    );
    assert!(
        n50 < iv,
        "native encode_us p50 ({n50} us) should collapse below one frame period ({iv} us)"
    );
}

/// 1080p HEVC Main10 ingest through the real `HdrP010Converter` (RTV-written P010,
/// the IDD out-ring bind profile). Dumps `%TEMP%\pf_qsv_conv_1080_bars.h265`.
#[cfg(feature = "qsv")]
#[test]
fn qsv_live_hdr_converter_e2e_1080_dump() {
    const W: u32 = 1920;
    const H: u32 = 1080;

    init_tracing();
    let Some(adapter) = adapter_of(VENDOR_INTEL) else {
        eprintln!("skipping: no Intel adapter on this box");
        return;
    };
    // SAFETY: `GetDesc1` fills a plain out-struct for the live adapter.
    let luid = unsafe { adapter.GetDesc1() }
        .expect("adapter desc")
        .AdapterLuid;
    if !qsv::probe_can_encode_10bit(Codec::H265, Some(luid)) {
        eprintln!("skipping: this GPU declines 10-bit HEVC");
        return;
    }
    let mut luid_bytes = [0u8; 8];
    luid_bytes[..4].copy_from_slice(&luid.LowPart.to_le_bytes());
    luid_bytes[4..].copy_from_slice(&luid.HighPart.to_le_bytes());
    let (device, tex) =
        pf_capture::dxgi::hdr_p010_convert_bars_on_luid(luid_bytes, W, H).expect("converter bars");

    let mut enc = qsv::QsvEncoder::open(
        Codec::H265,
        PixelFormat::P010,
        W,
        H,
        30,
        10_000_000,
        10,
        ChromaFormat::Yuv420,
        Some(luid),
    )
    .expect("open");
    enc.set_hdr_meta(Some(pf_frame::HdrMeta {
        display_primaries: [[13250, 34500], [7500, 3000], [34000, 16000]], // G,B,R
        white_point: [15635, 16450],
        max_display_mastering_luminance: 10_000_000, // 1000 nits @ 0.0001 cd/m²
        min_display_mastering_luminance: 500,        // 0.05 nits
        max_cll: 1000,
        max_fall: 400,
    }));
    let mut stream = Vec::new();
    let mut aus = 0usize;
    for i in 0..12u32 {
        let frame = CapturedFrame {
            provenance: Default::default(),
            width: W,
            height: H,
            pts_ns: i as u64 * 33_333_333,
            format: PixelFormat::P010,
            payload: pf_frame::FramePayload::D3d11(pf_frame::dxgi::D3d11Frame {
                texture: tex.clone(),
                device: device.clone(),
                pyro: None,
            }),
            cursor: None,
        };
        enc.submit_indexed(&frame, i).expect("submit");
        if let Some(au) = enc.poll().expect("poll") {
            aus += 1;
            stream.extend_from_slice(&au.data);
        }
    }
    enc.flush().expect("flush");
    while let Some(au) = enc.poll().expect("drain") {
        aus += 1;
        stream.extend_from_slice(&au.data);
    }
    assert!(aus >= 10, "expected ≥10 AUs, got {aus}");
    let path = std::env::temp_dir().join("pf_qsv_conv_1080_bars.h265");
    std::fs::write(&path, &stream).expect("write dump");
    println!(
        "wrote {aus} AUs ({} bytes) to {}",
        stream.len(),
        path.display()
    );
}
