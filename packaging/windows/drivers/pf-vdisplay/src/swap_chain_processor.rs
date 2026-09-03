//! The swap-chain processor (STEP 5 + STEP 6): a worker thread that DRAINS the IddCx swap-chain (so the
//! virtual monitor stays a usable display) and PUBLISHES each acquired surface into the host-created
//! shared ring (the IDD-push path).
//!
//! The OS presents the composited desktop to the driver through a swap-chain; the driver MUST consume it
//! (acquire → finished-processing) or the monitor stalls. STEP 5 binds our render device to the swap-chain
//! (`IddCxSwapChainSetDevice`) and loops acquire/finish. STEP 6 lazily attaches a [`FramePublisher`] to
//! the host's shared ring and, on each acquired frame, `CopyResource`s `out.MetaData.pSurface` into the
//! next ring slot before finishing the frame (a non-IDD-push session simply never attaches and keeps
//! draining). Frames the ring can NOT take feed the [`FrameStash`] instead, which every fresh attach
//! republishes as its instant first frame — the first-frame guarantee that makes a session opened onto
//! an IDLE desktop show a picture without waiting for anything to dirty the display.
//!
//! Ported from the proven oracle (`packaging/windows/vdisplay-driver/pf-vdisplay/src/
//! swap_chain_processor.rs`) onto wdk-sys + wdk-iddcx. The oracle's `wdf_umdf`/`wdf_umdf_sys` are
//! replaced by `wdk_sys::iddcx::*` + the `wdk_iddcx` DDI wrappers. Those wrappers return a RAW
//! `NTSTATUS` (`i32`) that is HRESULT-shaped for the swap-chain DDIs, so we classify it by hand
//! (`hr >= 0` = success; `0x8000_000A` = E_PENDING; `hr < 0 && != E_PENDING` = error) rather than with
//! `nt_success`.

use std::{
    mem::size_of,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

/// One tick per swap-chain assignment (`run_core` entry), process-wide — the v3 header's
/// `assignment_epoch`. Distinct per assignment is all the host needs; it never compares epochs
/// across monitors.
static ASSIGNMENT_EPOCH: AtomicU32 = AtomicU32::new(0);

use wdk_sys::iddcx::{
    IDARG_IN_RELEASEANDACQUIREBUFFER2, IDARG_IN_SETREALTIMEGPUPRIORITY,
    IDARG_IN_SWAPCHAINSETDEVICE, IDARG_OUT_RELEASEANDACQUIREBUFFER2, IDDCX_SWAPCHAIN,
};
// `HANDLE` is the shared wdk-sys typedef (`crate::types`) re-used by the iddcx bindings — take it from
// the crate root, which is guaranteed to export it (the iddcx module only re-exports it if bindgen
// re-declared it there). It is the same type as `IDARG_IN_SETSWAPCHAIN.hNextSurfaceAvailable`.
use wdk_sys::{HANDLE, NTSTATUS, WDFOBJECT, call_unsafe_wdf_function_binding};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE as WHANDLE, LUID, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Graphics::{
            Direct3D11::ID3D11Texture2D,
            Dxgi::{IDXGIDevice, IDXGIResource},
        },
        System::Threading::{
            AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
            GetCurrentThread, SetEvent, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
            WaitForMultipleObjects, WaitForSingleObject,
        },
    },
    core::{Interface, w},
};

use crate::{
    direct_3d_device::Direct3DDevice,
    frame_transport::{FramePublisher, FrameStash, PublishOutcome, RingEndpoint},
    monitor::Monitor,
    worker::Sendable,
};

/// E_PENDING — `ReleaseAndAcquireBuffer2` returns this (HRESULT-shaped) when the swap-chain is valid but
/// DWM has composed no new frame yet; wait on the surface-available event and retry.
const E_PENDING: u32 = 0x8000_000A;
/// Idle-wait timeout. Deliveries and stops arrive on the wake event, so this exists ONLY to keep
/// the drain heartbeat (`FramePublisher::note_drain`) ticking over a desktop that composes
/// nothing: the host convicts this worker when it reads that stamp older than `max(gap/2, 250 ms)`
/// (`pf-capture` stall attribution) or 2 s (`pf_frame::health`, which then runs its recovery
/// ladder). An INFINITE wait here would report every idle desktop as a stalled worker.
const IDLE_WAIT_MS: u32 = 125;

/// HRESULT-shaped success test for the swap-chain DDIs (raw `NTSTATUS`/HRESULT: success iff non-negative).
#[inline]
fn hr_success(hr: NTSTATUS) -> bool {
    hr >= 0
}

/// Whether the swap-chain processing device's GPU scheduling is raised to REALTIME (the IddCx
/// 1.9 `IddCxSetRealtimeGPUPriority` DDI — "higher priority than any regular application can
/// set"). Default **ON**: minimum latency at every layer — a GPU-saturating game must not starve
/// the leg that feeds every captured frame into the ring. The 2026-08 default-OFF (after an
/// RX 9070 XT field A/B blamed this raise for a metronomic ~1.8 s capture-stall class) at most
/// masked that still-unattributed stall — confirmed cases kept arriving with the raise off —
/// while regressing loaded NVIDIA boxes into feed starvation, so it was reverted. The per-box
/// A/B escape hatch remains: `setx /M PFVD_NO_RT_GPU 1` (any value) + a device restart disables
/// the raise — read via [`crate::log::knob`] (process environment, then the machine one, because WUDFHost's
/// own environment is stale until a reboot. The old `PFVD_RT_GPU` opt-in ladder
/// (off/thread/realtime) is gone; a stale `PFVD_RT_GPU` now just matches the default.
fn rt_gpu_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| crate::log::knob("PFVD_NO_RT_GPU").is_none())
}

pub struct SwapChainProcessor {
    terminate: Arc<AtomicBool>,
    /// AUTO-reset event that releases the worker's idle wait: a frame-channel delivery or a stop
    /// reaches it at once instead of waiting out [`IDLE_WAIT_MS`]. `None` when the event could not
    /// be created — the worker then only has its timeout. Closed by `Drop` AFTER the worker is
    /// joined, so [`Self::wake`] can never signal a closed handle.
    wake: Option<WHANDLE>,
    thread: Option<JoinHandle<()>>,
}

// SAFETY: Raw ptr is managed by external library; access is serialised by the worker thread + the
// terminate flag.
unsafe impl Send for SwapChainProcessor {}
// SAFETY: as above — the raw pointer is only touched by the serialised worker, so a shared
// `&SwapChainProcessor` reference exposes no unsynchronised access.
unsafe impl Sync for SwapChainProcessor {}

impl SwapChainProcessor {
    pub fn new() -> Self {
        // SAFETY: plain event creation — auto-reset, unsignalled, unnamed, no security descriptor.
        let wake = unsafe { CreateEventW(None, false, false, None) }.ok();
        if wake.is_none() {
            dbglog!("[pf-vd] swap-chain: wake event creation failed — timeout-only idle wait");
        }
        Self {
            terminate: Arc::new(AtomicBool::new(false)),
            wake,
            thread: None,
        }
    }

    /// Release the worker's idle wait so it re-runs the loop top — after a frame-channel delivery
    /// lands, and from `Drop`. `SetEvent` never blocks, so a caller may hold the monitor's
    /// `swap` guard across it. No-op when the event could not be created (the worker polls).
    pub fn wake(&self) {
        if let Some(h) = self.wake {
            // SAFETY: `h` is our own event handle; `Drop` closes it only after joining the worker.
            let _ = unsafe { SetEvent(h) };
        }
    }

    /// Spawn the drain worker for a freshly assigned swap-chain. It runs at MMCSS `Distribution`
    /// priority (TIME_CRITICAL if MMCSS declines), owns `swap_chain` for its lifetime and deletes
    /// that object before returning. `monitor` is the weak link to its owner: the worker never
    /// keeps the monitor alive, and the owner's teardown joins it. Its idle wait covers
    /// `available_buffer_event` (the framework's surface-available event) AND this processor's
    /// wake event, so a delivery or a stop reaches an idle display immediately.
    pub fn run(
        &mut self,
        swap_chain: IDDCX_SWAPCHAIN,
        device: Arc<Direct3DDevice>,
        available_buffer_event: HANDLE,
        monitor: Weak<Monitor>,
        render_luid: LUID,
    ) {
        let events = Sendable((self.wake, available_buffer_event));
        let swap_chain = Sendable(swap_chain);
        let terminate = self.terminate.clone();
        // For the log lines and the ring binding check: 0 for a monitor the registry does not
        // hold, whose worker only drains.
        let target_id = monitor.upgrade().map_or(0, |m| m.target_id());

        let join_handle = thread::spawn(move || {
            // Rust 2021 disjoint closure captures would otherwise grab the raw `swap_chain.0` /
            // `events.0` FIELDS directly (defeating the `Sendable` Send wrapper, since the inner
            // `*mut IDDCX_SWAPCHAIN__` / `HANDLE` are `!Send`). Rebind the WHOLE wrappers here so the
            // closure captures them as `Sendable<_>` (which IS `Send`), then unwrap from the locals.
            let swap_chain = swap_chain;
            let events = events;
            // It is very important to prioritize this thread by making use of the Multimedia Scheduler
            // Service. It will intelligently prioritize the thread for improved throughput in high
            // CPU-load scenarios.
            let mut av_task = 0u32;
            // SAFETY: `w!("Distribution")` is a 'static null-terminated UTF-16 task name; `av_task` is a
            // valid local out-param. The returned handle is reverted with AvRevertMmThreadCharacteristics.
            let res = unsafe { AvSetMmThreadCharacteristicsW(w!("Distribution"), &mut av_task) };
            // MMCSS can fail under the restricted WUDFHost token ('Distribution' task unregistered /
            // service unavailable). The MS sample CONTINUES unprioritized — never abort: returning
            // here would leave the assigned swap-chain undrained (the monitor stalls, DWM blocks on
            // it) and leak the WDF swap-chain object until device teardown. But "unprioritized" is
            // not acceptable either: this thread is the whole display's frame pump, and at normal
            // priority a display-stack disturbance (DDC/HPD servicing DPC pressure, poller-software
            // storms) can starve it into multi-hundred-ms delivery holes. Fall back to
            // TIME_CRITICAL — the highest band available without the realtime priority class, and
            // the closest to what MMCSS 'Distribution' would have granted. The thread spends its
            // life blocked on the surface-available event / keyed mutex, so it cannot starve others.
            let av_handle = match res {
                Ok(h) => Some(h),
                Err(e) => {
                    // SAFETY: plain FFI; GetCurrentThread returns a pseudo-handle (never fails,
                    // nothing to close), SetThreadPriority on it affects only this thread.
                    let fallback = unsafe {
                        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL)
                    };
                    dbglog!(
                        "[pf-vd] swap-chain: MMCSS prioritization failed ({e:?}) — fell back to \
                         TIME_CRITICAL thread priority (ok={})",
                        fallback.is_ok()
                    );
                    None
                }
            };

            Self::run_core(
                swap_chain.0,
                &device,
                events.0,
                &terminate,
                &monitor,
                target_id,
                render_luid,
            );

            dbglog!(
                "[pf-vd] swap-chain run_core RETURNED (target={target_id}) — deleting swap-chain, device drops next"
            );

            // Delete the swap-chain WDF object BEFORE the `Arc<Direct3DDevice>` drops (the swap-chain
            // referenced our device). `WdfObjectDelete` takes a WDFOBJECT.
            // SAFETY: `swap_chain` is a live IddCx swap-chain handle; we own the sole reference here and
            // the drain loop has exited.
            unsafe {
                call_unsafe_wdf_function_binding!(WdfObjectDelete, swap_chain.0 as WDFOBJECT);
            }

            // Revert the thread to normal once it's done (only if MMCSS was actually engaged).
            if let Some(h) = av_handle {
                // SAFETY: `h` is the live characteristics handle returned by
                // AvSetMmThreadCharacteristicsW above, reverted exactly once here at thread exit.
                let res = unsafe { AvRevertMmThreadCharacteristics(h) };
                if let Err(e) = res {
                    dbglog!("[pf-vd] swap-chain: failed to revert prioritized thread: {e:?}");
                }
            }
        });

        self.thread = Some(join_handle);
    }

    /// The drain loop. It upgrades `monitor` once per pass for the delivery gate and releases
    /// the strong count before it blocks, so the owner's teardown — which joins this thread
    /// before the last `Arc` goes — is the only thing that ends the monitor. A failed upgrade
    /// means this worker was never installed on a live monitor; it exits and the epilogue
    /// deletes the swap-chain.
    fn run_core(
        swap_chain: IDDCX_SWAPCHAIN,
        device: &Direct3DDevice,
        // (this processor's wake event — `None` if it could not be created, the framework's
        // surface-available event); one parameter so the argument count stays under the lint.
        events: (Option<WHANDLE>, HANDLE),
        terminate: &AtomicBool,
        monitor: &Weak<Monitor>,
        target_id: u32,
        render_luid: LUID,
    ) {
        let (wake, available_buffer_event) = events;
        let assignment_epoch = ASSIGNMENT_EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
        // `as_raw()` BORROWS our single reference — IddCx AddRefs its own — and it is released right
        // after the realtime raise below. An `into_raw()` here would orphan that reference and pin the
        // D3D device (its worker threads and VRAM) past the processor's drop.
        let dxgi_device = match device.device.cast::<IDXGIDevice>() {
            Ok(d) => d,
            Err(e) => {
                dbglog!("[pf-vd] swap-chain: failed to cast ID3D11Device to IDXGIDevice: {e:?}");
                return;
            }
        };
        // Built zeroed + field-assigned (driver style) — robust against a bindgen field-set difference.
        let mut set_device = pod_init!(IDARG_IN_SWAPCHAINSETDEVICE);
        set_device.pDevice = dxgi_device.as_raw().cast();
        // One shot: a failure here means the OS already unassigned this swap-chain, and
        // DXGI_ERROR_ACCESS_LOST on that handle never recovers. Returning lets the thread epilogue
        // delete it so the OS mints a fresh one — the reassign is what succeeds.
        // SAFETY: driver is loaded; `swap_chain` is valid; `set_device` points to valid local storage.
        let hr = unsafe { wdk_iddcx::IddCxSwapChainSetDevice(swap_chain, &set_device) };
        if !hr_success(hr) {
            dbglog!(
                "[pf-vd] swap-chain run_core: SetDevice failed ({hr:#x}, target={target_id}) — returning for a fresh swap-chain"
            );
            drop(dxgi_device);
            return;
        }
        dbglog!(
            "[pf-vd] swap-chain run_core: SetDevice OK (target={target_id}) — entering drain loop"
        );
        // GPU-scheduling raise for the swap-chain processing device — default ON, so the leg
        // feeding every captured frame into the ring outranks a GPU-saturating game (history +
        // `PFVD_NO_RT_GPU` escape hatch: [`rt_gpu_enabled`]). Best-effort, never fatal, issued
        // while our borrowed device reference is still alive (IddCx uses it synchronously); the
        // DDI may still decline (e.g. E_NOTIMPL on pre-WDDM-3.0 hardware).
        if rt_gpu_enabled() {
            let mut rt = pod_init!(IDARG_IN_SETREALTIMEGPUPRIORITY);
            rt.pDevice = dxgi_device.as_raw().cast();
            // SAFETY: driver is loaded; `swap_chain` is the live assigned swap-chain whose
            // device bind just succeeded; `rt.pDevice` is that same bound DXGI device,
            // alive across the synchronous call; `rt` points to valid local storage.
            let hr = unsafe { wdk_iddcx::IddCxSetRealtimeGPUPriority(swap_chain, &rt) };
            if hr_success(hr) {
                dbglog!(
                    "[pf-vd] swap-chain: processing device raised to REALTIME GPU priority (default; PFVD_NO_RT_GPU disables) (target={target_id})"
                );
            } else {
                dbglog!(
                    "[pf-vd] swap-chain: realtime GPU priority declined ({hr:#x}) — normal scheduling (target={target_id})"
                );
            }
        }
        // Release our borrowed device reference — IddCx holds its own now. (Explicit drop so NLL
        // can't release it mid-loop while the swap-chain still references the raw ptr.)
        drop(dxgi_device);

        // STEP 6 IDD-push: publish into the HOST-created shared ring over the SEALED channel (the
        // control plane stashes the delivered handle values on our monitor). Until a delivery lands
        // we just drain — exactly the STEP-5 behaviour — so a non-IDD-push session never stalls.
        let open = |ep: Arc<RingEndpoint>| {
            FramePublisher::open(
                ep,
                render_luid.LowPart,
                render_luid.HighPart,
                &device.device,
                &device.device_context,
                assignment_epoch,
                device.epoch(),
            )
        };
        // The MONITOR owns the ring (D4 / WP5): every worker opens its OWN device-bound publisher
        // on the monitor's endpoint, so a swap-chain flap (a SIBLING display churning the topology)
        // resumes the same host ring without any COM object crossing device epochs — a TDR
        // recreate or adapter move just makes the open fail, reported in the header.
        let mut publisher: Option<FramePublisher> = monitor
            .upgrade()
            .and_then(|m| m.endpoint())
            .and_then(|ep| match open(ep) {
                Ok(p) => {
                    dbglog!(
                        "[pf-vd] swap-chain run_core: re-opened the monitor's ring endpoint (target={target_id}) — resuming across the swap-chain flap"
                    );
                    Some(p)
                }
                Err(e) => {
                    dbglog!(
                        "[pf-vd] swap-chain run_core: re-open of the ring endpoint failed ({e:?}, target={target_id}) — waiting for a fresh delivery"
                    );
                    None
                }
            });
        // The FIRST-FRAME stash (see `FrameStash`): the retained last composed frame, republished
        // into every fresh ring at attach so a session opening onto an idle desktop is never black.
        // Worker-local (D4): its texture lives on THIS device, so it never crosses an assignment —
        // a reassigned worker starts empty and the first compose refills it.
        let mut stash = FrameStash::new();

        let mut logged_pending = false;
        let mut logged_frame = false;
        // The frame-channel delivery gate (see `Monitor::chan_gen`): the loop only locks the
        // channel slot when a delivery LANDED since it last looked. Seeded one behind the
        // current generation so a delivery that arrived before this worker started (host
        // delivered ahead of the swap-chain assign) is checked on the very first pass.
        let mut seen_chan_gen = monitor
            .upgrade()
            .map_or(0, |m| m.chan_gen.load(Ordering::Acquire))
            .wrapping_sub(1);
        loop {
            // Check terminate at the TOP, every iteration. The success branch below does NOT re-check it,
            // so during a CONTINUOUS frame burst (DWM rendering the freshly-activated desktop) a thread the
            // OS unassigns — or that the processor is dropping — never sees the flag and loops on, pinning
            // its D3D device (and ~36 NVIDIA worker threads). That is THE reconnect leak; it only
            // reproduced at full speed (E_PENDING gaps DO check terminate and masked it under a debugger).
            // Without this, `SwapChainProcessor::drop`'s join can also block until the burst ends.
            if terminate.load(Ordering::Relaxed) {
                break;
            }
            // A sibling worker (or the pool at checkout) saw this device REMOVED: every worker on
            // the entry stops — the swap-chain would fail on it anyway, and the OS reassigns onto a
            // fresh device (immunity plan WP5 item 6).
            if device.is_removed() {
                dbglog!(
                    "[pf-vd] swap-chain run_core: device epoch {} removed (target={target_id}) — exiting for reassignment",
                    device.epoch()
                );
                break;
            }

            let Some(owner) = monitor.upgrade() else {
                dbglog!("[pf-vd] swap-chain run_core: monitor gone (target={target_id}) — exiting");
                break;
            };
            // The delivery gate: `chan_pending` is true only when a delivery landed since the
            // last pass, so the two slot-locking checks below (`has_frame_channel`,
            // `take_frame_channel`) run only then and the steady state locks nothing.
            let chan_gen = owner.chan_gen.load(Ordering::Acquire);
            let chan_pending = chan_gen != seen_chan_gen;
            // Re-attach triggers: `is_stale` (the host recreated the ring mid-session — HDR flip —
            // and bumped OUR header's generation; publishing on would mismatch every frame and
            // freeze the stream), or a PENDING delivery (newest-wins: a host build-retry makes a
            // whole NEW ring with a different header mapping, which `is_stale` can never see; the
            // host only delivers after fully creating a ring, so a pending delivery supersedes).
            if publisher.as_ref().is_some_and(FramePublisher::is_stale)
                || (publisher.is_some() && chan_pending && owner.has_frame_channel())
            {
                // Harvest the superseded ring's last-published frame into the stash BEFORE dropping
                // the publisher: between sessions the driver keeps publishing into the (host-side
                // dead) previous ring, so that slot holds the CURRENT desktop image — exactly what
                // the new ring's attach below republishes as its instant first frame.
                if let Some(p) = publisher.take() {
                    // v3: tell the host this generation is being superseded, so a quiet ring reads
                    // REBUILDING rather than stalled (the fresh attach below marks ACTIVE again).
                    p.endpoint().mark_rebuilding();
                    p.harvest_into(&device.device, &mut stash);
                }
            }
            // Lazy-attach at the loop TOP so we keep trying while the display is idle (E_PENDING),
            // gated by `chan_pending` — attach latency is first-frame latency, since the attach
            // republishes the stash. A taken delivery is consumed whether it succeeds or not (its
            // handles close with the endpoint; the host reads the status code; a retry is a NEW
            // delivery). `from_channel` refuses a ring that does not name THIS monitor.
            if publisher.is_none()
                && chan_pending
                && let Some(channel) = owner.take_frame_channel()
            {
                let source_seq = owner.source_seq.clone();
                if let Ok(ep) = RingEndpoint::from_channel(channel, target_id, source_seq) {
                    let ep = Arc::new(ep);
                    // Install BEFORE opening: the endpoint is the monitor's whatever this worker's
                    // device makes of it.
                    owner.set_endpoint(ep.clone());
                    match open(ep.clone()) {
                        Ok(mut p) => {
                            // FIRST-FRAME GUARANTEE: republish the retained desktop image into the
                            // fresh ring immediately — on an idle desktop DWM composes nothing, so
                            // the host would otherwise wait (and kick synthetic input) for a frame
                            // that may never come. A stale-descriptor stash (pre-HDR-flip) is
                            // rejected by publish()'s guard: at worst the old wait-for-compose path.
                            if let Some(t) = stash.texture()
                                && p.publish(t, 0) == PublishOutcome::Published
                            {
                                dbglog!(
                                    "[pf-vd] frame-push(driver): republished the retained frame into the fresh ring (target={target_id}) — instant first frame, no compose needed"
                                );
                            }
                            publisher = Some(p);
                        }
                        Err(e) => {
                            // Terminal for THIS delivery (pre-WP5 semantics): the host reads the
                            // status and fails the open; a retry is a new delivery.
                            dbglog!(
                                "[pf-vd] frame-push(driver): open of the fresh ring failed ({e:?}, target={target_id}) — retiring the endpoint"
                            );
                            owner.clear_endpoint(ep.generation());
                        }
                    }
                }
            }
            // The pending generation was serviced above — whichever branch ran, a lock-taking
            // check happened (`has_frame_channel` and/or `take_frame_channel`), so this pass has
            // seen everything up to `chan_gen`. A delivery racing in between bumps past it and
            // re-arms the gate on the next pass.
            if chan_pending {
                seen_chan_gen = chan_gen;
            }
            // Blocking from here on: hand the strong count back so this thread never decides
            // when its own monitor drops.
            drop(owner);

            // ...Buffer2 is required once CAN_PROCESS_FP16 is set. AcquireSystemMemoryBuffer=FALSE keeps
            // the GPU surface (out.MetaData.pSurface) — STEP 6 publishes it into the shared ring in the
            // success branch below. Built zeroed + field-assigned (driver style) so a bindgen field-set
            // difference can't break a positional struct literal.
            let mut in_args = pod_init!(IDARG_IN_RELEASEANDACQUIREBUFFER2);
            #[allow(clippy::cast_possible_truncation)]
            {
                in_args.Size = size_of::<IDARG_IN_RELEASEANDACQUIREBUFFER2>() as u32;
            }
            in_args.AcquireSystemMemoryBuffer = 0;
            // `pod_init!` (zeroed, not `::default()`) — consistent with every other IddCx out-struct
            // in this driver, and robust whether or not bindgen derives `Default` for this type (its
            // `MetaData` field carries a raw `pSurface` pointer + union which can suppress the derive).
            let mut buffer = pod_init!(IDARG_OUT_RELEASEANDACQUIREBUFFER2);
            // SAFETY: driver is loaded; `swap_chain` is valid; in/out point to valid local storage.
            let hr: NTSTATUS = unsafe {
                wdk_iddcx::IddCxSwapChainReleaseAndAcquireBuffer2(
                    swap_chain,
                    &mut in_args,
                    &mut buffer,
                )
            };

            if (hr as u32) == E_PENDING {
                // Nothing composed: heartbeat only, stamped before the wait (see `note_drain`).
                if let Some(p) = publisher.as_ref() {
                    p.note_drain(false);
                }
                if !logged_pending {
                    dbglog!(
                        "[pf-vd] swap-chain run_core: E_PENDING (target={target_id}) — swap-chain valid but DWM has composed NO frame yet"
                    );
                    logged_pending = true;
                }
                let surface = WHANDLE(available_buffer_event.cast());
                // SAFETY: `surface` is the framework-provided surface-available event, live for
                // this assignment; `w` is this processor's own wake event, which it closes only
                // after joining this thread.
                let waited = unsafe {
                    match wake {
                        Some(w) => WaitForMultipleObjects(&[w, surface], false, IDLE_WAIT_MS),
                        None => WaitForSingleObject(surface, IDLE_WAIT_MS),
                    }
                };
                // Wake, surface-available or the heartbeat timeout all just re-run the loop top,
                // which re-checks terminate, device removal and the delivery gate. The wake event
                // is auto-reset with ONE waiter, so a `SetEvent` raised while this thread is not
                // waiting stays latched until its next wait — no wakeup is lost.
                if waited == WAIT_OBJECT_0
                    || waited == WAIT_TIMEOUT
                    || waited.0 == WAIT_OBJECT_0.0 + 1
                {
                    continue;
                }
                // The wait was cancelled or something unexpected happened.
                dbglog!(
                    "[pf-vd] swap-chain run_core: idle wait -> {:#x} (target={target_id}) — exiting",
                    waited.0
                );
                break;
            } else if hr_success(hr) {
                // The OS's display time for this frame — the provenance stamp the publish carries
                // into the ring record.
                let display_qpc = buffer.MetaData.PresentDisplayQPCTime;
                if !logged_frame {
                    dbglog!(
                        "[pf-vd] swap-chain run_core: FIRST FRAME acquired (target={target_id}) — DWM IS compositing the virtual display!"
                    );
                    logged_frame = true;
                }
                // STEP 6: copy the acquired surface into the shared ring BEFORE FinishedProcessingFrame
                // (the surface is valid until the next ReleaseAndAcquire). Every successful acquire
                // TRANSFERS one surface reference to the driver — the MS sample `Attach`es it into a
                // ComPtr and `Reset`s BEFORE FinishedProcessingFrame, warning that a driver which
                // "forgets to release the reference" leaves the surfaces alive after the swap-chain is
                // destroyed. Holding it (the old `from_raw_borrowed`) leaked the swap-chain's whole
                // surface set per assign/unassign cycle (reconnect, mode change, HDR flip) — so adopt
                // the reference UNCONDITIONALLY (publisher or not); it is released when `res` drops at
                // the end of this block. (Publisher attach happens at the loop top.)
                {
                    let raw = buffer.MetaData.pSurface as *mut core::ffi::c_void;
                    if !raw.is_null() {
                        // SAFETY: `raw` is the live surface IddCx just handed us, carrying the acquire's
                        // transferred reference; `from_raw` adopts exactly that reference (released on
                        // drop, below — the queued GPU copy is unaffected: D3D defers destruction, and
                        // the copy is ordered before the consumer via the slot keyed mutex).
                        let res = unsafe { IDXGIResource::from_raw(raw) };
                        if let Ok(tex) = res.cast::<ID3D11Texture2D>() {
                            match publisher.as_mut().map(|p| p.publish(&tex, display_qpc)) {
                                // Ring took it (or the host is alive and busy) — nothing to retain.
                                Some(
                                    PublishOutcome::Published
                                    | PublishOutcome::AllSlotsBusy
                                    | PublishOutcome::Dropped,
                                ) => {}
                                // No ring, or the surface's descriptor doesn't match it (a mode-set /
                                // HDR flip racing the host's ring recreate): RETAIN the frame — it is
                                // the desktop image the next attach republishes as its first frame.
                                // Unattached/mismatched composes are damage-driven and transient, so
                                // the extra copy costs nothing at steady state.
                                Some(PublishOutcome::DescMismatch) | None => {
                                    stash.store(
                                        &device.device,
                                        &device.device_context,
                                        &tex,
                                        Instant::now(),
                                    );
                                }
                                // Poisoned generation (host died holding a slot, a failed release,
                                // or a fatal device HRESULT): stop using this publisher — the next
                                // channel delivery attaches a fresh ring. Retain the frame so that
                                // attach has a first image; publish() already logged the cause.
                                Some(PublishOutcome::HostAbandoned | PublishOutcome::Fatal) => {
                                    stash.store(
                                        &device.device,
                                        &device.device_context,
                                        &tex,
                                        Instant::now(),
                                    );
                                    publisher = None;
                                    // A device-removal fatal poisons the DEVICE, not just the
                                    // ring: flag the pool entry so every worker on it stops and
                                    // the next assignment gets a fresh device (WP5 item 6).
                                    // SAFETY: plain status query on the worker's live device.
                                    if unsafe { device.device.GetDeviceRemovedReason() }.is_err() {
                                        device.mark_removed();
                                    }
                                }
                            }
                        }
                        // `res` drops here → the acquire's surface reference is released, pre-Finished.
                    }
                }

                // SAFETY: driver is loaded; `swap_chain` is valid.
                let hr = unsafe { wdk_iddcx::IddCxSwapChainFinishedProcessingFrame(swap_chain) };
                if !hr_success(hr) {
                    break;
                }
                // Stamped only now: nothing may sit between acquire and Finished (see `note_drain`).
                if let Some(p) = publisher.as_ref() {
                    p.note_drain(true);
                }
            } else {
                // The swap-chain was likely abandoned (e.g. DXGI_ERROR_ACCESS_LOST) — exit the loop.
                break;
            }
        }

        // Worker exit (the OS unassigned this swap-chain — typically a SIBLING display churned the
        // topology — or it errored): the endpoint stays on the MONITOR for the next worker to
        // re-open on its own device; the opened slots and the stash drop here. Tell the host the
        // quiet ring is REBUILDING, not stalled. If the monitor is gone, this worker's `Arc` was
        // the endpoint's last holder and dropping it closes the ring handles — no leak.
        if let Some(p) = publisher.take() {
            p.endpoint().mark_rebuilding();
            let before = crate::frame_transport::handle_count();
            drop(p);
            dbglog!(
                "[pf-vd] hcount: publisher dropped before={before} after={} (target={target_id})",
                crate::frame_transport::handle_count()
            );
        }
        drop(stash);
    }
}

impl Drop for SwapChainProcessor {
    fn drop(&mut self) {
        if let Some(handle) = self.thread.take() {
            // Store the flag BEFORE waking: the worker re-checks it at the loop top. Without the
            // wake, an idle worker would sit out its whole timeout before seeing the flag.
            self.terminate.store(true, Ordering::Relaxed);
            self.wake();
            // The worker deletes the swap-chain object before returning.
            let _ = handle.join();
        }
        if let Some(h) = self.wake.take() {
            // SAFETY: the worker has been joined (or never started), so nothing can signal `h` any
            // more; this is its sole close.
            let _ = unsafe { CloseHandle(h) };
        }
    }
}
