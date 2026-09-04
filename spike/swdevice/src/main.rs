//! E1b: create a pf-vdisplay devnode the way the RDP stack creates RdpIdd's, then hold it.
//!
//! A remote-session IddCx adapter is not a root devnode with a flag set. Microsoft's own remote
//! display is `SWD\REMOTEDISPLAYENUM\RDPIDD_INDIRECTDISPLAY&SESSIONID_0002` carrying
//! `DEVPKEY_Device_SessionId`, bound by the bare hardware id `RdpIdd_IndirectDisplay`. This makes
//! the equivalent for us so `IddCxAdapterInitAsync` can be asked again with
//! `REMOTE_SESSION_DRIVER` set. Run it INSIDE the seat session: the open question is whether the
//! session id is inherited from the creating process.
//!
//! The devnode lives only while the returned handle is open, so this parks until killed.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use windows::core::{HRESULT, PCWSTR};
use windows::Win32::Devices::Properties::{
    DEVPROPCOMPKEY, DEVPROPERTY, DEVPROPKEY, DEVPROP_STORE_SYSTEM, DEVPROP_TYPE_UINT32,
};
use windows::Win32::Devices::Enumeration::Pnp::{
    SwDeviceCreate, HSWDEVICE, SWDeviceCapabilitiesDriverRequired,
    SWDeviceCapabilitiesRemovable, SWDeviceCapabilitiesSilentInstall, SW_DEVICE_CREATE_INFO,
};

static DONE: AtomicBool = AtomicBool::new(false);
static RESULT: AtomicI32 = AtomicI32::new(0);

/// SwDeviceCreate reports the real outcome here, not through its return value.
unsafe extern "system" fn on_created(
    _dev: HSWDEVICE,
    hr: HRESULT,
    _ctx: *const c_void,
    instance: PCWSTR,
) {
    RESULT.store(hr.0, Ordering::SeqCst);
    let name = if instance.is_null() {
        String::new()
    } else {
        unsafe { instance.to_string() }.unwrap_or_default()
    };
    println!("callback: hr={:#010x} instance={name}", hr.0 as u32);
    DONE.store(true, Ordering::SeqCst);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// REG_MULTI_SZ: each entry NUL-terminated, the list terminated by a second NUL.
fn multi(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v.push(0);
    v
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |flag: &str, default: &str| -> String {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| default.to_string())
    };
    let enumerator = get("--enumerator", "PunktfunkDisplayEnum");
    let instance = get("--instance", "pf_vdisplay_seat");
    let hwid = get("--hwid", "pf_vdisplay_IndirectDisplay");

    let session = unsafe {
        let mut s = 0u32;
        let pid = windows::Win32::System::Threading::GetCurrentProcessId();
        let _ = windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(pid, &mut s);
        s
    };
    println!("creating enumerator={enumerator} instance={instance} hwid={hwid} from session={session}");

    let w_enum = wide(&enumerator);
    let w_parent = wide("HTREE\\ROOT\\0");
    let w_inst = wide(&instance);
    let w_hwids = multi(&hwid);
    let w_desc = wide("Punktfunk Virtual Display (seat)");

    let mut info = SW_DEVICE_CREATE_INFO {
        cbSize: std::mem::size_of::<SW_DEVICE_CREATE_INFO>() as u32,
        pszInstanceId: PCWSTR(w_inst.as_ptr()),
        pszzHardwareIds: PCWSTR(w_hwids.as_ptr()),
        pszzCompatibleIds: PCWSTR::null(),
        pContainerId: std::ptr::null(),
        CapabilityFlags: (SWDeviceCapabilitiesRemovable.0
            | SWDeviceCapabilitiesSilentInstall.0
            | SWDeviceCapabilitiesDriverRequired.0) as u32,
        pszDeviceDescription: PCWSTR(w_desc.as_ptr()),
        pszDeviceLocation: PCWSTR::null(),
        pSecurityDescriptor: std::ptr::null(),
    };

    // RdpIdd's devnode carries DEVPKEY_Device_SessionId; ours created from session 0 carries none,
    // and a session-less adapter is refused. Set it explicitly so a session-0 caller (the seats
    // supervisor, in production) can mint the device FOR a seat session.
    let want_session: Option<u32> = args
        .iter()
        .position(|a| a == "--session")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok());
    let mut session_value: u32 = want_session.unwrap_or(session);
    let session_key = DEVPROPKEY {
        fmtid: windows::core::GUID::from_u128(0x83da6326_97a6_4088_9453_a1923f573b29),
        pid: 6,
    };
    let props = [DEVPROPERTY {
        CompKey: DEVPROPCOMPKEY {
            Key: session_key,
            Store: DEVPROP_STORE_SYSTEM,
            LocaleName: PCWSTR::null(),
        },
        Type: DEVPROP_TYPE_UINT32,
        BufferSize: 4,
        Buffer: (&raw mut session_value).cast(),
    }];
    let props = want_session.map(|_| &props[..]);
    println!("session property: {props:?} (target session {session_value})");

    // SAFETY: every PCWSTR points at a NUL-terminated local that outlives the call and the wait
    // below; `info` is fully initialised with its own `cbSize`; the property buffer outlives it too.
    // The callback only stores results.
    let handle = unsafe {
        SwDeviceCreate(
            PCWSTR(w_enum.as_ptr()),
            PCWSTR(w_parent.as_ptr()),
            &mut info,
            props,
            Some(on_created),
            None,
        )
    };
    match handle {
        Ok(h) => {
            println!("SwDeviceCreate returned handle {:?}", h.0);
            for _ in 0..100 {
                if DONE.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let hr = RESULT.load(Ordering::SeqCst);
            println!("creation hr={:#010x}", hr as u32);
            if hr != 0 {
                println!("FAILED to create the software device");
                return;
            }
            println!("device created; holding it open (kill this process to remove it)");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
            }
        }
        Err(e) => println!("SwDeviceCreate failed: {e}"),
    }
}
