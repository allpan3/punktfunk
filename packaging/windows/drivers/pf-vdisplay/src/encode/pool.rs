//! The pool: three slots per monitor in the opened backend's input format, filled by the drain
//! worker's fused pass ([`Pool::offer`]) inside the acquire window and drained by the encode
//! thread. The monitor owns it, not the session: between sessions the newest frame keeps
//! landing in it, so a new `SET_ENCODE` on an idle desktop encodes the retained slot as its
//! first IDR and needs no compose. A pool built for another device epoch, size or format is
//! replaced by the next session; nothing rebuilds in place.
//!
//! [`Attached`] is the drain worker's cached view of the monitor's pool and session,
//! re-read only when `Monitor::encode_gen` moved, so the steady state takes no lock. It also
//! carries [`Cadence`], the compose-cadence histogram both modes stamp after `Finished`.
//!
//! [`bypass_enabled`] is spike S6 (design §3): no fused pass at all — the encoder reads the
//! acquired surface and the drain worker holds `FinishedProcessingFrame` until the AU is out.

use std::collections::VecDeque;
use std::mem::offset_of;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pf_driver_proto::encode::au::AuHeader;
use pf_frame::CapturedFrame;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::Win32::System::Threading::{SetEvent, WaitForSingleObject};
use windows62::Win32::Graphics::Direct3D11 as d3d;

use super::convert::{Fail, InputKind, Targets, bridge};
use super::section::EncodeSession;
use super::thread::{qpc_frequency, qpc_now};
use crate::cursor_cell::CursorCell;
use crate::direct_3d_device::Direct3DDevice;
use crate::monitor::Monitor;
use crate::registry::lock;
use crate::worker::OwnedHandle;

/// Three slots: the encoder holds up to two in flight while the drain worker fills one.
pub const SLOTS: usize = 3;

/// How long the bypass drain worker holds `FinishedProcessingFrame` for the encoder. A wedged
/// encoder must cost the stream, never the head: 100 ms is twelve frame periods at 120 Hz,
/// past any real access unit, and the timeout drops the frame rather than freezing DWM.
const BYPASS_HOLD_MS: u32 = 100;

/// Spike S6 (design §3): encode straight off the acquired surface, `Finished` on the encoder's
/// completion, no fused pass. The cargo feature keeps the arm out of a shipping build, and
/// inside a spike build `PFVD_POOL_BYPASS` (any value; machine environment plus a device
/// restart) still has to be set. Only [`InputKind::Bgra`] can take it — every other kind
/// needs its converter — so [`Pool::build`] decides per session.
#[cfg(feature = "pool-bypass")]
pub fn bypass_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::log::knob("PFVD_POOL_BYPASS").is_some())
}

/// Always false without the `pool-bypass` feature: the spike cannot ship live.
#[cfg(not(feature = "pool-bypass"))]
pub fn bypass_enabled() -> bool {
    false
}

/// What [`Pool::offer`] did with a surface.
pub enum Offer {
    /// In a slot; the frame's source sequence.
    Taken(u64),
    /// Counted; the new drop total.
    Dropped(u64),
    /// Not this pool's surface (device epoch, size or format) — nothing counted.
    Refused,
}

struct State {
    targets: Targets,
    free: Vec<usize>,
    /// `(slot, PresentDisplayQPCTime, source_seq)` in acquire order.
    full: VecDeque<(usize, u64, u64)>,
    /// Handed to the encoder, AU still owed.
    encoding: Vec<usize>,
    /// An encode thread is consuming. Without one the newest frame recycles the oldest full
    /// slot, so the retained image is always the current desktop.
    live: bool,
    /// Bypass only: the acquired surface the encoder reads. One at a time — the drain worker
    /// waits for its release before it acquires again — and the reference here is what keeps
    /// DWM's frame alive past the acquire's own.
    held: Option<d3d::ID3D11Texture2D>,
}

/// One monitor's pool. See the module docs.
pub struct Pool {
    device_epoch: u32,
    width: u32,
    height: u32,
    kind: InputKind,
    source_format: DXGI_FORMAT,
    state: Mutex<State>,
    /// Auto-reset, signalled once per filled slot.
    event: OwnedHandle,
    /// The monitor's source sequence, advanced per frame handed to the pool.
    source_seq: Arc<AtomicU64>,
    /// Frames dropped at the pool or skipped by the encode thread for a full slot table.
    dropped: AtomicU64,
    /// The monitor's cursor: read at every pass for the blend decision, at every frame for
    /// the shape.
    cursor: Arc<CursorCell>,
    /// S6: no fused pass, and the drain worker waits on `release_event` before `Finished`.
    bypass: bool,
    /// Bypass only: auto-reset, signalled when the encoder gives the held surface back.
    release_event: Option<OwnedHandle>,
}

impl Pool {
    /// Build the targets for `kind` at `size` on `device`.
    pub fn build(
        device: &Direct3DDevice,
        kind: InputKind,
        size: (u32, u32),
        source_seq: Arc<AtomicU64>,
        cursor: Arc<CursorCell>,
    ) -> Result<Arc<Self>, Fail> {
        let dev62: d3d::ID3D11Device = bridge(&device.device)?;
        let ctx62: d3d::ID3D11DeviceContext = bridge(&device.device_context)?;
        let targets = Targets::new(kind, &dev62, &ctx62, size, SLOTS)?;
        let event = OwnedHandle::event(false).ok_or((-2, "event"))?;
        // S6: the bypass needs its own release event; without one there is nothing to wait on,
        // so the pool stays on the fused pass.
        let release_event = (bypass_enabled() && kind == InputKind::Bgra)
            .then(|| OwnedHandle::event(false))
            .flatten();
        let bypass = release_event.is_some();
        Ok(Arc::new(Self {
            device_epoch: device.epoch(),
            width: size.0,
            height: size.1,
            kind,
            source_format: DXGI_FORMAT(kind.source_format().0),
            state: Mutex::new(State {
                targets,
                free: (0..SLOTS).collect(),
                full: VecDeque::new(),
                encoding: Vec::new(),
                live: false,
                held: None,
            }),
            event,
            source_seq,
            dropped: AtomicU64::new(0),
            cursor,
            bypass,
            release_event,
        }))
    }

    /// Whether this pool runs the S6 bypass ([`bypass_enabled`]).
    pub fn bypass(&self) -> bool {
        self.bypass
    }

    /// Whether a session opening `kind` at `size` on `device` can reuse this pool — and with
    /// it the retained slot.
    pub fn matches(&self, device: &Direct3DDevice, kind: InputKind, size: (u32, u32)) -> bool {
        self.device_epoch == device.epoch()
            && self.kind == kind
            && (self.width, self.height) == size
    }

    /// The filled-slot event, for the encode thread's wait.
    pub fn event(&self) -> HANDLE {
        self.event.as_raw()
    }

    /// The drain worker's pass: one GPU pass from the acquired surface into a free slot, then
    /// the event. Never blocks — a contended lock is a counted drop, as is a full pool with a
    /// live consumer. Under [`bypass_enabled`] there is no pass: the surface itself becomes
    /// the encoder's input and only one may be out at a time.
    pub fn offer(&self, device: &Direct3DDevice, tex: &ID3D11Texture2D, qpc: u64) -> Offer {
        if device.epoch() != self.device_epoch {
            return Offer::Refused;
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `tex` is the live acquired surface; `desc` is a valid local out-param.
        unsafe { tex.GetDesc(&mut desc) };
        if (desc.Width, desc.Height, desc.Format) != (self.width, self.height, self.source_format) {
            return Offer::Refused;
        }
        let Ok(mut st) = self.state.try_lock() else {
            return self.drop_one();
        };
        let i = if self.bypass {
            // A surface still held means the previous access unit is not out; this frame is
            // dropped rather than queued behind it, so the hold below is never nested.
            if st.held.is_some() {
                return self.drop_one();
            }
            match bridge::<d3d::ID3D11Texture2D>(tex) {
                Ok(src) => st.held = Some(src),
                Err(_) => return self.drop_one(),
            }
            // A hold the drain worker timed out on leaves its queue entry behind; it would pair
            // the next surface with the stale stamp, so the queue is this frame alone.
            st.full.clear();
            0
        } else {
            let recycled = (!st.live)
                .then(|| st.full.pop_front().map(|f| f.0))
                .flatten();
            let Some(i) = st.free.pop().or(recycled) else {
                return self.drop_one();
            };
            let blend = self.cursor.blend.load(Ordering::Relaxed);
            let passed =
                bridge::<d3d::ID3D11Texture2D>(tex).and_then(|src| st.targets.pass(&src, i, blend));
            if passed.is_err() {
                st.free.push(i);
                return self.drop_one();
            }
            i
        };
        let seq = self.source_seq.fetch_add(1, Ordering::Relaxed) + 1;
        st.full.push_back((i, qpc, seq));
        drop(st);
        // SAFETY: our own event, alive as long as `self`.
        unsafe {
            let _ = SetEvent(self.event.as_raw());
        }
        Offer::Taken(seq)
    }

    /// One more frame dropped; the new total, for the header.
    pub fn drop_one(&self) -> Offer {
        Offer::Dropped(self.dropped.fetch_add(1, Ordering::Relaxed) + 1)
    }

    /// The encode thread starts (`true`: only the newest full slot is kept, the stash) or
    /// stops (`false`: the pool goes back to recycling).
    pub fn set_live(&self, live: bool) {
        let mut st = lock(&self.state);
        st.live = live;
        if live {
            while st.full.len() > 1 {
                let (i, ..) = st.full.pop_front().expect("len > 1");
                st.free.push(i);
            }
        }
    }

    /// The oldest full slot, now the encoder's.
    pub fn take_full(&self) -> Option<(usize, u64, u64)> {
        let mut st = lock(&self.state);
        let f = st.full.pop_front()?;
        st.encoding.push(f.0);
        Some(f)
    }

    /// Hand a slot back, whether its AU was published or it was skipped. In bypass this is
    /// also where the acquired surface is given up, which releases the drain worker's hold —
    /// a surface the drain worker already took back on timeout signals nothing.
    pub fn release(&self, slot: usize) {
        let mut st = lock(&self.state);
        st.encoding.retain(|&s| s != slot);
        if !st.free.contains(&slot) {
            st.free.push(slot);
        }
        let handed_back = self.bypass && st.held.take().is_some();
        drop(st);
        if handed_back {
            self.signal_release();
        }
    }

    /// Every slot a departed encoder still held, freed — after a detach, whose encoder may be
    /// mid-read on the GPU; a torn first frame is the price of not waiting for it. The bypass
    /// surface goes back the same way, so a detach cannot leave the drain worker waiting.
    pub fn reclaim(&self) {
        let mut st = lock(&self.state);
        let slots = core::mem::take(&mut st.encoding);
        for slot in slots {
            if !st.free.contains(&slot) {
                st.free.push(slot);
            }
        }
        let handed_back = st.held.take().is_some();
        drop(st);
        if handed_back {
            self.signal_release();
        }
    }

    /// Release the drain worker's hold (see [`Self::wait_release`]).
    fn signal_release(&self) {
        if let Some(ev) = &self.release_event {
            // SAFETY: our own auto-reset event, alive as long as `self`.
            unsafe {
                let _ = SetEvent(ev.as_raw());
            }
        }
    }

    /// The drain worker's hold, bypass only: block until the encoder gave the acquired surface
    /// back, so `FinishedProcessingFrame` never returns a surface still being read. Bounded by
    /// [`BYPASS_HOLD_MS`]; a timeout takes the surface back, counts a drop and lets the head
    /// run on — the encoder then finds nothing to wrap and skips that frame.
    pub fn wait_release(&self) {
        let Some(ev) = &self.release_event else {
            return;
        };
        // SAFETY: our own auto-reset event, alive as long as `self`.
        let waited = unsafe { WaitForSingleObject(ev.as_raw(), BYPASS_HOLD_MS) };
        if waited == WAIT_OBJECT_0 {
            return;
        }
        let taken = lock(&self.state).held.take().is_some();
        if taken {
            self.drop_one();
            dbglog!("[pf-vd] encode: bypass hold timed out ({BYPASS_HOLD_MS} ms) — frame dropped");
        }
    }

    /// Wake the encode thread without a frame — a control op landed in its mailbox.
    pub fn wake(&self) {
        // SAFETY: our own event, alive as long as `self`.
        unsafe {
            let _ = SetEvent(self.event.as_raw());
        }
    }

    /// Wrap slot `slot` as the frame `submit` takes, the pointer blended in when the client
    /// draws none (the planar pair signals its fence here). In bypass the frame is the held
    /// surface itself and carries no pointer: there is no driver-owned image to draw one on,
    /// so that mode is only honest for a client that takes the cursor plane.
    pub fn frame(&self, slot: usize, pts_ns: u64) -> Result<CapturedFrame, Fail> {
        let cursor = self.cursor.to_blend();
        let mut st = lock(&self.state);
        if self.bypass {
            let src = st.held.clone().ok_or((-2, "bypass"))?;
            return Ok(st.targets.direct_frame(&src, pts_ns));
        }
        st.targets.frame(slot, pts_ns, cursor)
    }
}

/// The drain worker's view of its monitor's pool and session (see the module docs).
pub struct Attached {
    pool: Option<Arc<Pool>>,
    session: Option<Arc<EncodeSession>>,
    seen_gen: u32,
    cadence: Cadence,
}

impl Attached {
    pub fn new() -> Self {
        Self {
            pool: None,
            session: None,
            seen_gen: u32::MAX,
            cadence: Cadence::new(),
        }
    }

    /// Re-read the monitor's slots when its encode generation moved since the last pass.
    pub fn refresh(&mut self, monitor: &Monitor) {
        let generation = monitor.encode_gen.load(Ordering::Acquire);
        if generation == self.seen_gen {
            return;
        }
        self.seen_gen = generation;
        self.pool = monitor.pool();
        self.session = monitor.encode();
    }

    /// The hook, per acquired surface (see [`Pool::offer`]). A pool on another device epoch
    /// marks the session stale — logged once — and the frame goes nowhere. `true` means a
    /// bypass pool took the surface itself, so the caller owes [`Self::wait_release`] before
    /// `FinishedProcessingFrame`.
    pub fn offer(&self, device: &Direct3DDevice, tex: &ID3D11Texture2D, display_qpc: u64) -> bool {
        let Some(pool) = &self.pool else {
            return false;
        };
        let session = self.session.as_deref();
        let mut held = false;
        match pool.offer(device, tex, display_qpc) {
            Offer::Dropped(n) => {
                if let Some(s) = session {
                    s.section.store_u64(offset_of!(AuHeader, dropped_total), n);
                }
            }
            Offer::Taken(seq) => {
                held = pool.bypass();
                if let Some(s) = session {
                    s.section.store_u64(offset_of!(AuHeader, source_seq), seq);
                }
            }
            Offer::Refused => {
                if let Some(s) = session
                    && !s.stale.swap(true, Ordering::AcqRel)
                {
                    dbglog!(
                        "[pf-vd] encode: pool cannot take the surface (device epoch {} vs pool {}) — session stale until the next SET_ENCODE",
                        device.epoch(),
                        pool.device_epoch
                    );
                }
            }
        }
        held
    }

    /// Wait out the bypass hold ([`Pool::wait_release`]); a pool on the fused pass returns at
    /// once. Called between the offer and `FinishedProcessingFrame`, nowhere else.
    pub fn wait_release(&self) {
        if let Some(pool) = &self.pool {
            pool.wait_release();
        }
    }

    /// The drain heartbeat, stamped after `FinishedProcessingFrame` — never inside the window.
    pub fn note_drain(&self) {
        if let Some(s) = &self.session {
            s.section
                .store_u64(offset_of!(AuHeader, drain_heartbeat_qpc), qpc_now());
        }
    }

    /// The heartbeat plus one compose-cadence sample, both after `FinishedProcessingFrame`.
    /// `display_qpc` is the OS present stamp of the frame just handed back, so the deltas are
    /// DWM's own cadence on this head — the instrument S6 and gate §5-4 are judged on, stamped
    /// identically whichever mode the pool runs.
    pub fn note_frame(&mut self, display_qpc: u64) {
        self.note_drain();
        let fps = self.session.as_ref().map_or(0, |s| s.request.fps);
        let bypass = self.pool.as_ref().is_some_and(|p| p.bypass());
        self.cadence.note(display_qpc, fps, bypass);
    }
}

/// `PresentDisplayQPCTime` deltas in quarter frame periods: twelve buckets, the last one
/// everything at or over 2.75 periods, plus the run's worst gap. Reported every
/// [`Cadence::REPORT_MS`] to the driver log, which is what the S6 legs are compared from.
struct Cadence {
    hz: u64,
    last: u64,
    since: u64,
    n: u64,
    max_us: u64,
    buckets: [u32; 12],
}

impl Cadence {
    const REPORT_MS: u64 = 10_000;

    fn new() -> Self {
        Self {
            hz: qpc_frequency(),
            last: 0,
            since: 0,
            n: 0,
            max_us: 0,
            buckets: [0; 12],
        }
    }

    /// One acquired frame's present stamp. A zero stamp, a backwards one or an unknown refresh
    /// only re-anchors: the run's buckets stay in one unit.
    fn note(&mut self, qpc: u64, fps: u32, bypass: bool) {
        let period_us = 1_000_000 / u64::from(fps.max(1));
        let (last, since) = (self.last, self.since);
        self.last = qpc;
        if qpc == 0 || last == 0 || qpc <= last {
            self.since = qpc;
            return;
        }
        if since == 0 {
            self.since = last;
        }
        let delta_us = (qpc - last) * 1_000_000 / self.hz;
        self.n += 1;
        self.max_us = self.max_us.max(delta_us);
        let bucket = (delta_us * 4 / period_us.max(1)).min(11) as usize;
        self.buckets[bucket] += 1;
        let window_ms = (qpc - self.since) * 1_000 / self.hz;
        if window_ms < Self::REPORT_MS {
            return;
        }
        let h = self.buckets.map(|b| b.to_string()).join("/");
        dbglog!(
            "[pf-vd] cadence: mode={} fps={fps} win_ms={window_ms} n={} max_us={} h={h}",
            if bypass { "bypass" } else { "pool" },
            self.n,
            self.max_us
        );
        self.since = qpc;
        self.n = 0;
        self.max_us = 0;
        self.buckets = [0; 12];
    }
}
