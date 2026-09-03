# webos-glprobe — the shared console shell, on a TV

Draws `pf-console-ui` — the same crate the desktop session binary draws — with Skia's **GLES**
backend on an **SDL2** GL context, on an LG webOS TV. Step 1 of
`punktfunk-planning/design/webos-skia-console-port.md`.

Verified on an **OLED65G5 (webOS 10.3)**, 2026-09-03: the full "Select a Host" screen with the
mesh-aurora `RuntimeEffect`, card glow, focus ring and hint legend.

```
p50 1.9–2.2 ms    p90 2.3–2.9 ms    p99 3–5 ms    steady max ~7 ms
```

The webOS client's current `tiny-skia` CPU path costs **25–45 ms/frame**. That gap is the whole
argument for the port: a GPU canvas removes the reason the hybrid CPU-raster/SDL-composite
architecture exists.

## What it is for

It is a **probe**, not a client. No session, no NDL, no pairing — it builds the shell with empty
handles and renders it. Its value is in what it proves about the platform, and in being the only
in-tree build that exercises `pf-console-ui` with `--no-default-features --features gl`.

## Build

Needs Docker and the webosbrew SDK in a Docker volume. The SDK volume is the one
`punktfunk-webos` populates — from a checkout of that repo, `task toolchain:all` (it creates
`punktfunk-webos-toolchains`).

```sh
./scripts/build.sh                        # -> out/pf-webos-glprobe, out/libstdc++.so.6
```

Everything runs in a `linux/arm64` container: the webosbrew toolchain only ships a Linux-aarch64
build. Native on Apple Silicon, emulated elsewhere.

**Skia comes from a prebuilt archive.** rust-skia publishes no armv7 archive for any OS, so
`armv7-unknown-linux-gnueabi-gl-jpegd-jpege-pdf-textlayout` is one we cut ourselves. `build.sh`
defaults to the self-hosted release, which needs no credentials:

```
git.unom.io/unom/skia-binaries  tag 0.99.0
sha256 177178bfae46206b5963713e718e397f11b51ec143857397b24e59f6e2f626ac   18,367,399 bytes
```

To build against a local copy instead (e.g. while cutting the next bump's archives):

```sh
SKIA_BINARIES_URL='file:///abs/path/skia-binaries-{key}.tar.gz' ./scripts/build.sh
```

🛑 **skia-bindings never fails on a key that matches nothing** — it silently starts an hours-long
source build. `build.sh` checks for `DOWNLOAD AND INSTALL SUCCEEDED` and shouts if it is missing.
That string is *not* on stdout under `cargo build -v`; it lives in the build script's own
`output` file, which is where the check reads it.

## Run

Dev mode on, and an `ares` device registered as `tv` (or set `ARES_DEVICE`). Needs
`ares-launch` from [webosbrew/ares-cli-rs].

```sh
export TV_HOST=192.168.1.x
./scripts/deploy.sh deploy     # backs up the app's binary, then swaps ours in
./scripts/deploy.sh run
./scripts/deploy.sh log        # frame-time percentiles, GL/panel facts
./scripts/deploy.sh shot       # pull the framebuffer readback as a PNG
./scripts/deploy.sh restore    # put the real app binary back
```

**`restore` when you are done.** `deploy` overwrites the installed punktfunk-webos binary,
because webOS only composites for the SAM-managed foreground app and dev mode grants Luna's
`public` group by executable path — a probe anywhere else renders to a black screen. The original
is kept as `punktfunk-webos.orig`.

D-pad moves, Enter confirms, **Home** backgrounds it. Unmapped keycodes are logged, so pressing
the colour buttons tells you what the remote actually sends.

## What it settled

- **Skia GLES works here.** SDL2's webosbrew fork gives a GLES2 context with `alpha 8` and an
  8-bit stencil on the `wayland` driver; Skia wraps FBO 0 over it.
- **1080p is a platform ceiling, not a choice.** `SDL_webOSGetPanelResolution` reports
  `3840x2160`, but asking for a window that size still yields a `1920x1080` drawable — webOS
  hands native apps a 1080p logical surface and upscales. Video is unaffected: NDL decodes into
  its own hardware plane, which is why decode dimensions must stay decoupled from the
  punch-through rect.
- **libstdc++ must be bundled.** The TV ships `6.0.29` (GCC 11); the SDK builds against `6.0.30`.
  One version short, so `deploy` copies the SDK's beside the app's `libSDL2` — the existing
  `$ORIGIN/../lib` rpath already covers it.
- **`getauxval` is the only link gap**, and `punktfunk-webos`'s existing `glibc_compat_shim.c`
  (vendored here) already fixes it. Skia's bundled zlib needs it for the same reason libstd does.

## What it did NOT settle

- **Alpha over live NDL video.** The context *has* an alpha channel, but this probe plays no
  video, so whether the compositor blends it over the video plane is untested. That is the
  remaining gate on an in-stream OSD; the pre-stream shell works either way.
- **RAM headroom.** ~168 MB RSS here, alongside no decoder. A real client adds NDL's buffers.

[webosbrew/ares-cli-rs]: https://github.com/webosbrew/ares-cli-rs
