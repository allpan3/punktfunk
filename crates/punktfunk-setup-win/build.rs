fn main() {
    // cfg(windows) is the HOST (skips the Linux/macOS stub build); CARGO_CFG_WINDOWS is the
    // TARGET — the same double gate clients/windows/build.rs uses.
    #[cfg(windows)]
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // Self-contained deployment: stage the full WinAppSDK runtime next to the exe.
        // An installer cannot assume the runtime it is about to install, so unlike the
        // client (framework-dependent + bootstrap()) this ships its own copy — S1 measures it.
        windows_reactor_setup::as_self_contained();
    }
}
