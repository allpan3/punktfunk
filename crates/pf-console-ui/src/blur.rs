//! Progressive backdrop blur: what lies under a chrome band blurs by a radius that grows
//! linearly from nothing at the content edge to full strength at the screen edge, as
//! Glur's mask ramps it.
//!
//! The per-pixel variable Gaussian Glur's shader runs — each row's σ read off its depth
//! into the band — not one blurred copy faded in by a gradient. It runs on a quarter-size
//! copy of the band (two exact 2×2 box halvings), then scales back up, which makes it about
//! sixty times cheaper than full resolution. Where σ is still under that copy's own
//! softness, a few pixels from the clear edge, the result fades back to the sharp backdrop.
//! Pinned by `a_band_blurs_more_toward_its_edge`; costs are on the PR.

use skia_safe::{
    canvas::SaveLayerRec, image_filters, BlendMode, Canvas, FilterMode, ImageFilter, Matrix, Rect,
    RuntimeEffect, SamplingOptions,
};
use std::cell::OnceCell;

/// Downsample factor: two halvings.
const DOWN: f32 = 4.0;

const SKSL: &str = r#"
uniform shader src;
uniform float2 dir;
uniform float edge;
uniform float clear;
uniform float sigma;
uniform float2 lo;
uniform float2 hi;
uniform float fade;

half4 main(float2 p) {
    float t = clamp((clear - p.y) / (clear - edge), 0.0, 1.0);
    float s = sigma * t;
    half4 acc = src.eval(p);
    if (s >= 0.35) {
        float reach = ceil(3.0 * s);
        acc = half4(0.0);
        float wsum = 0.0;
        for (int i = -12; i <= 12; i++) {
            float fi = float(i);
            if (abs(fi) <= reach) {
                float w = exp(-fi * fi / (2.0 * s * s));
                acc += src.eval(clamp(p + dir * fi, lo, hi)) * half(w);
                wsum += w;
            }
        }
        acc /= half(wsum);
    }
    return acc * half(fade > 0.0 ? smoothstep(0.0, fade, s) : 1.0);
}
"#;

thread_local! {
    static EFFECT: OnceCell<Option<RuntimeEffect>> = const { OnceCell::new() };
}

/// Where the band is and how hard it blurs, device px.
#[derive(Clone, Copy, Debug)]
pub struct Band {
    /// The screen-side edge: full strength here.
    pub edge: f32,
    /// The content-side edge: untouched here.
    pub clear: f32,
    /// Full-strength σ.
    pub sigma: f32,
}

/// One separable pass over `rect`, in the quarter-size copy's coordinates. Taps clamp to
/// `rect`: past it the second pass would read the unblurred backdrop. `fade` > 0 scales the
/// output down to nothing as σ falls under it.
fn pass(
    band: Band,
    rect: Rect,
    dir: (f32, f32),
    fade: f32,
    input: Option<ImageFilter>,
) -> Option<ImageFilter> {
    let effect = EFFECT.with(|e| {
        e.get_or_init(|| RuntimeEffect::make_for_shader(SKSL, None).ok())
            .clone()
    })?;
    let mut b = skia_safe::runtime_effect::RuntimeShaderBuilder::new(effect);
    b.set_uniform_float("dir", &[dir.0, dir.1]).ok()?;
    b.set_uniform_float("edge", &[band.edge]).ok()?;
    b.set_uniform_float("clear", &[band.clear]).ok()?;
    b.set_uniform_float("sigma", &[band.sigma]).ok()?;
    b.set_uniform_float("lo", &[rect.left + 0.5, rect.top + 0.5])
        .ok()?;
    b.set_uniform_float("hi", &[rect.right - 0.5, rect.bottom - 0.5])
        .ok()?;
    b.set_uniform_float("fade", &[fade]).ok()?;
    image_filters::runtime_shader(&b, "src", input)
}

/// Blur what `canvas` already holds under `rect` by `band`. Skipped under the reduced
/// interface: each read-back ends a tiled GPU's render pass, more than a TV GPU has.
pub fn backdrop(canvas: &Canvas, rect: Rect, band: Band) {
    if crate::theme::reduced_ui() {
        return;
    }
    let Some(filter) = filter(rect, band) else {
        return;
    };
    canvas.save_layer(&SaveLayerRec::default().bounds(&rect).backdrop(&filter));
    canvas.restore();
}

fn filter(rect: Rect, band: Band) -> Option<ImageFilter> {
    let (ox, oy) = (rect.left, rect.top);
    // About the band's top-left, so the small copy starts where the band does.
    let about = |k: f32| {
        let mut m = Matrix::translate((ox, oy));
        m.pre_scale((k, k), None);
        m.pre_translate((-ox, -oy));
        m
    };
    // Exact halvings: each output pixel centre lands on a 2×2 corner, so bilinear is a box.
    let linear = SamplingOptions::from(FilterMode::Linear);
    let half = image_filters::matrix_transform(&about(0.5), linear, None);
    let small = image_filters::matrix_transform(&about(0.5), linear, half);
    let to_small = |y: f32| oy + (y - oy) / DOWN;
    let b = Band {
        edge: to_small(band.edge),
        clear: to_small(band.clear),
        sigma: band.sigma / DOWN,
    };
    let r = Rect::from_xywh(ox, oy, rect.width() / DOWN, rect.height() / DOWN);
    let x = pass(b, r, (1.0, 0.0), 0.0, small);
    // Under ~0.8 px here the upscaled copy is softer than the blur it stands for.
    let y = pass(b, r, (0.0, 1.0), 0.8, x);
    let up = image_filters::matrix_transform(&about(DOWN), linear, y);
    image_filters::blend(BlendMode::SrcOver, None, up, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use skia_safe::Color;

    /// 8 px stripes under a top band: rows at the clear edge keep full contrast, rows at
    /// the screen edge lose nearly all of it, and contrast falls steadily in between.
    #[test]
    fn a_band_blurs_more_toward_its_edge() {
        let mut surface = skia_safe::surfaces::raster_n32_premul((64, 100)).unwrap();
        let canvas = surface.canvas();
        canvas.clear(Color::BLACK);
        let white = crate::theme::fill(skia_safe::Color4f::new(1.0, 1.0, 1.0, 1.0));
        for x in (0..64).step_by(16) {
            canvas.draw_rect(Rect::from_xywh(x as f32, 0.0, 8.0, 100.0), &white);
        }
        let band = Band {
            edge: 0.0,
            clear: 80.0,
            sigma: 8.0,
        };
        backdrop(canvas, Rect::from_xywh(0.0, 0.0, 64.0, 100.0), band);
        let px = surface.image_snapshot();
        let info = px.image_info();
        let mut bytes = vec![0u8; info.compute_min_byte_size()];
        assert!(px.read_pixels(
            info,
            &mut bytes,
            info.min_row_bytes(),
            (0, 0),
            skia_safe::image::CachingHint::Allow
        ));
        let contrast = |y: usize| {
            let row = &bytes[y * info.min_row_bytes()..];
            let lum = |x: usize| i32::from(row[x * 4 + 1]);
            (lum(20) - lum(28)).abs()
        };
        assert_eq!(contrast(90), 255, "below the band nothing moves");
        assert_eq!(contrast(79), 255, "the clear edge is untouched");
        assert!(
            contrast(5) < 10,
            "the screen edge is fully blurred: {}",
            contrast(5)
        );
        let ramp: Vec<i32> = [74, 68, 62, 56, 50].into_iter().map(contrast).collect();
        assert!(
            ramp.windows(2).all(|w| w[0] > w[1]),
            "contrast falls into the band: {ramp:?}"
        );
    }
}
