//! Virtual monitors: the [`Monitor`] type and the control-plane verbs on it — create + arrive
//! (`IOCTL_ADD`), the in-place mode update, remove, clear, the watchdog reap, and the frame- and
//! cursor-channel deliveries — plus the mode-struct stamping the DDIs fill from.
//!
//! Ownership: [`crate::registry`] holds the only strong `Arc<Monitor>`; the drain worker holds a
//! `Weak`. Every field a worker, a DDI callback or an IOCTL can race on sits behind its own
//! mutex, held for a field swap and never across a DDI, a join, or a `Drop` that closes a
//! handle. Lock order is `REGISTRY → Monitor.*`, never reversed; workers never take the registry.
//!
//! Removal is two steps with no lock held between them: the registry hands the `Arc` back, then
//! [`Monitor::teardown`] stops the workers (cursor first, then the drain worker, the ring, an
//! unconsumed delivery), and only then does the caller run `IddCxMonitorDeparture`.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use pf_driver_proto::vdisplay;
use wdk_sys::{NTSTATUS, WDFOBJECT, call_unsafe_wdf_function_binding, iddcx};

use crate::cursor_worker::CursorChannel;
use crate::frame_transport::{FrameChannel, RingEndpoint};
use crate::registry::{self, lock};
use crate::swap_chain_processor::SwapChainProcessor;
use crate::worker::{OwnedHandle, Worker};

/// The advertised mode list and its flattening (the order the mode DDIs emit). Both live in
/// [`pf_driver_proto::vdisplay`], where they run under `cargo test` on any OS; re-exported so the
/// mode callbacks keep naming them through this module.
pub use pf_driver_proto::vdisplay::{Mode, flatten};

/// The IddCx monitor handle, set once `IddCxMonitorCreate` returns.
struct SendMonitor(iddcx::IDDCX_MONITOR);
// SAFETY: an opaque IddCx handle, only ever passed by value to IddCx DDIs (themselves the
// synchronisation point) and never dereferenced in Rust, so moving it between threads is sound.
unsafe impl Send for SendMonitor {}
// SAFETY: as above — a shared `&SendMonitor` yields only a by-value copy of the handle.
unsafe impl Sync for SendMonitor {}

/// What `IddCxMonitorArrival` reported: the OS target id — the key the host addresses every
/// later delivery by — and the render-adapter LUID for the ADD reply.
#[derive(Clone, Copy)]
pub struct Arrival {
    pub target_id: u32,
    pub luid_low: u32,
    pub luid_high: i32,
}

/// The hardware-cursor state (proto v5/v6).
///
/// `data_event` is the OS cursor-data event the hardware cursor is declared against. It is owned
/// here, not by the worker thread: the declare paths copy its raw value out and call the setup
/// DDI after the guard drops, so it may close only after the worker's join — every path that
/// replaces the pair drops the worker first. `forward_on == false` is the composite render mode:
/// the cursor stays un-declared and the per-mode-commit re-declare is skipped.
struct CursorState {
    data_event: Option<OwnedHandle>,
    worker: Option<Worker>,
    forward_on: bool,
}

/// A live (or pending) virtual monitor.
///
/// The registry holds the only strong `Arc`; the drain worker holds a `Weak` it upgrades per
/// pass, so a monitor is never kept alive by its own worker. Identity is plain fields, the
/// handle and the arrival record are write-once, and everything else has its own mutex, held
/// for a field swap only. Whatever a swap displaces goes back to the caller, who drops it with
/// no lock held: a join or a handle close under a lock head-blocks the control plane and every
/// mode DDI the OS issues during the same topology change.
///
/// `gone` is set first thing in [`teardown`](Self::teardown): an install landing after it gets
/// its value handed back instead of parking a worker on a monitor nobody will join.
pub struct Monitor {
    /// EDID serial / connector index — the key the mode DDIs match on.
    pub id: u32,
    /// The host's monotonic key (ADD/REMOVE).
    pub session_id: u64,
    /// The host asked for an IddCx hardware cursor at ADD.
    pub hw_cursor: bool,
    /// When the entry was created — the watchdog reap skips a still-initializing monitor.
    pub created_at: Instant,
    object: OnceLock<SendMonitor>,
    arrival: OnceLock<Arrival>,
    /// Advertised modes (requested mode first, then the proto's fallbacks).
    modes: Mutex<Vec<Mode>>,
    /// The live swap-chain drain worker; dropping it joins the thread.
    swap: Mutex<Option<SwapChainProcessor>>,
    /// A frame-channel delivery awaiting the drain worker's pickup. Exactly one owner per
    /// delivery: replacing or dropping it closes an unconsumed channel's handles.
    chan: Mutex<Option<FrameChannel>>,
    /// Bumped (Release) by every delivery landing in `chan`. The drain loop compares it
    /// (Acquire) with its last-seen value and locks `chan` only when one did, so its steady
    /// state (≥ 60 passes/s) takes no lock.
    pub chan_gen: AtomicU32,
    /// The monitor-owned ring endpoint of the current generation. Every drain worker — the next
    /// one after a swap-chain flap too — opens its own device-bound publisher on it, so nothing
    /// device-bound crosses assignments. The last `Arc` holder unmaps and closes it.
    endpoint: Mutex<Option<Arc<RingEndpoint>>>,
    /// The source sequence: advanced only by new desktop frames, monotonic across ring rebuilds
    /// — shared with every endpoint this monitor gets.
    pub source_seq: Arc<AtomicU64>,
    cursor: Mutex<CursorState>,
    gone: AtomicBool,
}

/// Take a slot's value with its guard already released, so the caller drops it lock-free.
fn take<T>(slot: &Mutex<Option<T>>) -> Option<T> {
    lock(slot).take()
}

impl Monitor {
    /// A registered-but-not-created entry; [`create_monitor`] fills the handle and arrival in.
    pub(crate) fn pending(id: u32, session_id: u64, hw_cursor: bool, modes: Vec<Mode>) -> Self {
        Self {
            id,
            session_id,
            hw_cursor,
            created_at: Instant::now(),
            object: OnceLock::new(),
            arrival: OnceLock::new(),
            modes: Mutex::new(modes),
            swap: Mutex::new(None),
            chan: Mutex::new(None),
            chan_gen: AtomicU32::new(0),
            endpoint: Mutex::new(None),
            source_seq: Arc::new(AtomicU64::new(0)),
            cursor: Mutex::new(CursorState {
                data_event: None,
                worker: None,
                forward_on: true,
            }),
            gone: AtomicBool::new(false),
        }
    }

    /// The IddCx handle — `None` until `IddCxMonitorCreate` returned.
    pub fn object(&self) -> Option<iddcx::IDDCX_MONITOR> {
        self.object.get().map(|o| o.0)
    }

    /// The OS target id — 0 until arrival, a value the host never sends (OS target ids are
    /// non-zero), so a pending entry matches no target-keyed lookup.
    pub fn target_id(&self) -> u32 {
        self.arrival.get().map_or(0, |a| a.target_id)
    }

    /// A clone of the advertised list, for a lock-free DDI fill.
    pub fn modes(&self) -> Vec<Mode> {
        lock(&self.modes).clone()
    }

    /// Stash a host frame-channel delivery for the drain worker and wake its idle wait, so an
    /// idle display attaches now instead of at its next timeout. The generation bump follows
    /// the store: a pass that read the old value attaches on its next. A superseded delivery —
    /// or `ch` itself, handed back as `Err` once the monitor is torn down — closes its handles
    /// only after every guard here has dropped.
    pub fn set_frame_channel(&self, ch: FrameChannel) -> Result<(), FrameChannel> {
        let superseded = {
            let mut slot = lock(&self.chan);
            if self.gone.load(Ordering::Acquire) {
                return Err(ch);
            }
            slot.replace(ch)
        };
        self.chan_gen.fetch_add(1, Ordering::Release);
        // `SetEvent` never blocks, so the `swap` guard may span it.
        let swap = lock(&self.swap);
        if let Some(p) = &*swap {
            p.wake();
        }
        drop(swap);
        drop(superseded);
        Ok(())
    }

    /// Whether a delivery is pending. The drain worker treats one as newest-wins over an
    /// attached publisher: the host only re-delivers after recreating the ring, and a retry-
    /// created ring is a different header mapping whose generation bump the old publisher can
    /// never observe.
    pub fn has_frame_channel(&self) -> bool {
        lock(&self.chan).is_some()
    }

    /// Take the pending delivery; the caller owns its handles from here.
    pub fn take_frame_channel(&self) -> Option<FrameChannel> {
        take(&self.chan)
    }

    /// The current ring endpoint, for a freshly assigned worker to open on its own device.
    pub fn endpoint(&self) -> Option<Arc<RingEndpoint>> {
        lock(&self.endpoint).clone()
    }

    /// Install the endpoint a worker built from a delivery, replacing the previous generation's.
    /// Whichever `Arc` this releases — the old one, or `ep` once the monitor is torn down —
    /// drops after the guard: as the last holder it unmaps the header and closes the ring.
    pub fn set_endpoint(&self, ep: Arc<RingEndpoint>) {
        let released = {
            let mut slot = lock(&self.endpoint);
            if self.gone.load(Ordering::Acquire) {
                Some(ep)
            } else {
                slot.replace(ep)
            }
        };
        drop(released);
    }

    /// Retire the endpoint if it is still ring generation `generation`: a worker whose open of
    /// a fresh delivery failed keeps that failure terminal for the delivery (the host reads the
    /// status; a retry is a new delivery). A newer endpoint is left alone. The retired `Arc`
    /// drops after the guard.
    pub fn clear_endpoint(&self, generation: u32) {
        let retired = lock(&self.endpoint).take_if(|e| e.generation() == generation);
        drop(retired);
    }

    /// Install the drain worker for a fresh swap-chain assignment. Returns the processor it
    /// displaced — or `proc` itself once the monitor is torn down — for the caller to drop with
    /// no lock held: dropping one joins its thread.
    #[must_use]
    pub fn set_swap(&self, proc: SwapChainProcessor) -> Option<SwapChainProcessor> {
        let mut slot = lock(&self.swap);
        if self.gone.load(Ordering::Acquire) {
            return Some(proc);
        }
        slot.replace(proc)
    }

    /// Take the drain worker out (unassign / reassign); the caller drops it with no lock held.
    #[must_use]
    pub fn take_swap(&self) -> Option<SwapChainProcessor> {
        take(&self.swap)
    }

    /// Install a fresh cursor worker and the event it waits on. Returns what they displaced —
    /// or the pair itself once the monitor is torn down — for the caller to drop with no lock
    /// held, worker first.
    fn set_cursor(
        &self,
        worker: Worker,
        data_event: OwnedHandle,
    ) -> (Option<Worker>, Option<OwnedHandle>) {
        let mut c = lock(&self.cursor);
        if self.gone.load(Ordering::Acquire) {
            return (Some(worker), Some(data_event));
        }
        (c.worker.replace(worker), c.data_event.replace(data_event))
    }

    /// Re-declare the hardware cursor after a swap-chain assign: a mode commit reverts the OS
    /// to a software cursor, and without this `IddCxMonitorQueryHardwareCursor` fails
    /// STATUS_NOT_SUPPORTED for good. No-op without a live worker, and in the composite render
    /// mode, whose software cursor is the point. The event value is copied out under the guard
    /// and the DDI runs after it: the DDI can re-enter the mode callbacks, and the event stays
    /// open because this monitor closes it only after joining the worker.
    pub fn resetup_cursor(&self) {
        let Some(object) = self.object() else {
            return;
        };
        let data_event = {
            let c = lock(&self.cursor);
            c.data_event
                .as_ref()
                .filter(|_| c.forward_on && c.worker.is_some())
                .map(|h| h.as_raw().0 as isize)
        };
        if let Some(ev) = data_event {
            let st = crate::cursor_worker::setup_hardware_cursor(object, ev);
            dbglog!("[pf-vd] cursor: re-setup on swap-chain assign -> {st:#x}");
            if wdk_iddcx::nt_success(st) {
                registry::mark_declared(self.target_id());
            }
        }
    }

    /// Stop everything whose drop blocks, each taken under its own guard and dropped after it:
    /// the cursor worker (it can be inside a hardware-cursor query against the handle the
    /// caller departs next), then the event it waited on, then the drain worker, the ring (its
    /// last `Arc` unmaps the header) and any delivery no worker picked up. `gone` is set first,
    /// so a racing install hands its value back instead of landing here unjoined. Run by the
    /// caller that removed this monitor from the registry, with the registry lock released.
    pub fn teardown(&self) {
        dbglog!(
            "[pf-vd] hcount: teardown entry n={}",
            crate::frame_transport::handle_count()
        );
        self.gone.store(true, Ordering::Release);
        let started = Instant::now();
        let (worker, event) = {
            let mut c = lock(&self.cursor);
            (c.worker.take(), c.data_event.take())
        };
        drop(worker);
        drop(event);
        drop(take(&self.swap));
        drop(take(&self.endpoint));
        drop(take(&self.chan));
        let took = started.elapsed();
        dbglog!(
            "[pf-vd] hcount: teardown exit n={}",
            crate::frame_transport::handle_count()
        );
        if took > Duration::from_millis(250) {
            dbglog!("[pf-vd] monitor teardown took {} ms", took.as_millis());
        }
    }
}

/// Tear every removed monitor down, then depart the ones that have an IddCx object: a worker
/// can be inside a DDI against the handle departure destroys, so the joins come first.
fn depart(removed: Vec<Arc<Monitor>>) {
    for m in &removed {
        m.teardown();
    }
    for m in removed {
        if let Some(object) = m.object() {
            // SAFETY: `object` is a live IddCx monitor handle; departure tears it down.
            unsafe { wdk_iddcx::IddCxMonitorDeparture(object) };
        }
    }
}

/// Depart every monitor that has existed at least `grace` — the host-gone watchdog reap
/// ([`crate::watchdog`]). The grace skips a just-created monitor (the host adds it, then starts
/// pinging) so a momentarily stale ping timer cannot reap a brand-new one. Returns the count.
pub fn reap_orphaned(grace: Duration) -> usize {
    let removed = registry::remove(|m| m.created_at.elapsed() >= grace);
    let n = removed.len();
    depart(removed);
    n
}

/// Stamp [`vdisplay::signal_info`]'s numbers into the OS `DISPLAYCONFIG_VIDEO_SIGNAL_INFO` —
/// one description for both mode DDI families, differing only in `v_sync_freq_divider` (0 for
/// monitor modes, 1 for target modes, per the DDI contract). Both sync rationals have
/// denominator 1, and total size equals active size: no fabricated blanking.
fn signal_info(
    width: u32,
    height: u32,
    refresh_rate: u32,
    v_sync_freq_divider: u32,
) -> wdk_sys::DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
    let n = vdisplay::signal_info(width, height, refresh_rate, v_sync_freq_divider);
    let region = wdk_sys::DISPLAYCONFIG_2DREGION {
        cx: width,
        cy: height,
    };
    let mut si = pod_init!(wdk_sys::DISPLAYCONFIG_VIDEO_SIGNAL_INFO);
    si.pixelRate = n.pixel_rate;
    si.hSyncFreq = wdk_sys::DISPLAYCONFIG_RATIONAL {
        Numerator: n.h_sync_num,
        Denominator: 1,
    };
    si.vSyncFreq = wdk_sys::DISPLAYCONFIG_RATIONAL {
        Numerator: n.v_sync_num,
        Denominator: 1,
    };
    si.totalSize = region;
    si.activeSize = region;
    si.scanLineOrdering =
        wdk_sys::DISPLAYCONFIG_SCANLINE_ORDERING::DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE;
    // union { AdditionalSignalInfo bitfield | videoStandard:u32 } — the proto packs the
    // vSyncFreqDivider into bits 16..21 of the "other" video standard.
    si.__bindgen_anon_1.videoStandard = n.video_standard;
    si
}

/// `DISPLAYCONFIG_VIDEO_SIGNAL_INFO` for a monitor mode (vSyncFreqDivider = 0, per the DDI contract).
pub fn display_info(
    width: u32,
    height: u32,
    refresh_rate: u32,
) -> wdk_sys::DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
    signal_info(width, height, refresh_rate, 0)
}

/// `IDDCX_TARGET_MODE` for a scan-out mode (vSyncFreqDivider = 1, per the DDI contract).
pub fn target_mode(width: u32, height: u32, refresh_rate: u32) -> iddcx::IDDCX_TARGET_MODE {
    let mut tm = pod_init!(iddcx::IDDCX_TARGET_MODE);
    tm.Size = core::mem::size_of::<iddcx::IDDCX_TARGET_MODE>() as u32;
    tm.TargetVideoSignalInfo = wdk_sys::DISPLAYCONFIG_TARGET_MODE {
        targetVideoSignalInfo: signal_info(width, height, refresh_rate, 1),
    };
    tm
}

/// Wire bit-depth advertised per mode in the `*2` (HDR) mode DDIs. STEP 7: advertise BOTH 8 and 10 bpc
/// RGB (so the OS offers HDR10 modes), no YCbCr. The wdk-sys bindgen enum is `ModuleConsts`, so each
/// `IDDCX_BITS_PER_COMPONENT_*` is a plain-int const and the `IDDCX_WIRE_BITS_PER_COMPONENT` fields are
/// plain ints — OR the constants directly (NO newtype `.0` like the oracle's wdf-umdf-sys binding). Field
/// names (Rgb/YCbCr444/YCbCr422/YCbCr420, IDDCX_BITS_PER_COMPONENT_8/_10/_NONE) are the verbatim C header
/// names, identical across both bindings.
pub fn wire_bits() -> iddcx::IDDCX_WIRE_BITS_PER_COMPONENT {
    let rgb = iddcx::IDDCX_BITS_PER_COMPONENT::IDDCX_BITS_PER_COMPONENT_8
        | iddcx::IDDCX_BITS_PER_COMPONENT::IDDCX_BITS_PER_COMPONENT_10;
    let mut w = pod_init!(iddcx::IDDCX_WIRE_BITS_PER_COMPONENT);
    w.Rgb = rgb;
    w.YCbCr444 = iddcx::IDDCX_BITS_PER_COMPONENT::IDDCX_BITS_PER_COMPONENT_NONE;
    w.YCbCr422 = iddcx::IDDCX_BITS_PER_COMPONENT::IDDCX_BITS_PER_COMPONENT_NONE;
    w.YCbCr420 = iddcx::IDDCX_BITS_PER_COMPONENT::IDDCX_BITS_PER_COMPONENT_NONE;
    w
}

/// `IDDCX_TARGET_MODE2` for a scan-out mode (HDR `*2` path): builds the v1 [`target_mode`] and copies its
/// `TargetVideoSignalInfo`, then stamps the `*2` Size + per-mode wire bit-depth ([`wire_bits`]). Rest
/// zeroed.
pub fn target_mode2(width: u32, height: u32, refresh_rate: u32) -> iddcx::IDDCX_TARGET_MODE2 {
    let m1 = target_mode(width, height, refresh_rate);
    let mut tm = pod_init!(iddcx::IDDCX_TARGET_MODE2);
    tm.Size = core::mem::size_of::<iddcx::IDDCX_TARGET_MODE2>() as u32;
    tm.TargetVideoSignalInfo = m1.TargetVideoSignalInfo;
    tm.BitsPerComponent = wire_bits();
    tm
}

/// Stash a host frame-channel delivery on the arrived monitor with `target_id`. `Err(ch)` if
/// there is none — the caller must NOT close those handles (the host only sees the error status
/// and reaps its remote duplicates itself; closing here too would double-close values the OS
/// may have reused).
pub fn set_frame_channel(target_id: u32, ch: FrameChannel) -> Result<(), FrameChannel> {
    if target_id == 0 {
        return Err(ch);
    }
    let Some(m) = registry::find(|m| m.target_id() == target_id) else {
        return Err(ch);
    };
    m.set_frame_channel(ch)
}

/// Adopt a hardware-cursor channel delivery (`IOCTL_SET_CURSOR_CHANNEL`, proto v5): create the
/// cursor-data event, declare the hardware cursor to the OS, start the worker. `Err(ch)` when no
/// arrived hw-cursor monitor has `target_id`, or the event could not be made. A re-delivery
/// replaces both — the host only re-sends after recreating the section.
///
/// A replaced worker is joined before the event it waited on closes, and both the setup DDI and
/// every join run with no lock held: the DDI can re-enter the mode callbacks, and a join under
/// a lock would head-block the control plane.
pub fn set_cursor_channel(target_id: u32, ch: CursorChannel) -> Result<(), CursorChannel> {
    if target_id == 0 {
        return Err(ch);
    }
    let Some(m) = registry::find(|m| m.target_id() == target_id && m.hw_cursor) else {
        return Err(ch);
    };
    let Some(object) = m.object() else {
        return Err(ch);
    };
    let (declare, old_worker, old_event) = {
        let mut c = lock(&m.cursor);
        (c.forward_on, c.worker.take(), c.data_event.take())
    };
    drop(old_worker); // join a replaced worker BEFORE the event it waits on closes
    drop(old_event);
    // Auto-reset: the OS signals it once per cursor update.
    let Some(data_event) = OwnedHandle::event(false) else {
        dbglog!("[pf-vd] cursor: data event creation failed — keeping composited cursor");
        return Err(ch);
    };
    // `declare = false`: the session is in the composite render mode (the mid-stream flip) —
    // adopt the channel and spawn the worker WITHOUT declaring the hardware cursor, so DWM
    // keeps compositing; a later enable-flip declares against this event.
    let Some(worker) =
        crate::cursor_worker::setup_and_spawn(object, ch, declare, data_event.as_raw().0 as isize)
    else {
        // setup_and_spawn consumed the channel and released everything it mapped; `data_event`
        // drops here. The host detects the missing publish and keeps its composited cursor.
        return Ok(());
    };
    if declare {
        // The worker only spawns after `IddCxMonitorSetupHardwareCursor` succeeded.
        registry::mark_declared(target_id);
    }
    let (displaced_worker, displaced_event) = m.set_cursor(worker, data_event);
    drop(displaced_worker); // join outside every lock, then close the event it waited on
    drop(displaced_event);
    Ok(())
}

/// The mid-stream cursor-render flip (`IOCTL_SET_CURSOR_FORWARD`, proto v6): `enable` declares
/// the hardware cursor again (DWM excludes the pointer; per-mode-commit re-declares resume);
/// disable only stores the flag, which stops the per-commit re-declare — there is no un-declare
/// DDI, so the host forces a same-mode re-commit whose software-cursor default then sticks.
/// `false` when no hw-cursor monitor has `target_id`.
///
/// The flip is state, not an edge on one monitor generation: the desired value persists per
/// target in the registry (a fresh entry inherits it at arrival) and is stamped on every live
/// entry matching the target, since duplicate generations coexist during re-arrival churn. The
/// DDI runs after every guard has dropped (it can re-enter the mode callbacks), against the
/// event value copied out of the entry, which closes it only after joining its worker.
pub fn set_cursor_forward(target_id: u32, enable: bool) -> bool {
    registry::set_cursor_forward_desired(target_id, enable);
    let matching = registry::find_all(|m| m.target_id() == target_id && m.hw_cursor);
    if matching.is_empty() {
        return false; // no hw-cursor monitor with this target at all
    }
    // Only a present worker gets the immediate declare DDI call.
    let mut declare_on: Option<(Option<iddcx::IDDCX_MONITOR>, Option<isize>)> = None;
    for m in &matching {
        let mut c = lock(&m.cursor);
        c.forward_on = enable;
        if c.worker.is_some() {
            declare_on = Some((
                m.object(),
                c.data_event.as_ref().map(|h| h.as_raw().0 as isize),
            ));
        }
    }
    let (object, worker_ev) = declare_on.unwrap_or((None, None));
    match (enable, object, worker_ev) {
        (true, Some(object), Some(ev)) => {
            // Enable declares immediately against the live worker's event (works any time).
            let st = crate::cursor_worker::setup_hardware_cursor(object, ev);
            dbglog!("[pf-vd] cursor: forward flip enable=1 (declare) -> {st:#x}");
            if wdk_iddcx::nt_success(st) {
                registry::mark_declared(target_id);
            }
        }
        (false, _, _) => {
            dbglog!(
                "[pf-vd] cursor: forward flip enable=0 stored — awaiting the host's mode \
                 re-commit (software cursor from then on)"
            );
        }
        _ => dbglog!(
            "[pf-vd] cursor: forward flip enable=1 stored (no live worker — applies at the \
             next channel delivery)"
        ),
    }
    true
}

/// `IOCTL_ADD`: create + arrive a virtual monitor at `width`x`height`@`refresh` for `session_id`,
/// named by `preferred_id` (the host's per-client stable id; `0` = lowest free) and advertising
/// the client display's luminance volume in its EDID (`client_lum`; all-zero = the built-in
/// defaults). Returns `(monitor_id, target_id, adapter_luid_low, adapter_luid_high)` for the
/// [`AddReply`](pf_driver_proto::control::AddReply), or `None` (no adapter yet / IddCx error).
///
/// The entry is registered pending before `IddCxMonitorCreate`, so the mode DDIs the create
/// re-enters find it by id; the handle and then the arrival are filled in write-once. A create
/// failure reclaims the id. An arrival failure must also `WdfObjectDelete` the created object:
/// departure is only valid for an arrived monitor, and a leaked object pins its slot against the
/// adapter's monitor budget. The entry is removed before that delete so a concurrent clear or
/// reap cannot depart the handle being deleted.
pub fn create_monitor(
    session_id: u64,
    width: u32,
    height: u32,
    refresh: u32,
    preferred_id: u32,
    client_lum: pf_driver_proto::edid::ClientLuminance,
    hw_cursor: bool,
) -> Option<(u32, u32, u32, i32)> {
    let adapter = crate::adapter::adapter()?;
    // One identity per session: a re-ADD of a still-live `session_id` departs the stale
    // monitor first, so no duplicate EDID/target lingers.
    if registry::find(|m| m.session_id == session_id).is_some() {
        dbglog!(
            "[pf-vd] create_monitor: session {session_id} already live — departing the stale monitor"
        );
        remove_monitor(session_id);
    }
    let mut modes = vec![Mode {
        width,
        height,
        refresh_rates: vec![refresh],
    }];
    modes.extend(vdisplay::default_modes());
    let monitor = registry::insert(session_id, hw_cursor, preferred_id, modes);
    let id = monitor.id;

    // EDID (serial = id) describes the monitor; the OS calls back into parse_monitor_description.
    // The session's own mode becomes the preferred-timing DTD when it fits the encoding.
    let mut edid = pf_driver_proto::edid::generate(id, client_lum, Some((width, height, refresh)));
    let mut desc = pod_init!(iddcx::IDDCX_MONITOR_DESCRIPTION);
    desc.Size = core::mem::size_of::<iddcx::IDDCX_MONITOR_DESCRIPTION>() as u32;
    desc.Type = iddcx::IDDCX_MONITOR_DESCRIPTION_TYPE::IDDCX_MONITOR_DESCRIPTION_TYPE_EDID;
    desc.DataSize = edid.len() as u32;
    // SAFETY: `edid` is a local array that outlives this `create_monitor` call; IddCxMonitorCreate
    // (below) reads through `pData` SYNCHRONOUSLY, before `edid` drops — the pointer never escapes.
    desc.pData = edid.as_mut_ptr().cast();

    let mut info = pod_init!(iddcx::IDDCX_MONITOR_INFO);
    info.Size = core::mem::size_of::<iddcx::IDDCX_MONITOR_INFO>() as u32;
    info.MonitorContainerId = container_guid(id);
    info.MonitorType =
        wdk_sys::DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY::DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI;
    info.ConnectorIndex = id;
    info.MonitorDescription = desc;

    let mut attr = pod_init!(wdk_sys::WDF_OBJECT_ATTRIBUTES);
    attr.Size = core::mem::size_of::<wdk_sys::WDF_OBJECT_ATTRIBUTES>() as u32;
    attr.ExecutionLevel = wdk_sys::_WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent;
    attr.SynchronizationScope =
        wdk_sys::_WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent;

    let create_in = iddcx::IDARG_IN_MONITORCREATE {
        ObjectAttributes: &raw mut attr,
        pMonitorInfo: &raw mut info,
    };
    let mut create_out = pod_init!(iddcx::IDARG_OUT_MONITORCREATE);
    // SAFETY: adapter is a valid IddCx adapter; create_in points to valid local storage read synchronously.
    let st = unsafe { wdk_iddcx::IddCxMonitorCreate(adapter, &create_in, &mut create_out) };
    dbglog!("[pf-vd] IddCxMonitorCreate(id={id}) -> {st:#x}");
    if !wdk_iddcx::nt_success(st) {
        remove_by_id(id);
        return None;
    }
    let object = create_out.MonitorObject;
    let _ = monitor.object.set(SendMonitor(object));

    // Tell the OS the monitor is plugged in.
    let mut arrival_out = pod_init!(iddcx::IDARG_OUT_MONITORARRIVAL);
    // SAFETY: `object` is the just-created IddCx monitor handle.
    let st = unsafe { wdk_iddcx::IddCxMonitorArrival(object, &mut arrival_out) };
    dbglog!("[pf-vd] IddCxMonitorArrival(id={id}) -> {st:#x}");
    if !wdk_iddcx::nt_success(st) {
        dbglog!(
            "[pf-vd] IddCxMonitorArrival(id={id}) FAILED — reclaiming the id + deleting the created monitor"
        );
        remove_by_id(id);
        // SAFETY: `object` is the just-created (not-yet-arrived) IddCx monitor handle, now owned
        // solely here (its registry entry was just removed); `WdfObjectDelete` takes a `WDFOBJECT`
        // (a raw handle cast, as in the swap-chain / device-cleanup teardowns).
        unsafe {
            call_unsafe_wdf_function_binding!(WdfObjectDelete, object as WDFOBJECT);
        }
        return None;
    }

    let arrival = Arrival {
        target_id: arrival_out.OsTargetId,
        luid_low: arrival_out.OsAdapterLuid.LowPart,
        luid_high: arrival_out.OsAdapterLuid.HighPart,
    };
    // The render flip survives entry churn: start at the session's desired state, stamped
    // before the arrival makes this entry findable by target.
    lock(&monitor.cursor).forward_on = registry::cursor_forward_desired(arrival.target_id);
    let _ = monitor.arrival.set(arrival);
    Some((id, arrival.target_id, arrival.luid_low, arrival.luid_high))
}

/// `IOCTL_UPDATE_MODES` (v4): lead the live monitor's mode list with a new preferred mode and
/// push the target list to the OS via `IddCxMonitorUpdateModes2` — the in-place mid-stream
/// resize. No departure: OS identity, drain worker and ring all survive.
///
/// The list is a union — new mode first, every previously advertised mode kept (deduped, capped
/// at [`vdisplay::MODE_LIST_CAP`]) — because the OS pins a monitor's settable set at arrival, so
/// a replacement could only lose settable modes; the union pays off at the next same-id
/// re-arrival, where the registry's mode history makes every previously used mode settable.
///
/// The stored list changes first, so an OS re-query through the mode DDIs sees the new one, and
/// is reverted if the DDI fails so the two stay coherent. The DDI runs with no lock held: it can
/// re-enter the mode-query callbacks.
pub fn update_monitor_modes(session_id: u64, width: u32, height: u32, refresh: u32) -> NTSTATUS {
    let Some(m) = registry::find(|m| m.session_id == session_id) else {
        return crate::STATUS_NOT_FOUND;
    };
    let Some(object) = m.object() else {
        return crate::STATUS_NOT_FOUND; // created but not yet arrived — nothing to update
    };
    let (old_modes, new_modes) = {
        let mut modes = lock(&m.modes);
        let mut new_modes = vec![Mode {
            width,
            height,
            refresh_rates: vec![refresh],
        }];
        vdisplay::union_modes(&mut new_modes, &modes);
        (
            core::mem::replace(&mut *modes, new_modes.clone()),
            new_modes,
        )
    };

    // The OS's target-mode list for this monitor (the `*2`/HDR shape, like `monitor_query_modes2`).
    let mut targets: Vec<iddcx::IDDCX_TARGET_MODE2> = flatten(&new_modes)
        .map(|item| target_mode2(item.width, item.height, item.refresh_rate))
        .collect();
    let mut in_args = pod_init!(iddcx::IDARG_IN_UPDATEMODES2);
    in_args.Reason = iddcx::IDDCX_UPDATE_REASON::IDDCX_UPDATE_REASON_OTHER;
    in_args.TargetModeCount = targets.len() as u32;
    in_args.pTargetModes = targets.as_mut_ptr();
    // SAFETY: `object` is a live IddCx monitor handle (arrived — checked above; a concurrent REMOVE
    // is serialized by the host, which only ever resizes a monitor its own session holds a lease
    // on). `in_args` points at valid local storage (`targets` outlives the synchronous DDI call).
    let st = unsafe { wdk_iddcx::IddCxMonitorUpdateModes2(object, &in_args) };
    dbglog!(
        "[pf-vd] IddCxMonitorUpdateModes2(session={session_id}, {width}x{height}@{refresh}) -> {st:#x}"
    );
    if !wdk_iddcx::nt_success(st) {
        // Keep the stored list coherent with what the OS actually holds (the old one).
        *lock(&m.modes) = old_modes;
        return st;
    }
    crate::STATUS_SUCCESS
}

/// `IOCTL_REMOVE`: unlink, tear down and depart the monitor for `session_id`, recording its
/// mode list for the next same-id create. Returns true if one was removed.
pub fn remove_monitor(session_id: u64) -> bool {
    let Some(m) = registry::remove_session(session_id) else {
        return false;
    };
    depart(vec![m]);
    true
}

/// `IOCTL_CLEAR_ALL`: tear down and depart every monitor (host-startup orphan reap).
pub fn clear_all() {
    depart(registry::remove(|_| true));
}

/// `EvtCleanupCallback` (device removal, [`crate::callbacks::device_cleanup`]): empty the
/// registry and release every monitor's heavy resources — drain workers, cursor workers, rings,
/// unconsumed deliveries — WITHOUT `IddCxMonitorDeparture`: the framework tears the IddCx
/// monitors down with the departing device, and departing here would double-tear.
pub fn cleanup_for_device_removal() {
    for m in registry::remove(|_| true) {
        m.teardown();
    }
}

/// Drop a pending entry by id (create failed before arrival). A pending entry normally holds
/// nothing, but a concurrent delivery may already have stocked it, so it is torn down anyway.
fn remove_by_id(id: u32) {
    for m in registry::remove(|m| m.id == id) {
        m.teardown();
    }
}

/// Rebuild [`vdisplay::container_guid`]'s fields as the OS `GUID` — the container id that groups a
/// monitor's targets into one physical device. `pf_driver_proto` is `no_std` and has no `GUID` type,
/// so it hands back the four fields.
fn container_guid(id: u32) -> wdk_sys::GUID {
    let (d1, d2, d3, d4) = vdisplay::container_guid(id);
    wdk_sys::GUID {
        Data1: d1,
        Data2: d2,
        Data3: d3,
        Data4: d4,
    }
}
