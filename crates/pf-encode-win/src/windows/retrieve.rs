//! The completion signal a backend with a retrieve thread hands to a caller that parks on
//! handles ([`crate::Encoder::ready_event`]).
//!
//! Manual-reset, not auto: an access unit stays announced until the queue it announces is empty,
//! so two AUs never need two waits and a caller that probes with a zero timeout gets the same
//! answer twice. Set and clear both happen under the queue's own mutex, which is what orders
//! them against each other.

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent, WaitForSingleObject};

/// A manual-reset event this value alone closes.
pub struct Ready(HANDLE);

// SAFETY: a Win32 event handle is a process-wide token, not thread-affine. This value is its sole
// closer and hands out only borrowed copies, so the retrieve thread and the encode thread may
// both hold it.
unsafe impl Send for Ready {}
// SAFETY: as above — every operation is a `&self` call the OS serializes internally.
unsafe impl Sync for Ready {}

impl Ready {
    /// `None` if the OS refused the handle; the caller then runs without a completion signal.
    pub fn new() -> Option<Self> {
        // SAFETY: plain event creation — manual-reset, unsignalled, unnamed, no descriptor.
        let h = unsafe { CreateEventW(None, true, false, None) }.ok()?;
        Some(Self(h))
    }

    /// The raw handle for [`crate::Encoder::ready_event`]; valid while `self` lives.
    pub fn raw(&self) -> isize {
        self.0 .0 as isize
    }

    /// An access unit is waiting.
    pub fn set(&self) {
        // SAFETY: our own event, alive as long as `self`.
        unsafe {
            let _ = SetEvent(self.0);
        }
    }

    /// The queue is empty again.
    pub fn clear(&self) {
        // SAFETY: our own event, alive as long as `self`.
        unsafe {
            let _ = ResetEvent(self.0);
        }
    }

    /// Wait up to `ms` for [`Self::set`]; `true` if it was signalled. Manual-reset, so this
    /// leaves the event alone — only [`Self::clear`] takes the announcement back.
    pub fn wait(&self, ms: u32) -> bool {
        // SAFETY: our own event, alive as long as `self`.
        unsafe { WaitForSingleObject(self.0, ms) == WAIT_OBJECT_0 }
    }
}

impl Drop for Ready {
    fn drop(&mut self) {
        // SAFETY: we created this handle and hand out only borrowed copies, so this is its sole
        // close; every thread that borrowed it was joined first.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
