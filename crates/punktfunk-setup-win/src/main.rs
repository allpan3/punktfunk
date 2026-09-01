//! S1 spike (`design/installer-v2-windows.md` §7): prove the installer wizard shape — a WinUI 3
//! (windows-reactor) window running **self-contained** and **elevated**, launched from a
//! directory it was copied to (the extract-and-run pattern) — and measure what it weighs.
//!
//! Deliberately static: no engine, no payload, no state hooks. One window, the elevation
//! verdict, one working button. Everything else is WP2.1's problem.

// No console window on double-click; the run proof goes to s1-report.txt beside the exe,
// because an SSH-driven launch may render in a session nobody can see. The CLI modes still
// print: a GUI-subsystem process inherits redirected stdout pipes (ssh, CI) just fine.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod cli;
mod overlay;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        if let Err(e) = cli::run(&args) {
            let _ = std::fs::write("s2-error.txt", &e);
            eprintln!("{e}");
            std::process::exit(2);
        }
        return;
    }
    ui_main();
}

#[cfg(windows)]
fn ui_main() {
    // Elevation probe without any `windows` crate features: `net session` succeeds only
    // elevated. Spike-grade; the real crate reads the token.
    let elevated = std::process::Command::new("net")
        .arg("session")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let exe = std::env::current_exe().expect("current_exe");
    let _ = std::fs::write(
        exe.with_file_name("s1-report.txt"),
        format!("started elevated={elevated} exe={}\n", exe.display()),
    );

    // Self-contained: the WinAppSDK runtime DLLs sit beside the exe (build.rs), so there is
    // deliberately NO windows_reactor::bootstrap() call — that is the framework-dependent path.
    let result = run_ui(elevated);

    let _ = std::fs::write(
        exe.with_file_name("s1-exit.txt"),
        format!("ui result: {result:?}\n"),
    );
    if result.is_err() {
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn run_ui(elevated: bool) -> windows_reactor::Result<()> {
    use windows_reactor::*;
    App::new()
        .title("Punktfunk Setup — S1 spike")
        .inner_size(560.0, 400.0)
        .backdrop(Backdrop::Mica)
        .render(move |_cx| {
            let verdict = if elevated {
                "running elevated — the host-installer requirement holds"
            } else {
                "running WITHOUT elevation — relaunch as administrator for the host leg"
            };
            vstack((
                text_block("Punktfunk Setup").font_size(24.0).semibold(),
                text_block("WinUI 3, self-contained, from a copied directory")
                    .font_size(12.0)
                    .foreground(ThemeRef::SecondaryText),
                text_block(verdict).font_size(14.0).wrap(),
                button("Close").accent().on_click(|| std::process::exit(0)),
            ))
            .spacing(12.0)
            .into()
        })
}

#[cfg(not(windows))]
fn main() {
    // The wizard is Windows-only, but the S2 overlay CLI runs anywhere the payload is.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("punktfunk-setup-win: the wizard is Windows-only; CLI modes: measure | pack | inspect");
        std::process::exit(2);
    }
    if let Err(e) = cli::run(&args) {
        eprintln!("{e}");
        std::process::exit(2);
    }
}
