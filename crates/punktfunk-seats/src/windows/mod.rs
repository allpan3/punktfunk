//! Production Windows implementation of the seat platform boundary.
//!
//! Account, credential, WTS, process/job, RDP, supervisor, and SCM concerns are
//! split into narrow modules. `WindowsBackend` owns only per-instance shared
//! state, so it cannot collide with the ordinary PunktfunkHost service. The
//! backend opens one hardened secret root, obtains the recorded absolute host
//! path from the 64-bit reservation key, and delegates each portable command.
//! Doctor reports prerequisite and ownership evidence but treats patch status as
//! unavailable and never infers HDR or IDD health from static configuration.

mod accounts;
mod credentials;
mod process;
pub mod rdp;
pub mod scm;
mod supervisor;
mod util;
mod wts;

use crate::backend::{BackendError, PlatformBackend};
use crate::model::{
    Ledger, RuntimeState, RuntimeStatus, Seat, DEFAULT_MGMT_PORTS, DEFAULT_NATIVE_PORTS,
};
use crate::persistence::SecretRoot;
use crate::protocol::{Diagnostic, DiagnosticLevel};
use accounts::AccountManager;
use credentials::CredentialStore;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;
use supervisor::Supervisor;
use util::{backend_error, io_error, require_local_system, WinResult};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::System::SystemInformation::{
    IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, OSVERSIONINFOW,
};
use windows::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};

#[derive(Clone)]
pub struct WindowsBackend {
    root: SecretRoot,
    accounts: AccountManager,
    supervisor: Supervisor,
}

pub fn trust_rdp(root: impl AsRef<Path>, replace: bool) -> Result<[u8; 32], BackendError> {
    util::require_elevated_admin()?;
    scm::require_termservice_running()?;
    let root = SecretRoot::open(root)
        .map_err(|error| io_error("seat_root", "open hardened seat root", error))?;
    rdp::trust(&root, replace)
}

pub fn run_rdp_keeper() -> Result<(), BackendError> {
    rdp::run_keeper_from_stdin()
}

impl WindowsBackend {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, BackendError> {
        if !scm::reservation_exists() {
            return Err(backend_error(
                "reservation_missing",
                r"HKLM\SOFTWARE\Punktfunk\Seats is missing; install the service first",
            ));
        }
        let root = SecretRoot::open(root)
            .map_err(|error| io_error("seat_root", "open hardened seat root", error))?;
        let host_path = scm::configured_host_path()?;
        let credentials = CredentialStore::new(root.clone());
        let accounts = AccountManager::new(credentials);
        let supervisor = Supervisor::new(root.clone(), accounts.clone(), host_path);
        Ok(Self {
            root,
            accounts,
            supervisor,
        })
    }

    pub fn stop_all(&self) {
        self.supervisor.stop_all();
    }

    fn ensure_runtime_prerequisites(&self) -> WinResult<()> {
        require_local_system()?;
        if !scm::reservation_exists() {
            return Err(backend_error(
                "reservation_missing",
                r"HKLM\SOFTWARE\Punktfunk\Seats is missing",
            ));
        }
        if !scm::termservice_running()? {
            return Err(backend_error(
                "termservice_stopped",
                "TermService is not running",
            ));
        }
        Ok(())
    }
}

impl PlatformBackend for WindowsBackend {
    fn provision(&self, seat: &Seat) -> Result<(), BackendError> {
        self.ensure_runtime_prerequisites()?;
        self.accounts.provision(seat)
    }

    fn start(&self, seat: &Seat) -> Result<RuntimeStatus, BackendError> {
        self.ensure_runtime_prerequisites()?;
        self.supervisor.start(seat)
    }

    fn stop(&self, seat: &Seat) -> Result<RuntimeStatus, BackendError> {
        self.supervisor.stop(seat)
    }

    fn remove(&self, seat: &Seat) -> Result<(), BackendError> {
        let _ = self.supervisor.stop(seat)?;
        self.accounts.delete(seat)
    }

    fn status(&self, seat: &Seat) -> Result<RuntimeStatus, BackendError> {
        Ok(self.supervisor.status(seat).status)
    }

    fn doctor(&self, ledger: &Ledger) -> Result<Vec<Diagnostic>, BackendError> {
        Ok(self.doctor_report(ledger))
    }
}

impl WindowsBackend {
    fn doctor_report(&self, ledger: &Ledger) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        match windows_build() {
            Ok(build) if build >= 22621 => diagnostics.push(Diagnostic::info(
                "windows_build",
                format!("Windows build {build} meets the 22621 minimum"),
            )),
            Ok(build) => diagnostics.push(Diagnostic::error(
                "windows_build",
                format!("Windows build {build} is below the 22621 minimum"),
            )),
            Err(error) => diagnostics.push(Diagnostic::error("windows_build", error.to_string())),
        }
        match native_x64() {
            Ok(true) => diagnostics.push(Diagnostic::info("architecture", "native OS is x64")),
            Ok(false) => {
                diagnostics.push(Diagnostic::error("architecture", "native OS is not x64"))
            }
            Err(error) => diagnostics.push(Diagnostic::error("architecture", error.to_string())),
        }
        match util::is_local_system() {
            Ok(true) => diagnostics.push(Diagnostic::info(
                "identity",
                "service process is LocalSystem",
            )),
            Ok(false) => diagnostics.push(Diagnostic::error(
                "identity",
                "service process is not LocalSystem",
            )),
            Err(error) => diagnostics.push(Diagnostic::error("identity", error.to_string())),
        }
        if scm::reservation_exists() {
            diagnostics.push(Diagnostic::info(
                "reservation",
                r"HKLM\SOFTWARE\Punktfunk\Seats reserves slots 12 through 15",
            ));
        } else {
            diagnostics.push(Diagnostic::error(
                "reservation",
                "Windows display-slot reservation marker is missing",
            ));
        }
        match scm::configured_host_path() {
            Ok(path) if path.is_file() => diagnostics.push(Diagnostic::info(
                "host_executable",
                format!("host executable exists at {}", path.display()),
            )),
            Ok(path) => diagnostics.push(Diagnostic::error(
                "host_executable",
                format!("host executable is missing at {}", path.display()),
            )),
            Err(error) => diagnostics.push(Diagnostic::error("host_executable", error.to_string())),
        }
        match rdp::load_pin_optional(&self.root) {
            Ok(Some(pin)) => diagnostics.push(Diagnostic::info(
                "rdp_pin",
                format!("trusted RDP leaf SHA-256 is {}", hex::encode(pin)),
            )),
            Ok(None) => diagnostics.push(Diagnostic::error(
                "rdp_pin",
                "RDP certificate pin is missing",
            )),
            Err(error) => diagnostics.push(Diagnostic::error("rdp_pin", error.to_string())),
        }
        match scm::termservice_running() {
            Ok(true) => diagnostics.push(Diagnostic::info("termservice", "TermService is running")),
            Ok(false) => {
                diagnostics.push(Diagnostic::error("termservice", "TermService is stopped"))
            }
            Err(error) => diagnostics.push(Diagnostic::error("termservice", error.to_string())),
        }
        diagnostics.push(Diagnostic::error(
            "patch_ready_unavailable",
            "patch-ready status is unavailable until the display patch module exposes it",
        ));
        for seat in &ledger.seats {
            self.doctor_seat(seat, &mut diagnostics);
        }
        diagnostics
    }

    fn doctor_seat(&self, seat: &Seat, diagnostics: &mut Vec<Diagnostic>) {
        let index = usize::from(seat.display_slot.saturating_sub(12));
        let resources_match = DEFAULT_NATIVE_PORTS.get(index) == Some(&seat.native_port)
            && DEFAULT_MGMT_PORTS.get(index) == Some(&seat.mgmt_port);
        diagnostics.push(seat_diagnostic(
            if resources_match {
                DiagnosticLevel::Info
            } else {
                DiagnosticLevel::Error
            },
            "seat_resources",
            format!(
                "slot {} uses native {} and management {}",
                seat.display_slot, seat.native_port, seat.mgmt_port
            ),
            seat,
        ));
        match self.accounts.inspect(seat) {
            Ok(inspection) => {
                diagnostics.push(seat_diagnostic(
                    if inspection.marker_matches {
                        DiagnosticLevel::Info
                    } else {
                        DiagnosticLevel::Error
                    },
                    "account_marker",
                    if inspection.marker_matches {
                        "account marker matches the exact seat ID".into()
                    } else {
                        "account is absent or its marker does not match".into()
                    },
                    seat,
                ));
                diagnostics.push(seat_diagnostic(
                    if inspection.credential_blob {
                        DiagnosticLevel::Info
                    } else {
                        DiagnosticLevel::Error
                    },
                    "credential_blob",
                    if inspection.credential_blob {
                        "DPAPI credential blob exists".into()
                    } else {
                        "DPAPI credential blob is missing".into()
                    },
                    seat,
                ));
                let policy_ok = inspection.rdp_member
                    && !inspection.administrator
                    && inspection.deny_console
                    && !inspection.deny_remote;
                diagnostics.push(seat_diagnostic(
                    if policy_ok {
                        DiagnosticLevel::Info
                    } else {
                        DiagnosticLevel::Error
                    },
                    "account_policy",
                    format!(
                        "rdp_member={} administrator={} deny_console={} deny_remote={}",
                        inspection.rdp_member,
                        inspection.administrator,
                        inspection.deny_console,
                        inspection.deny_remote
                    ),
                    seat,
                ));
            }
            Err(error) => diagnostics.push(seat_diagnostic(
                DiagnosticLevel::Error,
                "account_query",
                error.to_string(),
                seat,
            )),
        }
        let snapshot = self.supervisor.status(seat);
        diagnostics.push(seat_diagnostic(
            if snapshot.status.state == RuntimeState::Failed {
                DiagnosticLevel::Error
            } else {
                DiagnosticLevel::Info
            },
            "child_state",
            format!(
                "state={:?} session={:?} keeper_pid={:?} host_pid={:?}",
                snapshot.status.state, snapshot.session_id, snapshot.keeper_pid, snapshot.host_pid
            ),
            seat,
        ));
        for (kind, port) in [("native", seat.native_port), ("management", seat.mgmt_port)] {
            let open = port_open(port);
            let expected = snapshot.status.state == RuntimeState::Running;
            diagnostics.push(seat_diagnostic(
                if open == expected {
                    DiagnosticLevel::Info
                } else {
                    DiagnosticLevel::Warning
                },
                "seat_port",
                format!(
                    "{kind} port {port} is {}",
                    if open { "open" } else { "closed" }
                ),
                seat,
            ));
        }
    }
}

fn windows_build() -> WinResult<u32> {
    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // SAFETY: `version` is the initialized, correctly sized output structure required by ntdll.
    let status = unsafe { RtlGetVersion(&mut version) };
    if status.is_ok() {
        Ok(version.dwBuildNumber)
    } else {
        Err(io_error(
            "windows_build",
            "RtlGetVersion failed",
            std::io::Error::from_raw_os_error(status.0),
        ))
    }
}

fn native_x64() -> WinResult<bool> {
    let mut process_machine = IMAGE_FILE_MACHINE::default();
    let mut native_machine = IMAGE_FILE_MACHINE::default();
    // SAFETY: current-process pseudo-handle is valid and both machine outputs remain writable.
    unsafe {
        IsWow64Process2(
            GetCurrentProcess(),
            &mut process_machine,
            Some(&mut native_machine),
        )
    }
    .map_err(|error| io_error("architecture", "IsWow64Process2 failed", error))?;
    Ok(native_machine == IMAGE_FILE_MACHINE_AMD64)
}

fn seat_diagnostic(level: DiagnosticLevel, code: &str, message: String, seat: &Seat) -> Diagnostic {
    Diagnostic {
        level,
        code: code.into(),
        message,
        seat_id: Some(seat.id.clone()),
    }
}

fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        Duration::from_millis(100),
    )
    .is_ok()
}
