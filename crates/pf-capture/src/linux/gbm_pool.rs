//! A small pool of GBM-allocated dmabufs, for capture protocols where the *client*
//! supplies the buffers (`ext-image-copy-capture-v1`), unlike PipeWire where the
//! producer allocates.
//!
//! Only `libgbm` — already linked for the EGL importer — plus a render-node open.
//! No new build dependency: every Mesa/NVIDIA install ships it.

use anyhow::{anyhow, bail, Context, Result};
use std::ffi::c_void;
use std::os::fd::{FromRawFd, OwnedFd};

// `gbm_create_device`/`gbm_device_destroy` are already declared in pf-zerocopy for its
// EGL device. These are the allocation half; both resolve to the same `libgbm`.
#[link(name = "gbm")]
unsafe extern "C" {
    fn gbm_create_device(fd: i32) -> *mut c_void;
    fn gbm_device_destroy(device: *mut c_void);
    fn gbm_bo_create(
        device: *mut c_void,
        width: u32,
        height: u32,
        format: u32,
        flags: u32,
    ) -> *mut c_void;
    fn gbm_bo_create_with_modifiers2(
        device: *mut c_void,
        width: u32,
        height: u32,
        format: u32,
        modifiers: *const u64,
        count: u32,
        flags: u32,
    ) -> *mut c_void;
    fn gbm_bo_destroy(bo: *mut c_void);
    fn gbm_bo_get_fd(bo: *mut c_void) -> i32;
    fn gbm_bo_get_stride(bo: *mut c_void) -> u32;
    fn gbm_bo_get_offset(bo: *mut c_void, plane: i32) -> u32;
    fn gbm_bo_get_modifier(bo: *mut c_void) -> u64;
    fn gbm_bo_get_plane_count(bo: *mut c_void) -> i32;
}

/// The buffer is rendered into by the compositor's GPU.
const GBM_BO_USE_RENDERING: u32 = 1 << 2;
/// `gbm_bo_get_modifier` on a device that has none.
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// Render node whose `rdev` matches the `dev_t` a compositor advertised.
///
/// The protocol names the device by number, not path. Scanning `/dev/dri` is the only
/// mapping; `drmGetDeviceNameFromFd2` would need libdrm for one lookup.
pub(super) fn render_node_for(dev: u64) -> Result<std::fs::File> {
    let dir = std::fs::read_dir("/dev/dri").context("open /dev/dri")?;
    for e in dir.flatten() {
        let path = e.path();
        let Ok(md) = std::fs::metadata(&path) else {
            continue;
        };
        use std::os::unix::fs::MetadataExt;
        if md.rdev() == dev {
            return std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .with_context(|| format!("open {}", path.display()));
        }
    }
    bail!("no /dev/dri node with device id {dev:#x} — the compositor named a device this host cannot open")
}

/// One allocated buffer: the dmabuf the compositor writes and the encoder reads.
pub(super) struct Bo {
    raw: *mut c_void,
    /// Owned once; every published frame carries a `dup`.
    pub(super) fd: OwnedFd,
    pub(super) stride: u32,
    pub(super) offset: u32,
    pub(super) modifier: u64,
}

impl Drop for Bo {
    fn drop(&mut self) {
        // SAFETY: `raw` is the non-null bo this `Bo` uniquely owns, from `gbm_bo_create*`.
        // `drop` runs once, so the bo is destroyed exactly once. The fd is independent
        // (`gbm_bo_get_fd` returns a fresh one) and closes with `self.fd`.
        unsafe { gbm_bo_destroy(self.raw) };
    }
}

// SAFETY: the `gbm_bo*` is created, read and destroyed only on the capture thread that
// owns the pool. `Send` exists so the pool can be moved onto that thread once at
// construction; nothing published carries the pointer — a frame gets a `dup` of the fd,
// which is a plain owned fd with no thread affinity.
unsafe impl Send for Bo {}

/// A GBM device plus the buffers allocated on it, alive together.
pub(super) struct GbmPool {
    device: *mut c_void,
    /// Closed after `gbm_device_destroy`; the device borrows it.
    _node: std::fs::File,
    pub(super) bos: Vec<Bo>,
}

impl Drop for GbmPool {
    fn drop(&mut self) {
        self.bos.clear(); // bos reference the device, so free them first
                          // SAFETY: `device` is the non-null device from `gbm_create_device`, owned here and
                          // destroyed once. Every bo on it is already dropped by the line above.
        unsafe { gbm_device_destroy(self.device) };
    }
}

// SAFETY: as for `Bo` — the `gbm_device*` is touched only on the capture thread, and this
// impl exists so the whole pool can be moved there before any buffer is allocated.
unsafe impl Send for GbmPool {}

impl GbmPool {
    /// Allocate `count` buffers of `width`×`height` in `fourcc`.
    ///
    /// `modifiers` is what the compositor offered, already narrowed to what the consumer
    /// can import. An empty list (or an allocation that refuses every one) falls back to
    /// the implicit-modifier `gbm_bo_create`, which is what a LINEAR-only path needs.
    pub(super) fn new(
        node: std::fs::File,
        width: u32,
        height: u32,
        fourcc: u32,
        modifiers: &[u64],
        count: usize,
    ) -> Result<GbmPool> {
        use std::os::fd::AsRawFd;
        // SAFETY: `gbm_create_device` takes the DRM fd by value and returns a device (or
        // null, checked). It borrows the fd, which `_node` keeps open for the pool's life.
        let device = unsafe { gbm_create_device(node.as_raw_fd()) };
        if device.is_null() {
            bail!("gbm_create_device failed on the compositor's render node");
        }
        let mut pool = GbmPool {
            device,
            _node: node,
            bos: Vec::with_capacity(count),
        };
        for _ in 0..count {
            pool.bos.push(pool.alloc(width, height, fourcc, modifiers)?);
        }
        Ok(pool)
    }

    fn alloc(&self, width: u32, height: u32, fourcc: u32, modifiers: &[u64]) -> Result<Bo> {
        // SAFETY: both calls take the live device plus plain integers, and `modifiers` is
        // read as `count` u64s for the duration of the call. Each returns a bo or null.
        let raw = unsafe {
            let with_mods = if modifiers.is_empty() {
                std::ptr::null_mut()
            } else {
                gbm_bo_create_with_modifiers2(
                    self.device,
                    width,
                    height,
                    fourcc,
                    modifiers.as_ptr(),
                    modifiers.len() as u32,
                    GBM_BO_USE_RENDERING,
                )
            };
            if with_mods.is_null() {
                gbm_bo_create(self.device, width, height, fourcc, GBM_BO_USE_RENDERING)
            } else {
                with_mods
            }
        };
        if raw.is_null() {
            bail!("gbm_bo_create failed for {width}x{height} fourcc {fourcc:#010x}");
        }
        let bo = Bo {
            raw,
            // SAFETY: `gbm_bo_get_fd` returns a fresh fd the caller owns (or -1, checked
            // below before `OwnedFd` adopts it), independent of the bo's own lifetime.
            fd: unsafe {
                let fd = gbm_bo_get_fd(raw);
                if fd < 0 {
                    gbm_bo_destroy(raw);
                    return Err(anyhow!("gbm_bo_get_fd failed"));
                }
                OwnedFd::from_raw_fd(fd)
            },
            // SAFETY: plain accessors on the bo just created, valid until `gbm_bo_destroy`.
            stride: unsafe { gbm_bo_get_stride(raw) },
            // SAFETY: as above.
            offset: unsafe { gbm_bo_get_offset(raw, 0) },
            // SAFETY: as above.
            modifier: unsafe { gbm_bo_get_modifier(raw) },
        };
        // SAFETY: as above. A multi-plane bo cannot travel our single-fd frame contract.
        let planes = unsafe { gbm_bo_get_plane_count(raw) };
        if planes > 1 {
            bail!("gbm allocated a {planes}-plane buffer; the frame contract carries one fd");
        }
        Ok(bo)
    }
}

impl Bo {
    /// The modifier to advertise for this buffer. `INVALID` means the driver chose
    /// implicitly, which on the wire is LINEAR's neighbour: send it as-is and let the
    /// compositor reject it rather than guessing a layout.
    pub(super) fn wire_modifier(&self) -> u64 {
        self.modifier
    }

    pub(super) fn modifier_is_invalid(&self) -> bool {
        self.modifier == DRM_FORMAT_MOD_INVALID
    }
}
