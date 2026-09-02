//! Standalone four-seat service and its Windows platform backend.
//!
//! The portable core owns the validated ledger, crash-recoverable secret root,
//! pinned loopback control protocol, and serialized command dispatch. Windows
//! adds account ownership, DPAPI credentials, NLA session keepers, WTS selection,
//! job-contained supervision, and SCM lifecycle. This separate Cargo workspace
//! carries the RDP stack's newer Rust floor without raising the streaming repo's.
//! Platform effects cross `PlatformBackend`, so portable tests use a fake.

#![cfg_attr(not(windows), forbid(unsafe_code))]

pub mod backend;
#[cfg(any(windows, test))]
mod bootstrap;
pub mod cli;
pub mod control;
pub mod model;
pub mod persistence;
pub mod protocol;
pub mod service;
pub mod tls;
#[cfg(windows)]
pub mod windows;

pub use backend::{BackendError, PlatformBackend, UnsupportedBackend};
pub use control::{ClientError, ControlClient, ControlError, ControlServer};
pub use model::{CreateSeat, Ledger, PortPool, RuntimeState, RuntimeStatus, Seat, SeatId};
pub use persistence::{LedgerStore, SecretRoot, StoreError};
pub use protocol::{Command, CommandResult, Request, Response};
pub use service::SeatService;
#[cfg(windows)]
pub use windows::WindowsBackend;
