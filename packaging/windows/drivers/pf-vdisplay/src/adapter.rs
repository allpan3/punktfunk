//! IddCx adapter bring-up. Adapter creation is DEFERRED to the first `EvtDeviceD0Entry` (the adapter
//! object is only valid after D0), and is ASYNC: `init_adapter` builds the caps and calls
//! `IddCxAdapterInitAsync`; the adapter object arrives later via `EvtIddCxAdapterInitFinished`
//! (`adapter_init_finished` → [`set_adapter`]). FP16 caps + the obligated `*2`/gamma/hdr callbacks (in
//! `callbacks.rs`) together enable HDR. STEP 3.

use std::sync::Mutex;

use wdk_sys::{NTSTATUS, WDFDEVICE, iddcx};
use windows::core::w;

use crate::STATUS_SUCCESS;

/// The IddCx adapter handle, stashed for later DDIs (e.g. `SET_RENDER_ADAPTER`, STEP 4).
struct SendAdapter(iddcx::IDDCX_ADAPTER);
// SAFETY: an opaque IddCx handle, used only as an argument to IddCx DDIs (themselves the synchronisation
// point) — never dereferenced in Rust. Storing it across threads in a OnceLock is sound.
unsafe impl Send for SendAdapter {}
// SAFETY: as above — the handle is only ever passed by value to IddCx DDIs, never dereferenced, so
// shared `&SendAdapter` access across threads is sound.
unsafe impl Sync for SendAdapter {}

// A slot, NOT a OnceLock: `set_adapter` must be last-write-wins so a D0-resume re-init's fresh
// handle REPLACES the pre-power-cycle one (a OnceLock's second `set` was a silent no-op, leaving
// every later `IddCxMonitorCreate` pointed at a stale adapter). Poison-recovering lock idiom as
// in `monitor.rs` (panic = abort here, so poisoning is unreachable anyway).
static ADAPTER: Mutex<Option<SendAdapter>> = Mutex::new(None);

/// Build the adapter caps (FP16/HDR-capable) and kick off the async adapter creation. Called from
/// `EvtDeviceD0Entry`; idempotent across re-entrant D0 transitions.
pub fn init_adapter(device: WDFDEVICE) -> NTSTATUS {
    if adapter().is_some() {
        return STATUS_SUCCESS;
    }
    dbglog!("[pf-vd] init_adapter");

    // Firmware/hardware version (telemetry). The oracle points BOTH at one IDDCX_ENDPOINT_VERSION.
    // `version` is a stack local read synchronously by IddCxAdapterInitAsync (same as the oracle). `.Size`
    // is `size_of` throughout — these are the IddCx 1.10 structs and the framework here is 1.10 (= upstream).
    let mut version = pod_init!(iddcx::IDDCX_ENDPOINT_VERSION);
    version.Size = core::mem::size_of::<iddcx::IDDCX_ENDPOINT_VERSION>() as u32;
    version.MajorVer = env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap_or(0);
    version.MinorVer = env!("CARGO_PKG_VERSION_MINOR").parse().unwrap_or(0);
    version.Build = env!("CARGO_PKG_VERSION_PATCH").parse().unwrap_or(0);

    // Endpoint diagnostics. `pEndPointModelName` must be a non-empty string, and `w!` is what keeps
    // the three name pointers 'static — IddCx reads them after this frame. GammaSupport MUST be set:
    // a zeroed value is IDDCX_FEATURE_IMPLEMENTATION_UNINITIALIZED (0), which the framework's adapter
    // Validate rejects with INVALID_PARAMETER — set it to NONE (1) like upstream.
    let mut diag = pod_init!(iddcx::IDDCX_ENDPOINT_DIAGNOSTIC_INFO);
    diag.Size = core::mem::size_of::<iddcx::IDDCX_ENDPOINT_DIAGNOSTIC_INFO>() as u32;
    diag.GammaSupport = iddcx::IDDCX_FEATURE_IMPLEMENTATION::IDDCX_FEATURE_IMPLEMENTATION_NONE;
    diag.TransmissionType = iddcx::IDDCX_TRANSMISSION_TYPE::IDDCX_TRANSMISSION_TYPE_WIRED_OTHER;
    diag.pEndPointFriendlyName = w!("Punktfunk Virtual Display Adapter").as_ptr();
    diag.pEndPointManufacturerName = w!("Punktfunk").as_ptr();
    diag.pEndPointModelName = w!("Virtual Display").as_ptr();
    // SAFETY: `version` is a stack local that outlives this `init_adapter` call; IddCxAdapterInitAsync
    // (below) reads through these pointers SYNCHRONOUSLY, before `version` drops — the pointer never escapes.
    diag.pFirmwareVersion = (&raw mut version).cast();
    diag.pHardwareVersion = (&raw mut version).cast();

    let mut caps = pod_init!(iddcx::IDDCX_ADAPTER_CAPS);
    caps.Size = core::mem::size_of::<iddcx::IDDCX_ADAPTER_CAPS>() as u32;
    // STEP 7 (HDR): declare we can process FP16 (scRGB) desktop surfaces — this is what marks the virtual
    // monitor advanced-color-capable (→ the host sees display_hdr=true → the "Use HDR" toggle appears). The
    // ONLY reason STEP 3 rejected this flag was setting it WITHOUT the obligated *2/HDR DDIs; those are now
    // registered in entry.rs (parse_monitor_description2/monitor_query_modes2/adapter_commit_modes2 +
    // query_target_info/set_default_hdr_metadata/set_gamma_ramp). The proven oracle sets exactly this flag
    // with the INF still at UmdfExtensions=IddCx0102. GammaSupport stays NONE (set above). Enum is bindgen
    // ModuleConsts — the variant is a plain-int const assignable straight to the `Flags` field.
    caps.Flags = iddcx::IDDCX_ADAPTER_FLAGS::IDDCX_ADAPTER_FLAGS_CAN_PROCESS_FP16;
    // SPIKE E1b (multi-seat O1, `design/windows-seat-display-tier.md`). IddCx roles are exclusive:
    // a remote-session adapter serves remote-session monitors, a console one console monitors. The
    // role therefore has to be per DEVICE, and the devnode's own hardware id is what says which it
    // is — the seat devnode is the per-session SWD node (`pf_vdisplay_IndirectDisplay`, RdpIdd's
    // shape), the shipped console devnode is `Root\pf_vdisplay` and structurally cannot take this
    // branch.
    // SAFETY: `device` is the live WDFDEVICE this D0 entry is initialising, which is the contract
    // `query_hardware_ids` requires.
    let hardware_ids = unsafe { pf_umdf_util::wdf::query_hardware_ids(device) };
    if hardware_ids.contains("pf_vdisplay_indirectdisplay") {
        // The roles are exclusive, so the seat adapter declares ONLY the remote role: FP16 is a
        // console-desktop processing cap and pairing the two is what `IddCxAdapterInitAsync`
        // rejects. `PFVD_SEAT_CAPS` overrides the mask while the shape is still being probed.
        let caps_override = crate::log::knob("PFVD_SEAT_CAPS").and_then(|v| v.parse::<u32>().ok());
        caps.Flags = caps_override
            .unwrap_or(iddcx::IDDCX_ADAPTER_FLAGS::IDDCX_ADAPTER_FLAGS_REMOTE_SESSION_DRIVER);
        dbglog!(
            "[pf-vd] adapter: seat devnode ({hardware_ids}) caps={:#x}{}",
            caps.Flags,
            if caps_override.is_some() {
                " (PFVD_SEAT_CAPS)"
            } else {
                ""
            }
        );
    } else {
        dbglog!("[pf-vd] adapter: console role (hwids: {hardware_ids})");
    }
    caps.MaxMonitorsSupported = 16;
    caps.EndPointDiagnostics = diag;

    // The adapter WDF object's attributes. Execution/Synchronization must be spelled out: a zeroed
    // field is *Invalid*, not InheritFromParent. No context type — nothing reads adapter state off
    // the WDF object; the handle lives in [`ADAPTER`].
    let mut attr = pod_init!(wdk_sys::WDF_OBJECT_ATTRIBUTES);
    attr.Size = core::mem::size_of::<wdk_sys::WDF_OBJECT_ATTRIBUTES>() as u32;
    attr.ExecutionLevel = wdk_sys::_WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent;
    attr.SynchronizationScope =
        wdk_sys::_WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent;
    let init = iddcx::IDARG_IN_ADAPTER_INIT {
        WdfDevice: device,
        pCaps: &raw mut caps,
        ObjectAttributes: &raw mut attr,
    };
    let mut out = pod_init!(iddcx::IDARG_OUT_ADAPTER_INIT);
    // SAFETY: `init`/`out` are valid local storage; IddCxAdapterInitAsync reads the caps synchronously
    // (the adapter object itself is delivered later via adapter_init_finished). Called once per device.
    let st = unsafe { wdk_iddcx::IddCxAdapterInitAsync(&init, &mut out) };
    dbglog!("[pf-vd] IddCxAdapterInitAsync -> {st:#x}");
    st
}

/// Stash the adapter object delivered by `EvtIddCxAdapterInitFinished` (STEP 4 reads it).
/// Last write wins — see [`ADAPTER`].
pub fn set_adapter(adapter: iddcx::IDDCX_ADAPTER) {
    *ADAPTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(SendAdapter(adapter));
}

/// Forget the cached adapter. Called on a D0 re-entry from a REAL low-power state
/// (`callbacks::device_d0_entry`): the handle belongs to the pre-power-cycle incarnation, and
/// clearing is what lets `init_adapter` run again instead of short-circuiting on it.
pub fn clear_adapter() {
    *ADAPTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// The created adapter handle, once `EvtIddCxAdapterInitFinished` has fired — for `create_monitor`
/// (`IddCxMonitorCreate`) and SET_RENDER_ADAPTER. `None` before adapter init completes.
pub(crate) fn adapter() -> Option<iddcx::IDDCX_ADAPTER> {
    ADAPTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(|a| a.0)
}

/// The pin in force and the owner that set it. IddCx has one render adapter per IddCx adapter,
/// so this cannot be scoped: another owner may repeat it, not move it ([`set_render_adapter`]).
static RENDER_PIN: Mutex<Option<(i64, u32)>> = Mutex::new(None);

/// Honor `owner`'s `IOCTL_SET_RENDER_ADAPTER`: pin the GPU the IddCx swap-chain renders on. On a
/// hybrid iGPU+dGPU box the OS may otherwise pick the iGPU to render the virtual monitor, and the
/// encode pool then opens on an adapter whose encoder the host never selected. The pin is
/// adapter-wide and moving it flaps every live swap-chain, so a different GPU is refused with
/// `STATUS_ACCESS_DENIED` while the owner that pinned the current one still holds a monitor; the
/// host tolerates that and streams on the GPU in force. `STATUS_NOT_FOUND` before the adapter
/// exists.
pub fn set_render_adapter(owner: u32, luid_low: u32, luid_high: i32) -> NTSTATUS {
    let Some(adapter) = adapter() else {
        return crate::STATUS_NOT_FOUND;
    };
    let packed = (i64::from(luid_high) << 32) | i64::from(luid_low);
    let pin = *crate::registry::lock(&RENDER_PIN);
    if let Some((held, by)) = pin {
        if held != packed && by != owner && crate::registry::find(|m| m.owner == by).is_some() {
            dbglog!(
                "[pf-vd] set_render_adapter: owner {owner} asked for {luid_high:08x}:{luid_low:08x} \
                 while owner {by} holds monitors on the pinned GPU — refused"
            );
            return crate::STATUS_ACCESS_DENIED;
        }
    }
    *crate::registry::lock(&RENDER_PIN) = Some((packed, owner));
    let mut in_args = pod_init!(iddcx::IDARG_IN_ADAPTERSETRENDERADAPTER);
    in_args.PreferredRenderAdapter = wdk_sys::LUID {
        LowPart: luid_low,
        HighPart: luid_high,
    };
    dbglog!("[pf-vd] set_render_adapter -> {luid_high:08x}:{luid_low:08x}");
    // SAFETY: `adapter` is the stashed IddCx adapter; `in_args` is valid local storage read synchronously.
    unsafe { wdk_iddcx::IddCxAdapterSetRenderAdapter(adapter, &in_args) };
    STATUS_SUCCESS
}
