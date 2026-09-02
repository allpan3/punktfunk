//! Windows SCM lifecycle and reservation-marker ownership.
//!
//! `PunktfunkSeats` is an auto-start LocalSystem own-process service with SCM
//! restart recovery. Install records the absolute host path and creates the
//! 64-bit HKLM Seats key whose existence reserves display slots 12 through 15.
//! Service run opens the hardened root, reconciles autostart, and serves pinned
//! TLS only on 127.0.0.1:47994 until Stop, Preshutdown, or Shutdown. Uninstall
//! first waits for service teardown, logs off remaining exact marked sessions,
//! removes SCM registration and the reservation key, and preserves all files
//! and accounts that were not explicitly deleted through the seat API.

use crate::cli::{self, ServiceCommand};
use crate::control::ControlServer;
use crate::model::PortPool;
use crate::persistence::LedgerStore;
use crate::service::SeatService;
use crate::windows::accounts;
use crate::windows::util::{
    backend_error, computer_name, io_error, require_elevated_admin, WinResult,
};
use crate::windows::{wts, WindowsBackend};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceDependency, ServiceErrorControl, ServiceExitCode, ServiceFailureActions,
    ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus,
    ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use winreg::enums::{
    HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY, KEY_WRITE,
};
use winreg::RegKey;

pub(super) const SERVICE_NAME: &str = "PunktfunkSeats";
const SERVICE_DISPLAY: &str = "Punktfunk Multi-Seat Supervisor";
const SERVICE_DESCRIPTION: &str =
    "Owns Punktfunk seat accounts, pinned RDP sessions, and per-seat host processes.";
const REGISTRY_PATH: &str = r"SOFTWARE\Punktfunk\Seats";
const HOST_PATH_VALUE: &str = "HostPath";
const CONTROL_ADDRESS: &str = "127.0.0.1:47994";
const SERVICE_WAIT: Duration = Duration::from_secs(45);

windows_service::define_windows_service!(ffi_service_main, service_main);

pub fn run_command(command: ServiceCommand) -> WinResult<()> {
    match command {
        ServiceCommand::Install { host } => install(host.as_deref()),
        ServiceCommand::Uninstall => uninstall(),
        ServiceCommand::Start => start(),
        ServiceCommand::Stop => stop(),
        ServiceCommand::Restart => {
            stop()?;
            start()
        }
        ServiceCommand::Status => {
            let status = query_service_status()?;
            println!("{SERVICE_NAME}: {}", state_name(status.current_state));
            Ok(())
        }
        ServiceCommand::Run => {
            windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(
                |error| {
                    io_error(
                        "service_dispatcher",
                        "service run must be launched by the Service Control Manager",
                        error,
                    )
                },
            )
        }
    }
}

pub(super) fn reservation_exists() -> bool {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(REGISTRY_PATH, KEY_QUERY_VALUE | KEY_WOW64_64KEY)
        .is_ok()
}

pub(super) fn configured_host_path() -> WinResult<PathBuf> {
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(REGISTRY_PATH, KEY_QUERY_VALUE | KEY_WOW64_64KEY)
        .map_err(|error| {
            io_error(
                "reservation_missing",
                "open HKLM Punktfunk Seats key",
                error,
            )
        })?;
    let value: OsString = key
        .get_value(HOST_PATH_VALUE)
        .map_err(|error| io_error("host_path_missing", "read configured host path", error))?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(backend_error(
            "host_path_invalid",
            "configured host path is not absolute",
        ));
    }
    Ok(path)
}

pub(super) fn termservice_running() -> WinResult<bool> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|error| io_error("termservice", "open Service Control Manager", error))?;
    let service = manager
        .open_service("TermService", ServiceAccess::QUERY_STATUS)
        .map_err(|error| io_error("termservice", "open TermService", error))?;
    let status = service
        .query_status()
        .map_err(|error| io_error("termservice", "query TermService", error))?;
    Ok(status.current_state == ServiceState::Running)
}

pub fn require_termservice_running() -> WinResult<()> {
    if termservice_running()? {
        Ok(())
    } else {
        Err(backend_error(
            "termservice_stopped",
            "TermService must be running and listening before RDP trust",
        ))
    }
}

fn install(host: Option<&Path>) -> WinResult<()> {
    require_elevated_admin()?;
    let seats_exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| io_error("service_install", "resolve seats executable", error))?;
    let host = match host {
        Some(path) => std::fs::canonicalize(path),
        None => seats_exe
            .parent()
            .map(|parent| parent.join("punktfunk-host.exe"))
            .ok_or_else(|| std::io::Error::other("seats executable has no parent"))
            .and_then(std::fs::canonicalize),
    }
    .map_err(|error| io_error("host_missing", "resolve punktfunk-host.exe", error))?;
    if !host.is_absolute()
        || host
            .file_name()
            .and_then(OsStr::to_str)
            .is_none_or(|name| !name.eq_ignore_ascii_case("punktfunk-host.exe"))
    {
        return Err(backend_error(
            "host_path_invalid",
            "--host must name an existing absolute punktfunk-host.exe",
        ));
    }

    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|error| {
        io_error(
            "service_install",
            "open Service Control Manager (run elevated)",
            error,
        )
    })?;
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: SERVICE_DISPLAY.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: seats_exe,
        launch_arguments: vec![OsString::from("service"), OsString::from("run")],
        dependencies: vec![ServiceDependency::Service(OsString::from("TermService"))],
        account_name: None,
        account_password: None,
    };
    match manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START) {
        Ok(service) => {
            service
                .set_description(SERVICE_DESCRIPTION)
                .map_err(|error| io_error("service_install", "set service description", error))?;
        }
        Err(windows_service::Error::Winapi(error)) if error.raw_os_error() == Some(1073) => {
            let service = manager
                .open_service(SERVICE_NAME, ServiceAccess::CHANGE_CONFIG)
                .map_err(|error| io_error("service_install", "open existing service", error))?;
            service
                .change_config(&info)
                .map_err(|error| io_error("service_install", "reconfigure service", error))?;
            service
                .set_description(SERVICE_DESCRIPTION)
                .map_err(|error| io_error("service_install", "set service description", error))?;
        }
        Err(error) => {
            return Err(io_error(
                "service_install",
                "create PunktfunkSeats service",
                error,
            ));
        }
    }
    let recovery = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::CHANGE_CONFIG | ServiceAccess::START,
        )
        .and_then(|service| {
            service.update_failure_actions(ServiceFailureActions {
                reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 60 * 60)),
                reboot_msg: None,
                command: None,
                actions: Some(vec![
                    ServiceAction {
                        action_type: ServiceActionType::Restart,
                        delay: Duration::from_secs(1),
                    },
                    ServiceAction {
                        action_type: ServiceActionType::Restart,
                        delay: Duration::from_secs(5),
                    },
                    ServiceAction {
                        action_type: ServiceActionType::Restart,
                        delay: Duration::from_secs(30),
                    },
                ]),
            })?;
            service.set_failure_actions_on_non_crash_failures(true)
        });
    recovery.map_err(|error| {
        io_error(
            "service_install",
            "configure service restart recovery",
            error,
        )
    })?;
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let (key, _) = hklm
        .create_subkey_with_flags(REGISTRY_PATH, KEY_WRITE | KEY_SET_VALUE | KEY_WOW64_64KEY)
        .map_err(|error| {
            io_error(
                "reservation_create",
                "create HKLM Punktfunk Seats key",
                error,
            )
        })?;
    key.set_value(HOST_PATH_VALUE, &host.as_os_str())
        .map_err(|error| io_error("reservation_create", "record host path", error))?;
    println!(
        "Installed {SERVICE_NAME} as auto-start LocalSystem; host {}",
        host.display()
    );
    Ok(())
}

fn start() -> WinResult<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|error| io_error("service_start", "open Service Control Manager", error))?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::START | ServiceAccess::QUERY_STATUS,
        )
        .map_err(|error| io_error("service_start", "open PunktfunkSeats service", error))?;
    let status = service
        .query_status()
        .map_err(|error| io_error("service_start", "query service", error))?;
    if status.current_state != ServiceState::Running {
        service
            .start(&[] as &[&OsStr])
            .map_err(|error| io_error("service_start", "start PunktfunkSeats service", error))?;
        wait_for_state(&service, ServiceState::Running, SERVICE_WAIT)?;
    }
    println!("Started {SERVICE_NAME}.");
    Ok(())
}

fn stop() -> WinResult<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|error| io_error("service_stop", "open Service Control Manager", error))?;
    let service = match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
    ) {
        Ok(service) => service,
        Err(windows_service::Error::Winapi(error)) if error.raw_os_error() == Some(1060) => {
            return Ok(());
        }
        Err(error) => {
            return Err(io_error(
                "service_stop",
                "open PunktfunkSeats service",
                error,
            ));
        }
    };
    let status = service
        .query_status()
        .map_err(|error| io_error("service_stop", "query service", error))?;
    if status.current_state != ServiceState::Stopped {
        let _ = service.stop();
        wait_for_state(&service, ServiceState::Stopped, SERVICE_WAIT)?;
    }
    println!("Stopped {SERVICE_NAME}.");
    Ok(())
}

fn uninstall() -> WinResult<()> {
    require_elevated_admin()?;
    stop()?;
    cleanup_marked_sessions();
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|error| io_error("service_uninstall", "open Service Control Manager", error))?;
    match manager.open_service(SERVICE_NAME, ServiceAccess::DELETE) {
        Ok(service) => service.delete().map_err(|error| {
            io_error("service_uninstall", "delete PunktfunkSeats service", error)
        })?,
        Err(windows_service::Error::Winapi(error)) if error.raw_os_error() == Some(1060) => {}
        Err(error) => {
            return Err(io_error(
                "service_uninstall",
                "open PunktfunkSeats service",
                error,
            ));
        }
    }
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .delete_subkey_with_flags(REGISTRY_PATH, KEY_WOW64_64KEY)
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            }
        })
        .map_err(|error| {
            io_error(
                "reservation_remove",
                "remove HKLM Punktfunk Seats key",
                error,
            )
        })?;
    println!("Uninstalled {SERVICE_NAME}; seat ledger and credentials were preserved.");
    Ok(())
}

fn cleanup_marked_sessions() {
    let Ok(store) = LedgerStore::open(cli::default_root()) else {
        return;
    };
    let Ok(ledger) = store.load() else {
        return;
    };
    let Ok(sessions) = wts::enumerate() else {
        return;
    };
    let Ok(machine) = computer_name() else {
        return;
    };
    for seat in &ledger.seats {
        if accounts::marker_matches(seat) != Ok(true) {
            continue;
        }
        for session in sessions.iter().filter(|session| {
            !session.console
                && session.user.eq_ignore_ascii_case(&seat.account)
                && (session.domain.is_empty() || session.domain.eq_ignore_ascii_case(&machine))
        }) {
            let _ = wts::logoff(session.id);
        }
    }
}

fn query_service_status() -> WinResult<ServiceStatus> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|error| io_error("service_status", "open Service Control Manager", error))?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)
        .map_err(|error| io_error("service_status", "open PunktfunkSeats service", error))?;
    service
        .query_status()
        .map_err(|error| io_error("service_status", "query PunktfunkSeats service", error))
}

fn wait_for_state(
    service: &windows_service::service::Service,
    expected: ServiceState,
    timeout: Duration,
) -> WinResult<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = service
            .query_status()
            .map_err(|error| io_error("service_wait", "query service status", error))?;
        if status.current_state == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(backend_error(
                "service_timeout",
                format!(
                    "service did not reach {} within 45 seconds",
                    state_name(expected)
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn state_name(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Stopped => "stopped",
        ServiceState::StartPending => "start-pending",
        ServiceState::StopPending => "stop-pending",
        ServiceState::Running => "running",
        ServiceState::ContinuePending => "continue-pending",
        ServiceState::PausePending => "pause-pending",
        ServiceState::Paused => "paused",
    }
}

fn service_main(_arguments: Vec<OsString>) {
    if run_service().is_err() {
        std::process::exit(1);
    }
}

fn run_service() -> WinResult<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Preshutdown | ServiceControl::Shutdown => {
            signal.store(true, Ordering::Release);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status_handle = service_control_handler::register(SERVICE_NAME, handler)
        .map_err(|error| io_error("service_run", "register SCM control handler", error))?;
    let accepted = ServiceControlAccept::STOP
        | ServiceControlAccept::PRESHUTDOWN
        | ServiceControlAccept::SHUTDOWN;
    let mut status = ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 1,
        wait_hint: Duration::from_secs(30),
        process_id: None,
    };
    status_handle
        .set_service_status(status.clone())
        .map_err(|error| io_error("service_run", "set START_PENDING", error))?;

    let root = cli::default_root();
    let backend = WindowsBackend::open(&root)?;
    let service = Arc::new(
        SeatService::open(&root, backend.clone(), PortPool::default())
            .map_err(|error| io_error("service_run", "open seat ledger", error))?,
    );
    let reconcile_service = service.clone();
    let reconcile = std::thread::spawn(move || reconcile_service.reconcile_startup());
    let mut checkpoint_at = Instant::now();
    while !reconcile.is_finished() {
        if stop.load(Ordering::Acquire) {
            backend.stop_all();
        }
        if Instant::now() >= checkpoint_at {
            status.checkpoint = status.checkpoint.saturating_add(1);
            let _ = status_handle.set_service_status(status.clone());
            checkpoint_at = Instant::now() + Duration::from_secs(5);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let reconcile = reconcile
        .join()
        .map_err(|_| backend_error("service_reconcile", "autostart reconciliation panicked"))?;
    if stop.load(Ordering::Acquire) {
        backend.stop_all();
        status.current_state = ServiceState::Stopped;
        status.checkpoint = 0;
        status.wait_hint = Duration::ZERO;
        let _ = status_handle.set_service_status(status);
        return Ok(());
    }
    reconcile.map_err(|error| backend_error("service_reconcile", error.message))?;
    let server = ControlServer::bind(
        CONTROL_ADDRESS
            .parse()
            .expect("fixed control address is valid"),
        service,
    )
    .map_err(|error| io_error("service_control", "bind 127.0.0.1:47994", error))?;
    status.current_state = ServiceState::Running;
    status.controls_accepted = accepted;
    status.checkpoint = 0;
    status.wait_hint = Duration::ZERO;
    status_handle
        .set_service_status(status.clone())
        .map_err(|error| io_error("service_run", "set RUNNING", error))?;

    let stop_backend = backend.clone();
    let stop_signal = stop.clone();
    let stop_watcher = std::thread::spawn(move || {
        while !stop_signal.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(50));
        }
        stop_backend.stop_all();
    });
    let result = server
        .serve_until(&stop)
        .map_err(|error| io_error("service_control", "serve local control", error));
    stop.store(true, Ordering::Release);
    let _ = stop_watcher.join();
    status.current_state = ServiceState::StopPending;
    status.controls_accepted = ServiceControlAccept::empty();
    status.checkpoint = 1;
    status.wait_hint = Duration::from_secs(30);
    let _ = status_handle.set_service_status(status.clone());
    backend.stop_all();
    status.current_state = ServiceState::Stopped;
    status.checkpoint = 0;
    status.wait_hint = Duration::ZERO;
    let _ = status_handle.set_service_status(status);
    result
}

#[cfg(test)]
mod live_tests {
    #[test]
    #[ignore = "requires a live Windows TermService installation"]
    fn termservice_status_is_queryable() {
        assert!(super::termservice_running().is_ok());
    }
}
