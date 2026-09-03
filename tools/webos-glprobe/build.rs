// Mirrors punktfunk-webos's build.rs for the two things the link needs on this device:
// the glibc shim (webOS glibc ~2.12 has no getauxval, which both libstd and Skia's zlib
// reference) and the $ORIGIN-relative rpath for the bundled libSDL2.
fn main() {
    if std::env::var("TARGET").as_deref() != Ok("armv7-unknown-linux-gnueabi") {
        return;
    }
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let cc = std::env::var("CC_armv7_unknown_linux_gnueabi").unwrap_or_else(|_| "cc".into());

    let obj = format!("{out_dir}/glibc_compat_shim.o");
    let status = std::process::Command::new(&cc)
        .args(["-fPIC", "-c", "glibc_compat_shim.c", "-o"])
        .arg(&obj)
        .status()
        .unwrap_or_else(|e| panic!("run {cc} to compile glibc_compat_shim.c: {e}"));
    assert!(
        status.success(),
        "{cc} failed compiling glibc_compat_shim.c"
    );
    // After libstd: a single-pass linker drops `link-lib=static` before libstd asks for it.
    println!("cargo:rustc-link-arg={obj}");
    println!("cargo:rerun-if-changed=glibc_compat_shim.c");

    // skia-safe's linux `link_libraries()` only emits EGL/GLESv2 under its `egl`/`wayland`
    // features, which we do not take; SDL2 owns context creation here, so name them directly.
    println!("cargo:rustc-link-lib=GLESv2");
    println!("cargo:rustc-link-lib=EGL");

    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib");
}
