//! On-glass probe: the shared `pf-console-ui` shell, drawn by Skia's GLES backend on an
//! SDL2 GL context, on an LG TV.
//!
//! Answers the two questions §5 of the design doc says only glass can:
//!   1. can SDL2's webosbrew fork hand out a GL context with a real alpha channel, and
//!   2. what does Ganesh cost per frame on this panel, with the shell's blurs and its
//!      mesh-aurora runtime effect.
//!
//! Frame times print as a rolling summary rather than per frame — a per-frame log on this
//! device costs more than the frame it measures.

use anyhow::{anyhow, Result};
use pf_console_ui::{Console, ConsoleEntry, ConsoleHandles, ConsoleOptions, InputSource, Viewport};
use skia_safe::gpu::{self, DirectContext, SurfaceOrigin};
use skia_safe::{ColorType, Surface};
use std::time::Instant;

/// stdout AND `$HOME/glprobe.log`: SAM launches the binary directly, so stdout goes nowhere
/// reachable, and the app jail has no pty to attach to.
macro_rules! logln {
    ($($a:tt)*) => {{
        let line = format!($($a)*);
        println!("{line}");
        log_to_file(&line);
    }};
}

pub fn log_to_file(line: &str) {
    use std::io::Write;
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{home}/glprobe.log"))
    {
        let _ = writeln!(f, "{line}");
    }
}

// The webosbrew fork's own entry point, absent from the `sdl2` crate. `current_display_mode`
// reports the app's LOGICAL output (1080p on every panel); this reports the physical one, which
// is how we tell "webOS upscales us" from "we asked for the wrong size".
extern "C" {
    fn SDL_webOSGetPanelResolution(
        w: *mut std::ffi::c_int,
        h: *mut std::ffi::c_int,
    ) -> std::ffi::c_int;
}

fn panel_resolution() -> Option<(i32, i32)> {
    let (mut w, mut h) = (0, 0);
    // SAFETY: both pointers are to live locals, and the fork's symbol is linked by libSDL2.
    let ok = unsafe { SDL_webOSGetPanelResolution(&mut w, &mut h) };
    (ok != 0 && w > 0 && h > 0).then_some((w, h))
}

/// Sized internal format of the RGBA8888 default framebuffer, same constant Android's host uses.
const GL_RGBA8: u32 = 0x8058;

/// A 2020-era TV shares one small pool with the decoder; a quarter of the desktop budget.
const GPU_CACHE_BYTES: usize = 64 << 20;

fn main() {
    if let Err(e) = run() {
        logln!("glprobe: FAILED: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // The client claims Back/Home/Guide so no input can leak to the TV OS mid-stream. A probe
    // on someone's living-room television wants the opposite for HOME: leaving it unclaimed
    // keeps the remote's Home button working as an escape hatch, so the app can always be
    // backgrounded from the couch. Back is still claimed, and mapped to quit below.
    sdl2::hint::set("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true");
    sdl2::hint::set("SDL_WEBOS_ACCESS_POLICY_KEYS_GUIDE", "true");
    sdl2::hint::set("SDL_WEBOS_ACCESS_POLICY_RIBBON", "false");

    let sdl = sdl2::init().map_err(|e| anyhow!("SDL_Init: {e}"))?;
    let video = sdl.video().map_err(|e| anyhow!("SDL video: {e}"))?;
    logln!("glprobe: video driver = {}", video.current_video_driver());

    let mode = video
        .current_display_mode(0)
        .map_err(|e| anyhow!("current_display_mode: {e}"))?;
    logln!(
        "glprobe: display {}x{}@{}",
        mode.w,
        mode.h,
        mode.refresh_rate
    );

    // GLES2 is what this panel exposes, and Skia's GL backend supports it. ALPHA_SIZE 8 is
    // the half that matters beyond drawing: unknown 1 is whether the webOS compositor honours
    // per-pixel alpha on a GL surface, which decides if an in-stream OSD over NDL is possible
    // at all. A pre-stream shell would work either way.
    let attr = video.gl_attr();
    attr.set_context_profile(sdl2::video::GLProfile::GLES);
    attr.set_context_version(2, 0);
    attr.set_red_size(8);
    attr.set_green_size(8);
    attr.set_blue_size(8);
    attr.set_alpha_size(8);
    attr.set_stencil_size(8);
    attr.set_double_buffer(true);

    // Ask for the PANEL's size, not the logical output's. If webOS hands back a 1080p drawable
    // anyway, the compositor is upscaling us and 4K is simply not on offer to a native app —
    // which is worth knowing precisely, because at 1080p every glyph on a 65" panel is scaled.
    let panel = panel_resolution();
    match panel {
        Some((pw, ph)) => logln!(
            "glprobe: panel resolution {pw}x{ph} (logical output {}x{})",
            mode.w,
            mode.h
        ),
        None => logln!("glprobe: SDL_webOSGetPanelResolution unavailable"),
    }
    let (want_w, want_h) = panel.unwrap_or((mode.w, mode.h));

    let window = video
        .window("punktfunk-glprobe", want_w as u32, want_h as u32)
        .opengl()
        .fullscreen()
        .build()
        .map_err(|e| anyhow!("create window: {e}"))?;

    let _ctx = window
        .gl_create_context()
        .map_err(|e| anyhow!("gl_create_context: {e}"))?;
    window
        .subsystem()
        .gl_set_swap_interval(sdl2::video::SwapInterval::VSync)
        .ok();

    logln!(
        "glprobe: GL context up — profile {:?}, alpha bits {}, stencil bits {}",
        attr.context_profile(),
        attr.alpha_size(),
        attr.stencil_size()
    );

    // Load through SDL's resolver rather than Skia's `new_native()`: on this device the
    // native assembler has no reason to find webOS's loader, and SDL already knows it.
    let interface = gpu::gl::Interface::new_load_with(|name| {
        video.gl_get_proc_address(name) as *const std::ffi::c_void
    })
    .ok_or_else(|| anyhow!("Skia: could not assemble a GL interface from SDL's resolver"))?;
    let mut context = gpu::direct_contexts::make_gl(interface, None)
        .ok_or_else(|| anyhow!("Skia: DirectContext over GLES failed"))?;
    context.set_resource_cache_limit(GPU_CACHE_BYTES);
    logln!(
        "glprobe: Skia DirectContext up, {} MB budget",
        GPU_CACHE_BYTES >> 20
    );

    let (w, h) = window.drawable_size();
    let mut surface = wrap_fb0(&mut context, w, h, attr.stencil_size() as usize)?;
    logln!(
        "glprobe: asked {want_w}x{want_h}, GOT drawable {w}x{h} — Skia surface {}x{}",
        surface.width(),
        surface.height()
    );

    let handles = ConsoleHandles::new();
    let mut opts = ConsoleOptions::desktop("punktfunk-glprobe".into(), false);
    opts.gpu_cache_bytes = GPU_CACHE_BYTES;
    let mut console = Console::new(opts, ConsoleEntry::Home, &handles)?;
    logln!("glprobe: console built — entering frame loop");

    let viewport = Viewport::plain(w, h);
    let mut events = sdl.event_pump().map_err(|e| anyhow!("event pump: {e}"))?;
    let mut times: Vec<f32> = Vec::with_capacity(256);
    let started = Instant::now();
    let mut frames: u64 = 0;

    'outer: loop {
        for ev in events.poll_iter() {
            use sdl2::event::Event;
            use sdl2::keyboard::Keycode;
            match ev {
                Event::Quit { .. } => break 'outer,
                Event::KeyDown {
                    keycode: Some(k), ..
                } => {
                    // Only a real keyboard's Escape quits. The remote's Back is NOT an exit:
                    // something on this device delivers it unprompted within seconds of
                    // launch, which killed the first live run. Home is left unclaimed above,
                    // so backgrounding the app from the couch is the escape hatch instead.
                    if k == Keycode::Escape {
                        logln!("glprobe: Escape — exiting");
                        break 'outer;
                    }
                    match menu_event(k) {
                        Some(ev) => {
                            console.menu(ev, InputSource::Keys);
                        }
                        // Learn the remote's vocabulary: webOS's colour and nav keys are
                        // scancode-less keycodes that are not in vanilla SDL2's table.
                        None => logln!("glprobe: unmapped keycode {}", i32::from(k)),
                    }
                }
                _ => {}
            }
        }

        let t0 = Instant::now();
        {
            let canvas = surface.canvas();
            console.frame(canvas, &viewport, None, None, &[]);
        }
        context.flush_and_submit();
        let cpu_ms = t0.elapsed().as_secs_f32() * 1000.0;

        window.gl_swap_window();

        times.push(cpu_ms);
        frames += 1;

        // Read the panel's own framebuffer back once the shell has settled (fonts resolved,
        // first layout done) — the only way to SEE what the TV drew without a camera.
        if frames == 90 {
            match snapshot(&mut surface, &mut context) {
                Ok(bytes) => logln!("glprobe: wrote glprobe.png ({bytes} bytes)"),
                Err(e) => logln!("glprobe: snapshot failed: {e:#}"),
            }
        }

        if times.len() == 600 {
            report(&mut times);
        }
    }

    if !times.is_empty() {
        report(&mut times);
    }
    logln!(
        "glprobe: {} frames in {:.1}s — done",
        frames,
        started.elapsed().as_secs_f32()
    );
    Ok(())
}

/// PNG of what is actually on the panel, next to the log in the app's own directory.
fn snapshot(surface: &mut Surface, context: &mut DirectContext) -> Result<usize> {
    let image = surface.image_snapshot();
    let data = image
        .encode(&mut *context, skia_safe::EncodedImageFormat::PNG, 100)
        .ok_or_else(|| anyhow!("PNG encode returned nothing"))?;
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::fs::write(format!("{home}/glprobe.png"), data.as_bytes())?;
    Ok(data.len())
}

/// Skia surface over the window's default framebuffer (SDL owns the swap).
fn wrap_fb0(context: &mut DirectContext, w: u32, h: u32, stencil: usize) -> Result<Surface> {
    let fb = gpu::gl::FramebufferInfo {
        fboid: 0,
        format: GL_RGBA8,
        protected: gpu::Protected::No,
    };
    let target = gpu::backend_render_targets::make_gl((w as i32, h as i32), None, stencil, fb);
    gpu::surfaces::wrap_backend_render_target(
        context,
        &target,
        SurfaceOrigin::BottomLeft,
        ColorType::RGBA8888,
        None,
        None,
    )
    .ok_or_else(|| anyhow!("Skia: could not wrap the window framebuffer"))
}

/// Percentiles, not a mean: this is about the worst frames, which is where a blur or the
/// aurora runtime effect would show up.
fn report(times: &mut Vec<f32>) {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = times.len();
    let at = |p: f32| times[((n as f32 * p) as usize).min(n - 1)];
    logln!(
        "glprobe: cpu-side frame ms over {n}: p50 {:.2}  p90 {:.2}  p99 {:.2}  max {:.2}",
        at(0.50),
        at(0.90),
        at(0.99),
        times[n - 1]
    );
    times.clear();
}

fn menu_event(k: sdl2::keyboard::Keycode) -> Option<pf_client_core::menu_nav::MenuEvent> {
    use pf_client_core::menu_nav::{MenuDir, MenuEvent};
    use sdl2::keyboard::Keycode as K;
    Some(match k {
        K::Up => MenuEvent::Move(MenuDir::Up),
        K::Down => MenuEvent::Move(MenuDir::Down),
        K::Left => MenuEvent::Move(MenuDir::Left),
        K::Right => MenuEvent::Move(MenuDir::Right),
        K::Return | K::KpEnter => MenuEvent::Confirm,
        K::Backspace => MenuEvent::Back,
        _ => return None,
    })
}
