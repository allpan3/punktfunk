//! The host-created AU section, mapped and written from the driver side, and the
//! [`EncodeSession`] one `SET_ENCODE` installs on a monitor.
//!
//! [`AuSection`] adopts the two handle values the host duplicated into this process and owns
//! them from then on: `Drop` unmaps and closes, whatever the session's outcome, because the
//! host does not reap after an IOCTL that completed successfully. Every field past the
//! host-stamped layout is written through atomic views over the mapping — the encode thread
//! is the only writer, the host reads under the `latest` token's generation check.

use std::collections::VecDeque;
use std::mem::offset_of;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use pf_driver_proto::encode::au::{self, AuHeader, AuSlot};
use pf_driver_proto::encode::{FrameToken, SetEncodeRequest};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Memory::{FILE_MAP_READ, FILE_MAP_WRITE, MapViewOfFile};
use windows::Win32::System::Threading::SetEvent;

use super::convert::Fail;
use super::thread::EncodeThread;
use crate::worker::{OwnedHandle, OwnedView};

/// The mapped section and the ready event, owned.
pub struct AuSection {
    view: OwnedView,
    event: OwnedHandle,
    /// Host-stamped heap bounds, validated once at map time.
    heap_offset: u32,
    heap_bytes: u32,
}

// SAFETY: the raw view pointer is a mapped section alive until `Drop`; every cross-thread
// access goes through the atomic views below, and the handles are process-wide tokens.
unsafe impl Send for AuSection {}
// SAFETY: as above — shared references only reach atomic views of the mapping.
unsafe impl Sync for AuSection {}

impl AuSection {
    /// Map `section` and adopt both handles. `Err` means NOTHING was adopted: the values are
    /// left for the host to reap alongside the IOCTL's failure status.
    pub fn map(section: u64, event: u64, section_bytes: u32) -> Result<Self, Fail> {
        let map = HANDLE(section as usize as *mut core::ffi::c_void);
        // SAFETY: `map` is the section handle the host duplicated into this process; the byte
        // count is what the host declared, so a smaller section fails here instead of faulting
        // on a later write. The null `view.Value` is checked below.
        let view = unsafe {
            MapViewOfFile(
                map,
                FILE_MAP_READ | FILE_MAP_WRITE,
                0,
                0,
                section_bytes as usize,
            )
        };
        if view.Value.is_null() {
            dbglog!(
                "[pf-vd] encode: MapViewOfFile({section_bytes} B) failed: {:?}",
                windows::core::Error::from_win32()
            );
            return Err((-9, "map"));
        }
        // SAFETY: the view is at least `section_bytes` long (the map above), which the check
        // below proves covers the header; `read` copies the Pod header out.
        let header = unsafe { core::ptr::read(view.Value.cast::<AuHeader>()) };
        let fits = section_bytes as usize >= au::HEAP_OFFSET
            && au::au_readable(&header)
            && au::section_bytes(header.heap_bytes) <= section_bytes;
        if !fits {
            dbglog!("[pf-vd] encode: AU section unreadable: {header:?} in {section_bytes} B");
            // SAFETY: our own mapping of `view`, unmapped once here; the handles stay the host's.
            unsafe {
                let _ = windows::Win32::System::Memory::UnmapViewOfFile(view);
            }
            return Err((-9, "section"));
        }
        // SAFETY: `view`/`map` are the live mapping and section handle adopted above; `event`
        // is the duplicated ready event. Each value becomes the sole closer of its handle.
        let (view, event) = unsafe {
            (
                OwnedView::from_raw(view.Value, OwnedHandle::from_raw(map)),
                OwnedHandle::from_raw(HANDLE(event as usize as *mut core::ffi::c_void)),
            )
        };
        Ok(Self {
            view,
            event,
            heap_offset: header.heap_offset,
            heap_bytes: header.heap_bytes,
        })
    }

    /// `(offset, bytes)` of the heap inside the section.
    pub fn heap(&self) -> (u32, u32) {
        (self.heap_offset, self.heap_bytes)
    }

    fn u32_at(&self, off: usize) -> &AtomicU32 {
        // SAFETY: `off` is an `offset_of!` of a naturally-aligned u32 inside the header or the
        // slot table, both inside the mapping `map` validated; the view lives as long as `self`.
        unsafe { &*self.view.base().cast::<u8>().add(off).cast::<AtomicU32>() }
    }

    fn u64_at(&self, off: usize) -> &AtomicU64 {
        // SAFETY: as `u32_at`, for an 8-aligned u64 field.
        unsafe { &*self.view.base().cast::<u8>().add(off).cast::<AtomicU64>() }
    }

    /// Slot `i`'s state word.
    pub fn slot_state(&self, i: usize) -> &AtomicU32 {
        self.u32_at(au::slot_offset(i) + offset_of!(AuSlot, state))
    }

    /// Write slot `i`'s record, state last with Release, so a reader that Acquire-loads
    /// `PUBLISHED` sees the fields that belong to these bytes.
    pub fn publish_slot(&self, i: usize, slot: &AuSlot) {
        let base = au::slot_offset(i);
        self.u32_at(base + offset_of!(AuSlot, offset))
            .store(slot.offset, Ordering::Relaxed);
        self.u32_at(base + offset_of!(AuSlot, len))
            .store(slot.len, Ordering::Relaxed);
        self.u32_at(base + offset_of!(AuSlot, wire_seq))
            .store(slot.wire_seq, Ordering::Relaxed);
        self.u32_at(base + offset_of!(AuSlot, source_seq))
            .store(slot.source_seq, Ordering::Relaxed);
        self.u64_at(base + offset_of!(AuSlot, qpc_pts))
            .store(slot.qpc_pts, Ordering::Relaxed);
        self.u32_at(base + offset_of!(AuSlot, flags))
            .store(slot.flags, Ordering::Relaxed);
        self.slot_state(i).store(au::PUBLISHED, Ordering::Release);
    }

    /// Copy `bytes` into the heap at section offset `offset`. `false` — nothing written — for a
    /// range outside the heap, which no reservation the ring allocator hands out can be.
    #[must_use]
    pub fn write_heap(&self, offset: u32, bytes: &[u8]) -> bool {
        let start = offset as usize;
        let end = start + bytes.len();
        let heap_end = self.heap_offset as usize + self.heap_bytes as usize;
        if start < self.heap_offset as usize || end > heap_end {
            return false;
        }
        // SAFETY: the range is inside the heap (checked), which is inside the mapping; the
        // encode thread is the only writer and the host reads only slots it Acquire-loaded.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.view.base().cast::<u8>().add(start),
                bytes.len(),
            );
        }
        true
    }

    /// Store the publish token (Release, after the slot) and wake the host.
    pub fn publish_latest(&self, token: FrameToken) {
        self.u64_at(offset_of!(AuHeader, latest))
            .store(token.pack(), Ordering::Release);
        // SAFETY: `event` is the live host-created ready event this section owns.
        unsafe {
            let _ = SetEvent(self.event.as_raw());
        }
    }

    pub fn store_u32(&self, off: usize, v: u32) {
        self.u32_at(off).store(v, Ordering::Relaxed);
    }

    pub fn store_u64(&self, off: usize, v: u64) {
        self.u64_at(off).store(v, Ordering::Relaxed);
    }

    /// Read back a word this side owns — only the encode thread writes them, so a
    /// read-modify-write over one is uncontended.
    pub fn load_u64(&self, off: usize) -> u64 {
        self.u64_at(off).load(Ordering::Relaxed)
    }

    pub fn add_u32(&self, off: usize, n: u32) -> u32 {
        self.u32_at(off).fetch_add(n, Ordering::Relaxed) + n
    }

    pub fn add_u64(&self, off: usize, n: u64) -> u64 {
        self.u64_at(off).fetch_add(n, Ordering::Relaxed) + n
    }
}

/// One `ENCODE_CTL` op for the encode thread, drained between frames. One-shot: the host
/// sends the next only after this IOCTL returned, so the queue never holds more than a few.
#[derive(Clone, Copy, Debug)]
pub enum Ctl {
    RequestKeyframe,
    /// Wire indexes `first..=last`.
    InvalidateRefFrames(u32, u32),
    DistrustReferences,
    /// kbps.
    ReconfigureBitrate(u32),
    /// `pf_frame::HdrMeta` as its 28 bytes.
    SetHdrMeta([u8; 28]),
    Flush,
}

/// One monitor's live encode: the request it was opened from, the section it publishes into
/// and the thread doing it. The monitor holds one `Arc`; the encode thread holds another for
/// as long as it runs, so a detached thread keeps the section mapped until it really exits.
pub struct EncodeSession {
    pub request: SetEncodeRequest,
    pub section: AuSection,
    /// Stamped into every publish token; bumped per `SET_ENCODE` by the monitor.
    pub generation: u32,
    /// The first `wire_seq` the current thread stamps ([`crate::encode::thread`]).
    pub wire_seq_base: AtomicU32,
    /// The drain worker saw a different device epoch than the pool was built on (TDR): frames
    /// stop, the host's next `SET_ENCODE` rebuilds on the new device.
    pub stale: AtomicBool,
    /// The control mailbox ([`Ctl`]); the pool event wakes the thread to drain it.
    pub ctl: Mutex<VecDeque<Ctl>>,
    thread: Mutex<Option<EncodeThread>>,
}

impl EncodeSession {
    pub fn new(request: SetEncodeRequest, section: AuSection, generation: u32) -> Self {
        section.store_u32(offset_of!(AuHeader, generation), generation);
        section.store_u32(offset_of!(AuHeader, wire_seq_base), request.wire_seq_base);
        section.store_u32(offset_of!(AuHeader, encoder_state), au::ENCODER_CLOSED);
        Self {
            request,
            section,
            generation,
            wire_seq_base: AtomicU32::new(request.wire_seq_base),
            stale: AtomicBool::new(false),
            ctl: Mutex::new(VecDeque::new()),
            thread: Mutex::new(None),
        }
    }

    /// Queue one op for the thread; the caller wakes it.
    pub fn push_ctl(&self, op: Ctl) {
        crate::registry::lock(&self.ctl).push_back(op);
    }

    /// Everything queued, in order.
    pub fn take_ctl(&self) -> Vec<Ctl> {
        crate::registry::lock(&self.ctl).drain(..).collect()
    }

    /// Install the running thread; whatever it displaces is handed back to stop with no lock
    /// held.
    #[must_use]
    pub fn set_thread(&self, thread: EncodeThread) -> Option<EncodeThread> {
        crate::registry::lock(&self.thread).replace(thread)
    }

    /// Take the thread out; the caller stops it with no lock held.
    #[must_use]
    pub fn take_thread(&self) -> Option<EncodeThread> {
        crate::registry::lock(&self.thread).take()
    }

    /// Stop the thread within [`EncodeThread::STOP_BOUND`], detaching it if it will not.
    pub fn stop(&self) {
        if let Some(t) = self.take_thread() {
            t.stop(&self.section);
        }
    }
}
