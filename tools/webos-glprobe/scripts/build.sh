#!/bin/sh
# Cross-build the probe for a webOS TV, in Docker.
#
# Everything runs in a linux/arm64 container because the webosbrew toolchain only ships a
# Linux-aarch64 build. On an Apple Silicon Mac that is native; elsewhere Docker emulates it.
#
#   ./scripts/build.sh
#
# Env:
#   SKIA_BINARIES_URL  Template for the prebuilt Skia archive ({key} is substituted). Defaults
#                      to our self-hosted release. Point it at a `file://` path to use a local
#                      archive — see README.md, and note the override skips digest checking.
#   TOOLCHAIN_VOLUME   Docker volume holding the webosbrew SDK (default punktfunk-webos-toolchains).
#                      Populated by punktfunk-webos's `task toolchain:all`.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
PROBE=$(dirname "$HERE")
REPO=$(cd "$PROBE/../.." && pwd)

TOOLCHAIN_VOLUME=${TOOLCHAIN_VOLUME:-punktfunk-webos-toolchains}
OUT=${OUT:-$PROBE/out}
mkdir -p "$OUT"

if ! docker volume inspect "$TOOLCHAIN_VOLUME" >/dev/null 2>&1; then
    echo "no docker volume '$TOOLCHAIN_VOLUME' — see README.md for how to populate it" >&2
    exit 1
fi

# rust:trixie, NOT bookworm. clang 16 cannot compile GCC 12's libstdc++ <ranges> and dies on
# Skia's SkPDFTag.cpp; clang 19 builds it. Only matters for a source build, but the container
# is also what compiles this crate, so keep them the same.
docker run --rm --platform linux/arm64 \
    -e CARGO_TARGET_DIR=/ptarget \
    -e "SKIA_BINARIES_URL=${SKIA_BINARIES_URL:-}" \
    -v "$TOOLCHAIN_VOLUME:/tc" \
    -v "$REPO:/repo" \
    -v "$OUT:/out" \
    -v pf-webos-glprobe-cargo:/usr/local/cargo/registry \
    -v pf-webos-glprobe-target:/ptarget \
    rust:trixie /bin/sh /repo/tools/webos-glprobe/scripts/build-in-container.sh
