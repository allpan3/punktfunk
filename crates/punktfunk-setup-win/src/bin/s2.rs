//! Console-subsystem front for the S2 overlay CLI — waits, prints, exits like a tool should.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = punktfunk_setup_win::cli::run(&args) {
        eprintln!("{e}");
        std::process::exit(2);
    }
}
