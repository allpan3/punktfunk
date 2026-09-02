//! Small parser and renderer for the standalone executable.
//!
//! `serve` owns the loopback service. `ctl` maps one-for-one to protocol
//! commands: list, create, start, stop, delete, and doctor. The parser keeps
//! dependencies and startup cost small and accepts `--root` plus `--json`
//! anywhere after `ctl`. Human output is tabular and includes full stable IDs;
//! JSON output is the exact versioned response envelope. Parsing does no I/O,
//! so tests can cover usage independently from TLS and platform behavior.

use crate::model::{CreateSeat, RuntimeState, SeatId};
use crate::protocol::{Command, CommandResult, DiagnosticLevel, ErrorCode, Response};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cli {
    Serve {
        root: PathBuf,
        bind: SocketAddr,
    },
    Service(ServiceCommand),
    RdpTrust {
        root: PathBuf,
        replace: bool,
    },
    RdpKeeper,
    Ctl {
        root: PathBuf,
        json: bool,
        command: Command,
    },
    Help,
    CtlHelp,
    Version,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceCommand {
    Install { host: Option<PathBuf> },
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
    Run,
}

pub fn parse(args: &[String]) -> Result<Cli, CliError> {
    match args.first().map(String::as_str) {
        Some("serve") => parse_serve(&args[1..]),
        Some("service") => parse_service(&args[1..]),
        Some("rdp") => parse_rdp(&args[1..]),
        Some("rdp-keeper") => {
            no_args(&args[1..], "rdp-keeper")?;
            Ok(Cli::RdpKeeper)
        }
        Some("ctl") => parse_ctl(&args[1..]),
        Some("help") | Some("--help") | Some("-h") | None => Ok(Cli::Help),
        Some("version") | Some("--version") | Some("-V") => Ok(Cli::Version),
        Some(other) => Err(CliError(format!("unknown command '{other}'"))),
    }
}

pub fn default_root() -> PathBuf {
    pf_paths::config_dir().join("seats")
}

fn parse_service(args: &[String]) -> Result<Cli, CliError> {
    let Some(verb) = args.first().map(String::as_str) else {
        return Err(CliError("service requires a command".into()));
    };
    let command = match verb {
        "install" => {
            let mut host = None;
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str() {
                    "--host" if host.is_none() => {
                        host = Some(PathBuf::from(value(args, &mut index, "--host")?));
                    }
                    other => {
                        return Err(CliError(format!(
                            "service install: unknown or repeated argument '{other}'"
                        )));
                    }
                }
                index += 1;
            }
            ServiceCommand::Install { host }
        }
        "uninstall" => {
            no_args(&args[1..], "service uninstall")?;
            ServiceCommand::Uninstall
        }
        "start" => {
            no_args(&args[1..], "service start")?;
            ServiceCommand::Start
        }
        "stop" => {
            no_args(&args[1..], "service stop")?;
            ServiceCommand::Stop
        }
        "restart" => {
            no_args(&args[1..], "service restart")?;
            ServiceCommand::Restart
        }
        "status" => {
            no_args(&args[1..], "service status")?;
            ServiceCommand::Status
        }
        "run" => {
            no_args(&args[1..], "service run")?;
            ServiceCommand::Run
        }
        other => return Err(CliError(format!("unknown service command '{other}'"))),
    };
    Ok(Cli::Service(command))
}

fn parse_rdp(args: &[String]) -> Result<Cli, CliError> {
    if args.first().map(String::as_str) != Some("trust") {
        return Err(CliError("rdp needs the 'trust' command".into()));
    }
    let mut root = default_root();
    let mut replace = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--root" => root = PathBuf::from(value(args, &mut index, "--root")?),
            "--replace" => replace = true,
            other => return Err(CliError(format!("rdp trust: unknown argument '{other}'"))),
        }
        index += 1;
    }
    Ok(Cli::RdpTrust { root, replace })
}

fn parse_serve(args: &[String]) -> Result<Cli, CliError> {
    let mut root = default_root();
    let mut bind: SocketAddr = "127.0.0.1:0".parse().expect("static socket address");
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--root" => {
                root = PathBuf::from(value(args, &mut index, "--root")?);
            }
            "--bind" => {
                bind = value(args, &mut index, "--bind")?
                    .parse()
                    .map_err(|error| CliError(format!("--bind needs IP:PORT: {error}")))?;
            }
            "--help" | "-h" => return Ok(Cli::Help),
            other => return Err(CliError(format!("serve: unknown argument '{other}'"))),
        }
        index += 1;
    }
    Ok(Cli::Serve { root, bind })
}

fn parse_ctl(args: &[String]) -> Result<Cli, CliError> {
    let mut root = default_root();
    let mut json = false;
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--root" => root = PathBuf::from(value(args, &mut index, "--root")?),
            "--json" => json = true,
            other => rest.push(other.to_owned()),
        }
        index += 1;
    }
    let Some(verb) = rest.first().map(String::as_str) else {
        return Err(CliError("ctl needs a command".into()));
    };
    let command = match verb {
        "list" => {
            no_args(&rest[1..], "list")?;
            Command::List
        }
        "create" => Command::create(parse_create(&rest[1..])?),
        "start" => Command::Start {
            id: one_id(&rest[1..], "start")?,
        },
        "stop" => Command::Stop {
            id: one_id(&rest[1..], "stop")?,
        },
        "delete" => Command::Delete {
            id: one_id(&rest[1..], "delete")?,
        },
        "doctor" => {
            no_args(&rest[1..], "doctor")?;
            Command::Doctor
        }
        "help" | "--help" | "-h" => return Ok(Cli::CtlHelp),
        other => return Err(CliError(format!("unknown ctl command '{other}'"))),
    };
    Ok(Cli::Ctl {
        root,
        json,
        command,
    })
}

fn parse_create(args: &[String]) -> Result<CreateSeat, CliError> {
    let mut name = None;
    let mut account = None;
    let mut autostart = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--name" if name.is_none() => name = Some(value(args, &mut index, "--name")?),
            "--account" if account.is_none() => {
                account = Some(value(args, &mut index, "--account")?)
            }
            "--autostart" => autostart = true,
            other => {
                return Err(CliError(format!(
                    "create: unknown or repeated argument '{other}'"
                )))
            }
        }
        index += 1;
    }
    Ok(CreateSeat {
        name: name.ok_or_else(|| CliError("create needs --name NAME".into()))?,
        account: account.ok_or_else(|| CliError("create needs --account ACCOUNT".into()))?,
        autostart,
    })
}

fn value(args: &[String], index: &mut usize, flag: &str) -> Result<String, CliError> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| CliError(format!("{flag} needs a value")))
}

fn no_args(args: &[String], verb: &str) -> Result<(), CliError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(CliError(format!("{verb} takes no arguments")))
    }
}

fn one_id(args: &[String], verb: &str) -> Result<SeatId, CliError> {
    let [id] = args else {
        return Err(CliError(format!("{verb} needs exactly one seat ID")));
    };
    id.parse::<SeatId>()
        .map_err(|error| CliError(error.to_string()))
}

pub fn render_response(response: &Response, json: bool) -> Result<String, serde_json::Error> {
    if json {
        return serde_json::to_string(response).map(|mut line| {
            line.push('\n');
            line
        });
    }
    Ok(render_human(response))
}

fn render_human(response: &Response) -> String {
    match response {
        Response::Error { error, .. } => format!(
            "error[{}]: {}{}\n",
            error_code(error.code),
            error.message,
            if error.retryable { " (retryable)" } else { "" }
        ),
        Response::Success { result, .. } => match result {
            CommandResult::List { seats } => {
                if seats.is_empty() {
                    return "no seats configured\n".into();
                }
                let mut output = String::from(
                    "ID                               NAME             ACCOUNT          SLOT NATIVE MGMT  AUTO STATUS\n",
                );
                for seat in seats {
                    output.push_str(&format!(
                        "{:<32} {:<16} {:<16} {:>4} {:>6} {:>5} {:<4} {}\n",
                        seat.id,
                        truncate(&seat.name, 16),
                        truncate(&seat.account, 16),
                        seat.display_slot,
                        seat.native_port,
                        seat.mgmt_port,
                        if seat.autostart { "yes" } else { "no" },
                        runtime_name(&seat.runtime.state),
                    ));
                }
                output
            }
            CommandResult::Created { seat } => format!(
                "created {} ({}) slot {} native {} mgmt {}{}\n",
                seat.name,
                seat.id,
                seat.display_slot,
                seat.native_port,
                seat.mgmt_port,
                if seat.autostart { " autostart" } else { "" }
            ),
            CommandResult::Started { seat } => {
                format!("started {} ({})\n", seat.name, seat.id)
            }
            CommandResult::Stopped { seat } => {
                format!("stopped {} ({})\n", seat.name, seat.id)
            }
            CommandResult::Deleted { id } => format!("deleted {id}\n"),
            CommandResult::Doctor { report } => {
                let mut output = format!(
                    "doctor: {}\n",
                    if report.healthy {
                        "healthy"
                    } else {
                        "problems found"
                    }
                );
                for diagnostic in &report.diagnostics {
                    output.push_str(&format!(
                        "  {:<7} {:<24} {}{}\n",
                        diagnostic_level(diagnostic.level),
                        diagnostic.code,
                        diagnostic
                            .seat_id
                            .as_ref()
                            .map(|id| format!("{id}: "))
                            .unwrap_or_default(),
                        diagnostic.message,
                    ));
                }
                output
            }
        },
    }
}

fn runtime_name(state: &RuntimeState) -> &'static str {
    match state {
        RuntimeState::Starting => "starting",
        RuntimeState::Running => "running",
        RuntimeState::Stopping => "stopping",
        RuntimeState::Stopped => "stopped",
        RuntimeState::Failed => "failed",
        RuntimeState::Unknown => "unknown",
    }
}

fn diagnostic_level(level: DiagnosticLevel) -> &'static str {
    match level {
        DiagnosticLevel::Info => "info",
        DiagnosticLevel::Warning => "warning",
        DiagnosticLevel::Error => "error",
    }
}

fn error_code(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unauthorized => "unauthorized",
        ErrorCode::SchemaVersion => "schema_version",
        ErrorCode::InvalidRequest => "invalid_request",
        ErrorCode::Capacity => "capacity",
        ErrorCode::Conflict => "conflict",
        ErrorCode::NotFound => "not_found",
        ErrorCode::Backend => "backend",
        ErrorCode::Persistence => "persistence",
        ErrorCode::FrameTooLarge => "frame_too_large",
        ErrorCode::MalformedFrame => "malformed_frame",
        ErrorCode::Transport => "transport",
    }
}

fn truncate(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let mut output: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() && max > 0 {
        output.pop();
        output.push('…');
    }
    output
}

pub fn usage() -> &'static str {
    "USAGE:\n    punktfunk-seats service <install|uninstall|start|stop|restart|status|run>\n    punktfunk-seats rdp trust [--root DIR] [--replace]\n    punktfunk-seats ctl [--root DIR] [--json] <COMMAND>\n    punktfunk-seats serve [--root DIR] [--bind 127.0.0.1:PORT]\n\nCTL COMMANDS:\n    list\n    create --name NAME --account ACCOUNT [--autostart]\n    start ID\n    stop ID\n    delete ID\n    doctor\n"
}

pub fn ctl_usage() -> &'static str {
    "USAGE:\n    punktfunk-seats ctl [--root DIR] [--json] list\n    punktfunk-seats ctl [--root DIR] [--json] create --name NAME --account ACCOUNT [--autostart]\n    punktfunk-seats ctl [--root DIR] [--json] <start|stop|delete> ID\n    punktfunk-seats ctl [--root DIR] [--json] doctor\n"
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CliError(pub String);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CONTROL_SCHEMA_VERSION;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn ctl_json_and_root_are_modes_not_command_arguments() {
        let parsed = parse(&strings(&[
            "ctl",
            "create",
            "--json",
            "--name",
            "Desk",
            "--root",
            "/tmp/seats",
            "--account",
            "pf-desk",
            "--autostart",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            Cli::Ctl {
                root: "/tmp/seats".into(),
                json: true,
                command: Command::Create {
                    name: "Desk".into(),
                    account: "pf-desk".into(),
                    autostart: true,
                },
            }
        );
    }

    #[test]
    fn json_rendering_keeps_the_versioned_envelope() {
        let response = Response::success(CommandResult::List { seats: Vec::new() });
        let output = render_response(&response, true).unwrap();
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["schema_version"], CONTROL_SCHEMA_VERSION);
        assert_eq!(json["status"], "success");
    }

    #[test]
    fn privileged_roles_have_typed_parsing() {
        assert_eq!(
            parse(&strings(&[
                "service",
                "install",
                "--host",
                "C:/pf/punktfunk-host.exe"
            ]))
            .unwrap(),
            Cli::Service(ServiceCommand::Install {
                host: Some("C:/pf/punktfunk-host.exe".into()),
            })
        );
        assert_eq!(
            parse(&strings(&[
                "rdp",
                "trust",
                "--replace",
                "--root",
                "C:/ProgramData/pf"
            ]))
            .unwrap(),
            Cli::RdpTrust {
                root: "C:/ProgramData/pf".into(),
                replace: true,
            }
        );
        assert!(parse(&strings(&["rdp-keeper", "secret"])).is_err());
    }
}
