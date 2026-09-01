//! Shared between the GUI wizard bin and the console S2 CLI bin: a GUI-subsystem exe is
//! never awaited by `&` in PowerShell and its stdout vanishes, so the overlay tooling needs
//! its own console binary.

pub mod cli;
pub mod overlay;
