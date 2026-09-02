use punktfunk_seats::backend::PlatformBackend;
#[cfg(not(windows))]
use punktfunk_seats::backend::UnsupportedBackend;
use punktfunk_seats::cli::{self, Cli};
use punktfunk_seats::control::{ClientError, ControlClient, ControlServer};
use punktfunk_seats::model::PortPool;
use punktfunk_seats::protocol::{ApiError, ErrorCode, Response};
use punktfunk_seats::service::SeatService;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    punktfunk_seats::tls::install_default_provider();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|argument| argument == "--json");
    let code = match cli::parse(&args) {
        Ok(command) => run(command),
        Err(error) => {
            let code = report_error(2, ErrorCode::InvalidRequest, error.to_string(), json);
            if !json {
                eprintln!("{}", cli::usage());
            }
            code
        }
    };
    if code != 0 {
        std::process::exit(code);
    }
}

fn run(command: Cli) -> i32 {
    match command {
        Cli::Help => {
            print!("{}", cli::usage());
            0
        }
        Cli::CtlHelp => {
            print!("{}", cli::ctl_usage());
            0
        }
        Cli::Version => {
            println!("punktfunk-seats {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Cli::Serve { root, bind } => serve(root, bind),
        Cli::Service(command) => {
            #[cfg(windows)]
            {
                match punktfunk_seats::windows::scm::run_command(command) {
                    Ok(()) => 0,
                    Err(error) => report_error(1, ErrorCode::Backend, error.to_string(), false),
                }
            }
            #[cfg(not(windows))]
            {
                let _ = command;
                unsupported_platform("Windows SCM service management")
            }
        }
        Cli::RdpTrust { root, replace } => {
            #[cfg(windows)]
            {
                match punktfunk_seats::windows::trust_rdp(root, replace) {
                    Ok(pin) => {
                        println!("trusted RDP leaf SHA-256 {}", hex::encode(pin));
                        0
                    }
                    Err(error) => report_error(1, ErrorCode::Backend, error.to_string(), false),
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (root, replace);
                unsupported_platform("RDP certificate trust")
            }
        }
        Cli::RdpKeeper => {
            #[cfg(windows)]
            {
                match punktfunk_seats::windows::run_rdp_keeper() {
                    Ok(()) => 0,
                    Err(error) => report_error(1, ErrorCode::Backend, error.to_string(), false),
                }
            }
            #[cfg(not(windows))]
            {
                unsupported_platform("the internal RDP keeper")
            }
        }
        Cli::Ctl {
            root,
            json,
            command,
        } => run_ctl(root, json, command),
    }
}

fn serve(root: PathBuf, bind: SocketAddr) -> i32 {
    #[cfg(windows)]
    {
        let backend = match punktfunk_seats::WindowsBackend::open(&root) {
            Ok(backend) => backend,
            Err(error) => return report_error(1, ErrorCode::Backend, error.to_string(), false),
        };
        serve_with_backend(root, bind, backend)
    }
    #[cfg(not(windows))]
    {
        serve_with_backend(root, bind, UnsupportedBackend)
    }
}

fn serve_with_backend<B: PlatformBackend>(root: PathBuf, bind: SocketAddr, backend: B) -> i32 {
    let service = match SeatService::open(&root, backend, PortPool::default()) {
        Ok(service) => Arc::new(service),
        Err(error) => {
            return report_error(1, ErrorCode::Persistence, error.to_string(), false);
        }
    };
    if let Err(error) = service.reconcile_startup() {
        return report_error(1, error.code, error.message, false);
    }
    let server = match ControlServer::bind(bind, service) {
        Ok(server) => server,
        Err(error) => return report_error(1, ErrorCode::Transport, error.to_string(), false),
    };
    match server.local_addr() {
        Ok(address) => println!("punktfunk-seats listening on {address}"),
        Err(error) => eprintln!("punktfunk-seats: cannot read listener address: {error}"),
    }
    match server.serve() {
        Ok(()) => 0,
        Err(error) => report_error(1, ErrorCode::Transport, error.to_string(), false),
    }
}

fn run_ctl(root: PathBuf, json: bool, command: punktfunk_seats::Command) -> i32 {
    let client = match ControlClient::open(&root) {
        Ok(client) => client,
        Err(error) => return report_client_error(error, json),
    };
    match client.request(command) {
        Ok(response) => {
            let failed = matches!(response, Response::Error { .. });
            match cli::render_response(&response, json) {
                Ok(output) => print!("{output}"),
                Err(error) => {
                    return report_error(1, ErrorCode::Transport, error.to_string(), json);
                }
            }
            i32::from(failed)
        }
        Err(error) => report_client_error(error, json),
    }
}

#[cfg(not(windows))]
fn unsupported_platform(operation: &str) -> i32 {
    report_error(
        1,
        ErrorCode::Backend,
        format!("{operation} requires Windows; portable control and ledger tests remain available"),
        false,
    )
}

fn report_client_error(error: ClientError, json: bool) -> i32 {
    let code = if matches!(error, ClientError::PinMismatch { .. }) {
        4
    } else {
        3
    };
    report_error(code, ErrorCode::Transport, error.to_string(), json)
}

fn report_error(exit: i32, code: ErrorCode, message: String, json: bool) -> i32 {
    if json {
        let response = Response::error(ApiError::new(code, message));
        match cli::render_response(&response, true) {
            Ok(output) => print!("{output}"),
            Err(error) => eprintln!("punktfunk-seats: {error}"),
        }
    } else {
        eprintln!("punktfunk-seats: {message}");
    }
    exit
}
