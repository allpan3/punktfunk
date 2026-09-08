//! Modal overlays: connecting, waking, toast, full-screen takeover.

use crate::anim::{approach, springs};
use crate::glyphs::{hint_bar, Hint, HintKey};
use crate::library::{card_matrix, PERSPECTIVE};
use crate::theme::{fg, fill, Fonts, PanelStroke, W};
use skia_safe::{gradient, Canvas, Color4f, Image, PathBuilder, Point, RRect, Rect, TileMode, M44};

use super::{Launching, Shell, ToastMark, BOTTOM_BAND};
use crate::model::SpeedPhase;

/// Kind mark in a 13 dp box.
fn draw_toast_mark(canvas: &Canvas, mark: ToastMark, cx: f64, cy: f64, k: f64, ink: Color4f) {
    let mut p = fill(ink);
    match mark {
        ToastMark::Dot => {
            canvas.draw_circle((cx as f32, cy as f32), (3.4 * k) as f32, &p);
        }
        ToastMark::Check => {
            p.set_style(skia_safe::PaintStyle::Stroke);
            p.set_stroke_width((1.9 * k) as f32);
            p.set_stroke_cap(skia_safe::PaintCap::Round);
            p.set_stroke_join(skia_safe::PaintJoin::Round);
            let r = 5.0 * k;
            let mut path = PathBuilder::new();
            path.move_to(((cx - r) as f32, cy as f32));
            path.line_to(((cx - r * 0.25) as f32, (cy + r * 0.7) as f32));
            path.line_to(((cx + r) as f32, (cy - r * 0.7) as f32));
            canvas.draw_path(&path.detach(), &p);
        }
        ToastMark::Bang => {
            // Stem + dot, not a text "!": at 0.75× k a font bang is two pixels and vanishes.
            let r = 5.4 * k;
            let w = 2.0 * k;
            canvas.draw_rrect(
                skia_safe::RRect::new_rect_xy(
                    Rect::from_xywh(
                        (cx - w / 2.0) as f32,
                        (cy - r) as f32,
                        w as f32,
                        (r * 1.35) as f32,
                    ),
                    (w / 2.0) as f32,
                    (w / 2.0) as f32,
                ),
                &p,
            );
            canvas.draw_circle((cx as f32, (cy + r * 0.72) as f32), (w * 0.62) as f32, &p);
        }
    }
}

impl Shell {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::shell) fn draw_overlays(
        &mut self,
        canvas: &Canvas,
        w: f64,
        h: f64,
        k: f64,
        dt: f64,
        t: f64,
        fonts: &Fonts,
    ) {
        // Resolved before the chain below: naming the layer Apply writes to reads the
        // whole shell, which the chain's `&mut self.connecting` arm would forbid.
        let pinned_by = self
            .speed
            .as_ref()
            .and_then(|sp| self.speed_pinned_by(&sp.key))
            .map(str::to_string);
        // Connect, wake and the speed test share one full-screen shape. A connect can
        // follow a wake (`sync`) so they share the backdrop and never blink between them.
        let takeover: Option<(f64, bool, String, String, Vec<Hint>)> =
            if let Some(c) = &mut self.connecting {
                c.appear = approach(c.appear, 1.0, dt, 0.07);
                if c.request_access {
                    Some((
                        c.appear,
                        true,
                        "Waiting for approval…".to_string(),
                        format!(
                            "Approve this device in {}'s console or web UI — no PIN needed.",
                            c.title
                        ),
                        vec![Hint::new(HintKey::Back, "Cancel")],
                    ))
                } else {
                    Some((
                        c.appear,
                        true,
                        format!("Connecting to {}…", c.title),
                        "Starting the stream in this window.".to_string(),
                        vec![Hint::new(HintKey::Back, "Cancel")],
                    ))
                }
            } else if let Some(wk) = &self.wake {
                // Service-driven: already settled, no fade-in.
                if wk.timed_out {
                    Some((
                        1.0,
                        false,
                        format!("{} didn't wake", wk.name),
                        "Check its power settings, or wake it manually and try again.".to_string(),
                        vec![
                            Hint::new(HintKey::Confirm, "Try Again"),
                            Hint::new(HintKey::Back, "Cancel"),
                        ],
                    ))
                } else {
                    Some((
                        1.0,
                        true,
                        format!("Waking {}…", wk.name),
                        format!("Waiting for it to come online · {} s", wk.seconds),
                        // Wake-only offers "Stop Waiting"; wake-then-connect is "Cancel".
                        vec![Hint::new(
                            HintKey::Back,
                            if wk.then_connect {
                                "Cancel"
                            } else {
                                "Stop Waiting"
                            },
                        )],
                    ))
                }
            } else if let Some(sp) = &self.speed {
                // Service-driven like the wake card: already settled, no fade-in.
                let close = Hint::new(HintKey::Back, "Close");
                Some(match &sp.phase {
                    SpeedPhase::Connecting => (
                        1.0,
                        true,
                        format!("Testing {}\u{2026}", sp.name),
                        "Connecting.".to_string(),
                        vec![Hint::new(HintKey::Back, "Cancel")],
                    ),
                    SpeedPhase::Measuring => (
                        1.0,
                        true,
                        format!("Testing {}\u{2026}", sp.name),
                        "Measuring the link \u{2014} this takes two seconds.".to_string(),
                        vec![Hint::new(HintKey::Back, "Cancel")],
                    ),
                    SpeedPhase::Failed(why) => (
                        1.0,
                        false,
                        format!("Couldn't measure {}", sp.name),
                        why.clone(),
                        vec![close],
                    ),
                    SpeedPhase::Done {
                        throughput_kbps,
                        loss_pct,
                        recommended_kbps,
                    } => {
                        let measured = format!(
                            "{} Mb/s \u{b7} {loss_pct:.1} % loss",
                            throughput_kbps / 1_000
                        );
                        match &pinned_by {
                            // Read-only: the default is not the layer this host streams at.
                            Some(name) => (
                                1.0,
                                false,
                                measured,
                                format!(
                                    "\u{201c}{name}\u{201d} sets this host's bitrate \u{2014} \
                                     change it there to use this."
                                ),
                                vec![close],
                            ),
                            None => (
                                1.0,
                                false,
                                measured,
                                format!(
                                    "{} Mb/s recommended, leaving headroom for FEC and loss.",
                                    recommended_kbps / 1_000
                                ),
                                vec![Hint::new(HintKey::Confirm, "Set as the default"), close],
                            ),
                        }
                    }
                })
            } else {
                None
            };
        if let Some((appear, spinner, title, body, hints)) = takeover {
            self.draw_takeover(
                canvas, w, h, k, appear, t, fonts, spinner, &title, &body, &hints,
            );
        }
        if let Some(l) = &mut self.launching {
            l.appear = approach(l.appear, 1.0, dt, 0.09);
            l.flight.step_spec(1.0, crate::anim::springs::LAUNCH, dt);
            l.flight.settle(1.0, 0.001, 0.01);
        }
        if let Some(l) = &self.launching {
            // The shelf that launched it still holds its decoded poster underneath.
            let poster = match self.stack.last() {
                Some(crate::screens::Screen::Library(lib)) => lib.poster(&l.host.id),
                _ => None,
            };
            self.draw_launch_hold(canvas, w, h, k, t, fonts, l, poster);
        }

        if self.toast.as_ref().is_some_and(|toast| t - toast.at > 4.0) {
            self.toast = None;
        }
        if let Some(toast) = &mut self.toast {
            let age = t - toast.at;
            if crate::theme::reduce_motion() {
                toast.seat = crate::anim::Spring::rest(1.0);
            } else {
                toast.seat.step_spec(1.0, springs::INDICATOR, dt);
                toast.seat.settle(1.0, 0.001, 0.01);
            }
            // Seat springs the slide. Fade stays linear: dismissal is a 4 s deadline, not a gesture.
            let slide = toast.seat.pos.clamp(0.0, 1.0);
            let fade = if age > 3.4 {
                (1.0 - (age - 3.4) / 0.6).max(0.0)
            } else {
                1.0
            };
            let alpha = (slide * fade) as f32;
            let (tint, mark) = toast.kind.look();
            let size = 13.0 * k;
            let tw = f64::from(fonts.measure(&toast.text, W::Medium, size));
            let (pad_x, bh) = (16.0 * k, 34.0 * k);
            // Kind mark, then text. Pad is 13 dp (shy of `pad_x`) because the 13 dp mark
            // never fills its box; equal pad leaves the pill left-heavy.
            let (mark_pad, mark_w, gap) = (13.0 * k, 13.0 * k, 9.0 * k);
            let lead = mark_pad + mark_w + gap;
            let bw = lead + tw + pad_x;
            let bx = (w - bw) / 2.0;
            let by = h - BOTTOM_BAND * k - bh - 8.0 * k + (1.0 - slide) * 12.0 * k;
            let rect = Rect::from_xywh(bx as f32, by as f32, bw as f32, bh as f32);
            // Bound the fade layer to the pill. Unbounded `save_layer` is a full-surface
            // offscreen every frame. 12 k outset is stroke slack (no blur to reach further).
            let bounds = rect.with_outset((12.0 * k as f32, 12.0 * k as f32));
            canvas.save_layer_alpha_f(Some(bounds), alpha);
            canvas.draw_rrect(
                skia_safe::RRect::new_rect_xy(rect, (bh / 2.0) as f32, (bh / 2.0) as f32),
                &fill(crate::theme::shade(0.6)),
            );
            crate::theme::panel(
                canvas,
                rect,
                (bh / 2.0 / k) as f32,
                None,
                PanelStroke::Plain(0.14),
                k as f32,
            );
            let cy = by + bh / 2.0;
            draw_toast_mark(canvas, mark, bx + mark_pad + mark_w / 2.0, cy, k, tint);
            fonts.draw(
                canvas,
                &toast.text,
                bx + lead,
                cy + size * 0.36,
                W::Medium,
                size,
                fg(0.92),
            );
            canvas.restore();
        }
    }
    /// Full-screen connect/wake takeover: aurora, optional spinner, title, one
    /// detail line, own hint row. `appear` = 1.0 when a wake hands off to a connect
    /// so the two never blink. Not a centered card.
    #[allow(clippy::too_many_arguments)]
    fn draw_takeover(
        &self,
        canvas: &Canvas,
        w: f64,
        h: f64,
        k: f64,
        appear: f64,
        t: f64,
        fonts: &Fonts,
        spinner: bool,
        title: &str,
        body: &str,
        hints: &[Hint],
    ) {
        let cx = w / 2.0;
        // Only while it is arriving: an unbounded layer is a full-screen offscreen per frame,
        // and `appear` is at 1.0 within half a second of a hold that runs for many.
        if appear < 0.999 {
            canvas.save_layer_alpha_f(None, appear as f32);
        } else {
            canvas.save();
        }
        self.draw_takeover_field(canvas, w, h, t);

        let title_y = h / 2.0 + if spinner { 14.0 * k } else { 0.0 };
        if spinner {
            crate::theme::spinner(canvas, cx, title_y - 52.0 * k, 22.0 * k, t);
        }
        fonts.centered(
            canvas,
            title,
            W::SemiBold,
            23.0 * k,
            fg(1.0),
            cx,
            title_y,
            w * 0.82,
        );
        if !body.is_empty() {
            fonts.centered(
                canvas,
                body,
                W::Regular,
                14.0 * k,
                fg(0.55),
                cx,
                title_y + 32.0 * k,
                w * 0.66,
            );
        }
        self.draw_takeover_hints(canvas, w, h, k, fonts, hints);
        canvas.restore();
    }

    /// The takeover's ground: an opaque aurora — the home field, so this reads as the
    /// console taking over — with a shade pool under the centre so text separates from a
    /// bright field.
    fn draw_takeover_field(&self, canvas: &Canvas, w: f64, h: f64, t: f64) {
        self.draw_aurora(canvas, w, h, t, 0.0);
        let mut vignette = crate::theme::shaded();
        let shades = [crate::theme::shade(0.5), crate::theme::shade(0.0)];
        vignette.set_shader(gradient::shaders::radial_gradient(
            (
                Point::new((w / 2.0) as f32, (h / 2.0) as f32),
                (w.max(h) * 0.42) as f32,
            ),
            &gradient::Gradient::new(
                gradient::Colors::new_evenly_spaced(&shades, TileMode::Clamp, None),
                gradient::Interpolation::default(),
            ),
            None,
        ));
        canvas.draw_rect(Rect::from_wh(w as f32, h as f32), &vignette);
    }

    /// The takeover's legend, centered where every console screen's sits.
    fn draw_takeover_hints(
        &self,
        canvas: &Canvas,
        w: f64,
        h: f64,
        k: f64,
        fonts: &Fonts,
        hints: &[Hint],
    ) {
        if hints.is_empty() {
            return;
        }
        let probe = hint_bar(canvas, fonts, hints, self.glyphs, -10_000.0, -10_000.0, k);
        hint_bar(
            canvas,
            fonts,
            hints,
            self.glyphs,
            w / 2.0 - probe.size.0 / 2.0,
            h - 34.0 * k,
            k,
        );
    }

    /// The launch hold: the title's poster on the takeover field, its name and store
    /// beneath, a spinner for the wait. `appear` lifts the poster group in; the field
    /// itself is already on screen from the connect, so it never blinks.
    #[allow(clippy::too_many_arguments)]
    fn draw_launch_hold(
        &self,
        canvas: &Canvas,
        w: f64,
        h: f64,
        k: f64,
        t: f64,
        fonts: &Fonts,
        l: &Launching,
        poster: Option<&Image>,
    ) {
        // Fades in rather than replacing the shelf outright: the cover has to be seen
        // LEAVING its tile, which means the tile has to still be there when it does.
        if l.appear < 0.999 {
            canvas.save_layer_alpha_f(None, l.appear as f32);
        } else {
            canvas.save();
        }
        self.draw_takeover_field(canvas, w, h, t);
        canvas.restore();

        // Cover and column as one centred pair: a 2:3 card carrying the left, and a text
        // column beside it wide enough for a long title to break twice, not fifteen times.
        let ch = (h * 0.62).min(460.0 * k);
        let cw = ch * 2.0 / 3.0;
        let gap = (w * 0.04).min(56.0 * k);
        let dw = (w * 0.34).clamp(260.0 * k, 460.0 * k);
        let x0 = (w - (cw + gap + dw)) / 2.0;
        let top = (h - ch) / 2.0;
        let settled = Rect::from_xywh(x0 as f32, top as f32, cw as f32, ch as f32);
        // Where it flies from: its shelf tile, or — with no tile to leave — the settled rect
        // a little small, so the arrival still reads as one.
        let start = if l.from.is_empty() {
            settled.with_inset((settled.width() * 0.07, settled.height() * 0.07))
        } else {
            l.from
        };
        let p = l.flight.pos;
        let lerp = |a: f64, b: f64| a + (b - a) * p;

        canvas.save();
        // One full turn on the way over, about the card's own vertical axis and through the
        // same projection the coverflow tilts its side cards with — so the near edge grows
        // and the far one recedes instead of the face merely narrowing.
        let m = card_matrix(
            lerp(f64::from(start.center_x()), f64::from(settled.center_x())),
            lerp(f64::from(start.center_y()), f64::from(settled.center_y())),
            360.0 * p,
            lerp(f64::from(start.width()) / cw.max(1.0), 1.0),
            cw,
            ch,
            PERSPECTIVE * k,
        );
        canvas.concat_44(&M44::row_major(&m));
        let card = Rect::from_wh(cw as f32, ch as f32);
        let corner = (14.0 * k) as f32;
        let mut shadow = fill(crate::theme::shade(0.6));
        shadow.set_mask_filter(skia_safe::MaskFilter::blur(
            skia_safe::BlurStyle::Normal,
            (18.0 * k) as f32,
            None,
        ));
        canvas.draw_rrect(
            RRect::new_rect_xy(card.with_offset((0.0, (12.0 * k) as f32)), corner, corner),
            &shadow,
        );
        match poster {
            Some(img) => {
                canvas.save();
                canvas.clip_rrect(RRect::new_rect_xy(card, corner, corner), None, true);
                // Cover-fit: crop the source to the card's aspect, centred.
                let (iw, ih) = (f64::from(img.width()), f64::from(img.height()));
                let scale = (cw / iw).max(ch / ih);
                let (sw, sh) = (cw / scale, ch / scale);
                let src = Rect::from_xywh(
                    ((iw - sw) / 2.0) as f32,
                    ((ih - sh) / 2.0) as f32,
                    sw as f32,
                    sh as f32,
                );
                canvas.draw_image_rect_with_sampling_options(
                    img,
                    Some((&src, skia_safe::canvas::SrcRectConstraint::Fast)),
                    card,
                    crate::theme::art_sampling(),
                    &fill(fg(1.0)),
                );
                canvas.restore();
            }
            None => crate::screens::library::draw_poster_placeholder(canvas, fonts, None, card, k),
        }
        canvas.draw_rrect(
            RRect::new_rect_xy(card, corner, corner),
            &crate::theme::stroke(fg(0.14), 1.0),
        );
        canvas.restore();

        // The column: left-aligned beside the cover, centred on its height. Most titles carry
        // one fact line, and a block hung from the top of a card this tall reads as fallen off
        // it. The height is estimated from the rows present; measuring the paragraphs to place
        // them would be a second layout pass for accuracy nobody can see.
        let rows = [&l.facts, &l.developer, &l.genres]
            .iter()
            .filter(|s| !s.is_empty())
            .count() as f64;
        let block = (44.0 + rows * 21.0 + 52.0) * k;
        let a = l.appear as f32;
        let dx = x0 + cw + gap;
        let ty = top + (ch - block).max(0.0) / 2.0;
        fonts.leading(canvas, &l.title, W::SemiBold, 34.0 * k, fg(a), dx, ty, dw);
        let mut y = ty + 52.0 * k;
        for (text, size, alpha) in [
            (&l.facts, 15.0, 0.62),
            (&l.developer, 14.0, 0.45),
            (&l.genres, 14.0, 0.45),
        ] {
            if text.is_empty() {
                continue;
            }
            fonts.leading(canvas, text, W::Regular, size * k, fg(alpha * a), dx, y, dw);
            y += (size + 7.0) * k;
        }
        crate::theme::spinner(canvas, dx + 8.0 * k, y + 30.0 * k, 8.0 * k, t);
        fonts.leading(
            canvas,
            if l.connected {
                "Starting the game\u{2026}"
            } else {
                "Connecting\u{2026}"
            },
            W::Regular,
            12.5 * k,
            fg(0.5 * a),
            dx + 24.0 * k,
            y + 22.0 * k,
            dw,
        );
        // Before the dial lands B cancels it, exactly as it does on the connect card;
        // after, the only thing left to ask for is the picture.
        let hint = if l.connected {
            Hint::new(HintKey::Confirm, "Show stream")
        } else {
            Hint::new(HintKey::Back, "Cancel")
        };
        self.draw_takeover_hints(canvas, w, h, k, fonts, &[hint]);
    }
}
