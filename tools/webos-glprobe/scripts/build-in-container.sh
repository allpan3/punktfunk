#!/bin/sh
# Runs INSIDE the linux/arm64 container started by build.sh. Not meant to be run directly.
set -eu

TC=/tc/arm-webos-linux-gnueabi_sdk-buildroot
SR=$TC/arm-webos-linux-gnueabi/sysroot

echo "### host deps"
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
    build-essential pkg-config curl ca-certificates git cmake > /dev/null
# cc1plus of the SDK's gcc wants libisl at runtime; the package name moves between releases.
apt-get install -y -qq --no-install-recommends libisl23 > /dev/null 2>&1 \
    || apt-get install -y -qq --no-install-recommends libisl-dev > /dev/null 2>&1 || true

# The SDK's baked-in default sysroot points at a build-time path that no longer exists after
# relocate-sdk.sh, so every invocation passes --sysroot explicitly (same shim punktfunk-webos uses).
mkdir -p /shims
cat > /shims/cc <<EOF
#!/bin/sh
exec $TC/bin/arm-webos-linux-gnueabi-gcc --sysroot=$SR "\$@"
EOF
cat > /shims/cxx <<EOF
#!/bin/sh
exec $TC/bin/arm-webos-linux-gnueabi-g++ --sysroot=$SR "\$@"
EOF
chmod +x /shims/cc /shims/cxx

export CC_armv7_unknown_linux_gnueabi=/shims/cc
export CXX_armv7_unknown_linux_gnueabi=/shims/cxx
export AR_armv7_unknown_linux_gnueabi=$TC/bin/arm-webos-linux-gnueabi-gcc-ar
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABI_LINKER=/shims/cc
# Codegen only — real VFP/NEON instructions, softfp calling convention unchanged. Mirrors
# punktfunk-webos/.cargo/config.toml, where it was worth ~300ms -> ~30ms per render.
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABI_RUSTFLAGS="-C target-feature=+neon,+vfp3,-soft-float -C target-cpu=cortex-a73"

# SDL2 (the webosbrew fork overlaid onto the sysroot), plus freetype/fontconfig, which Skia's
# linux link_libraries() probes through pkg-config unconditionally.
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR=$SR
export PKG_CONFIG_LIBDIR=$SR/usr/lib/pkgconfig

# punktfunk-core's `quic` pulls audiopus_sys, which cross-builds vendored libopus with CMake.
export CMAKE_TOOLCHAIN_FILE_armv7_unknown_linux_gnueabi=$TC/share/buildroot/toolchainfile.cmake
export CMAKE_POLICY_VERSION_MINIMUM=3.5

if [ -z "${SKIA_BINARIES_URL:-}" ]; then
    export SKIA_BINARIES_URL='https://git.unom.io/unom/skia-binaries/releases/download/0.99.0/skia-binaries-{key}.tar.gz'
fi
echo "### skia archive template: $SKIA_BINARIES_URL"

# cd FIRST: this crate lives inside the repo, so the root's rust-toolchain.toml pins the
# toolchain here. Adding the target from anywhere else installs it for the default toolchain
# instead, and the build then fails with "can't find crate for `core`".
cd /repo/tools/webos-glprobe
rustup target add armv7-unknown-linux-gnueabi > /dev/null

cargo build --target armv7-unknown-linux-gnueabi --release

# 🛑 skia-bindings NEVER fails when no archive matches the key — it silently starts an
# hours-long source build. The confirmation is not on stdout under `cargo build -v` either, so
# read the build script's own output file.
OUTPUT=$(find /ptarget -path '*skia-bindings*' -name output 2>/dev/null | head -1)
if [ -n "$OUTPUT" ] && grep -q 'DOWNLOAD AND INSTALL SUCCEEDED' "$OUTPUT"; then
    echo "### skia: unpacked the prebuilt archive (no source build)"
elif [ -n "$OUTPUT" ]; then
    echo "### !! skia did NOT use a prebuilt archive — it built from source. Check the key:"
    grep -i 'download\|skia-binaries' "$OUTPUT" | head
fi

BIN=/ptarget/armv7-unknown-linux-gnueabi/release/pf-webos-glprobe
$TC/bin/arm-webos-linux-gnueabi-strip -o /out/pf-webos-glprobe "$BIN"
# The TV ships libstdc++.so.6.0.29 (GCC 11); the SDK builds against 6.0.30. One version short,
# so the deploy bundles the SDK's copy next to the app's libSDL2.
cp -L "$SR/usr/lib/libstdc++.so.6.0.30" /out/libstdc++.so.6
ls -l /out/pf-webos-glprobe /out/libstdc++.so.6
echo "### BUILD OK"
