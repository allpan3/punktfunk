//! Link-only proof that the `pf-encode-win` backends survive the WDK toolchain
//! (`--features encode-probe`, P2 step 2.2). Takes the address of one entry point
//! per backend so the linker keeps it; calls nothing, opens no device, adds no
//! IOCTL. A probe build is not shippable — see the feature note in Cargo.toml.

/// Backend names in the order the addresses below pin them, for the load-time log.
pub fn backends_linked() -> &'static [&'static str] {
    // `as *const ()` first: a fn item cast straight to an integer is the
    // `function_casts_as_integer` lint.
    let addrs = [
        pf_encode_win::nvenc::NvencD3d11Encoder::open as *const (),
        pf_encode_win::amf::AmfEncoder::open as *const (),
        pf_encode_win::qsv::QsvEncoder::open as *const (),
        pf_encode_win::pyrowave::PyroWaveEncoder::open as *const (),
        pf_encode_win::convert::BgraToYuvPlanes::new as *const (),
    ];
    // Without the barrier, LTO can prove the array unread and drop every backend
    // with it — leaving a green build that linked nothing.
    core::hint::black_box(addrs);
    // One `pf_frame` type named through a pf-encode-win signature: the annotation
    // fails to compile if the driver graph resolves a second copy of pf-frame.
    let _: fn(pf_frame::PixelFormat, u8) -> bool = pf_encode_win::ten_bit_input;
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
