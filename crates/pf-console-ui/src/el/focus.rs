//! Focus on the element tree: which target a direction reaches, and the plate behind it.
//!
//! Search follows the tvOS focus engine: of the targets ahead in the direction, one that
//! overlaps the current target across the axis wins, then the nearest, then the most
//! centred. A [`Group`] is searched before the rest of the tree along the axes it holds;
//! a group with an id hands focus back to the child it last had.
//!
//! [`Plate`] is one sprung rounded rect that morphs from target to target behind the
//! focused node, then sweeps a light across its rim once on arrival. Under Reduce Motion
//! it jumps and fades. Pinned by `el::tests`.

use crate::anim::{Spring, SpringSpec};
use crate::theme::{accent, fg, fill, stroke};
use pf_client_core::menu_nav::MenuDir;
use skia_safe::{gradient, BlurStyle, Canvas, Color4f, MaskFilter, Point, RRect, Rect, TileMode};

/// A focus container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// Left and Right stay inside; Up and Down leave.
    Row,
    /// Up and Down stay inside; Left and Right leave.
    Column,
    /// Every direction stays inside while a target lies that way.
    Grid,
}

impl Group {
    pub(crate) fn holds(self, dir: MenuDir) -> bool {
        match self {
            Group::Row => matches!(dir, MenuDir::Left | MenuDir::Right),
            Group::Column => matches!(dir, MenuDir::Up | MenuDir::Down),
            Group::Grid => true,
        }
    }
}

/// How `to` ranks as the next target from `from` going `dir`; lower is better. `None`
/// when it is not ahead.
pub(crate) fn score(from: Rect, to: Rect, dir: MenuDir) -> Option<(u8, f32, f32)> {
    let (f, t) = (from.center(), to.center());
    let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| a1.min(b1) - a0.max(b0);
    let (ahead, gap, across, off) = match dir {
        MenuDir::Right => (
            t.x - f.x,
            to.left - from.right,
            overlap(from.top, from.bottom, to.top, to.bottom),
            t.y - f.y,
        ),
        MenuDir::Left => (
            f.x - t.x,
            from.left - to.right,
            overlap(from.top, from.bottom, to.top, to.bottom),
            t.y - f.y,
        ),
        MenuDir::Down => (
            t.y - f.y,
            to.top - from.bottom,
            overlap(from.left, from.right, to.left, to.right),
            t.x - f.x,
        ),
        MenuDir::Up => (
            f.y - t.y,
            from.top - to.bottom,
            overlap(from.left, from.right, to.left, to.right),
            t.x - f.x,
        ),
    };
    (ahead > 0.5).then_some((u8::from(across <= 0.0), gap.max(0.0), off.abs()))
}

/// The plate's travel: the focus spring.
pub const TRAVEL: SpringSpec = crate::anim::springs::FOCUS;
/// Seconds the arrival sweep takes to cross the rim.
const SWEEP_S: f64 = 0.6;
/// Plate growth past its target, design units.
const OUTSET: f32 = 7.0;

/// Sprung rect behind the focused node. It springs in its scroll's content space, so it
/// rides a scrolling list rigidly and only its own travel lags.
#[derive(Default)]
pub struct Plate {
    /// left, top, right, bottom, corner, in `space`'s content px; `None` until the first
    /// target.
    edges: Option<[Spring; 5]>,
    /// Target the plate is travelling to or resting on.
    to: Option<super::Id>,
    /// Scroll whose content the plate lives in, and that scroll's shift this frame.
    space: Option<super::Id>,
    shift: (f32, f32),
    /// Seconds on the plate's own clock.
    t: f64,
    /// When the sweep started; armed by a focus change, fired on arrival.
    sweep: Option<f64>,
    armed: bool,
    /// Reduce Motion fade, 0..1.
    shown: f64,
    /// OK went down: the plate's scale, springing back to 1.
    press: Option<Spring>,
}

impl Plate {
    /// Chase `target`, `id`'s content rect in the scroll `space` shifted by `shift` this
    /// frame, by `dt` seconds.
    pub(crate) fn step(
        &mut self,
        id: super::Id,
        target: Rect,
        corner: f32,
        dt: f64,
        space: Option<super::Id>,
        shift: (f32, f32),
    ) {
        // A new scroll: carry the plate over where it stands on screen.
        if let Some(e) = self.edges.as_mut().filter(|_| space != self.space) {
            let (dx, dy) = (
                f64::from(shift.0 - self.shift.0),
                f64::from(shift.1 - self.shift.1),
            );
            for (s, d) in e.iter_mut().zip([dx, dy, dx, dy, 0.0]) {
                s.pos += d;
            }
        }
        self.space = space;
        self.shift = shift;
        let goal = [
            f64::from(target.left),
            f64::from(target.top),
            f64::from(target.right),
            f64::from(target.bottom),
            f64::from(corner),
        ];
        self.t += dt;
        let reduced = crate::theme::reduce_motion();
        let moved = self.to != Some(id);
        if moved {
            self.armed = true;
            self.sweep = None;
            if reduced {
                self.shown = 0.0;
            }
        }
        self.to = Some(id);
        let edges = self.edges.get_or_insert(goal.map(Spring::rest));
        if reduced {
            *edges = goal.map(Spring::rest);
            self.shown = crate::anim::approach(self.shown, 1.0, dt, 0.06);
        } else {
            self.shown = 1.0;
            for (s, g) in edges.iter_mut().zip(goal) {
                s.step_spec(g, TRAVEL, dt);
                s.settle(g, 0.25, 4.0);
            }
        }
        if let Some(p) = self.press.as_mut() {
            p.step_spec(1.0, crate::anim::springs::PRESS, dt);
            p.settle(1.0, 0.0005, 0.01);
        }
        self.press = self.press.filter(|p| p.pos != 1.0 || p.vel != 0.0);
        let landed = edges
            .iter()
            .zip(goal)
            .all(|(s, g)| s.pos == g && s.vel == 0.0);
        if self.armed && landed {
            self.armed = false;
            self.sweep = Some(self.t);
        }
    }

    /// Still travelling, fading, pressed or sweeping: the frame loop must keep drawing.
    pub fn busy(&self) -> bool {
        self.armed
            || self.shown < 1.0
            || self.press.is_some()
            || self.sweep.is_some_and(|s| self.t - s < SWEEP_S)
    }

    /// OK went down on the focused node: the plate dips and springs back. Reduce Motion
    /// keeps it still; the release acts either way.
    pub(crate) fn press(&mut self) {
        if !crate::theme::reduce_motion() {
            self.press = Some(Spring::rest(crate::anim::PRESS_SCALE));
        }
    }

    /// The pressed element's scale this frame: 1 at rest.
    pub(crate) fn press_scale(&self) -> f32 {
        self.press.map_or(1.0, |p| p.pos as f32)
    }

    /// The plate on screen this frame, before its outset.
    pub(crate) fn rect(&self) -> Option<(Rect, f32)> {
        let e = self.edges.as_ref()?;
        let r = Rect::from_ltrb(
            e[0].pos as f32,
            e[1].pos as f32,
            e[2].pos as f32,
            e[3].pos as f32,
        );
        let s = self.press.map_or(1.0, |p| p.pos) as f32;
        let (dx, dy) = (r.width() * (1.0 - s) / 2.0, r.height() * (1.0 - s) / 2.0);
        Some((
            r.with_inset((dx, dy))
                .with_offset((-self.shift.0, -self.shift.1)),
            e[4].pos.max(0.0) as f32 * s,
        ))
    }

    /// A lifted glass plate with a brighter rim, then the sweep. `k` scales the outset;
    /// `cheap` skips the blurred shadow.
    pub(crate) fn draw(&self, canvas: &Canvas, k: f32, cheap: bool) {
        let Some((r, corner)) = self.rect() else {
            return;
        };
        let alpha = self.shown as f32;
        let out = OUTSET * k;
        let rr = RRect::new_rect_xy(r.with_outset((out, out)), corner + out, corner + out);
        if !cheap {
            let mut shadow = fill(Color4f::new(
                0.0,
                0.0,
                0.0,
                alpha * crate::theme::shadow(0.4),
            ));
            shadow.set_mask_filter(MaskFilter::blur(BlurStyle::Normal, 12.0 * k, None));
            canvas.draw_rrect(rr.with_offset((0.0, 10.0 * k)), &shadow);
        }
        canvas.draw_rrect(rr, &fill(fg(0.12 * alpha)));
        canvas.draw_rrect(rr, &fill(accent(0.10 * alpha)));
        let rim = [fg(0.62 * alpha), fg(0.14 * alpha)];
        let mut p = stroke(fg(1.0), 1.5 * k);
        p.set_shader(linear(rr.rect(), &rim, None));
        canvas.draw_rrect(rr, &p);
        if let Some(s) = self.sweep {
            let p = ((self.t - s) / SWEEP_S) as f32;
            if (0.0..1.0).contains(&p) {
                sweep(canvas, rr, k, p, alpha, cheap);
            }
        }
    }
}

/// One light crossing the rim top-left to bottom-right at `p` of the way, an accent glow
/// under it; a rim glow fading in and out under Reduce Motion.
fn sweep(canvas: &Canvas, rr: RRect, k: f32, p: f32, alpha: f32, cheap: bool) {
    let glow = (std::f32::consts::PI * p).sin() * alpha;
    let band = |c: Color4f| -> Option<skia_safe::Shader> {
        let at = -0.15 + 1.3 * p;
        let clear = Color4f::new(c.r, c.g, c.b, 0.0);
        let stops = [
            (at - 0.14).clamp(0.0, 1.0),
            at.clamp(0.0, 1.0),
            (at + 0.14).clamp(0.0, 1.0),
        ];
        linear(rr.rect(), &[clear, c, clear], Some(&stops))
    };
    let reduced = crate::theme::reduce_motion();
    if !cheap {
        let mut halo = stroke(accent(0.9 * glow), 6.0 * k);
        halo.set_mask_filter(MaskFilter::blur(BlurStyle::Normal, 4.0 * k, None));
        if !reduced {
            halo.set_shader(band(accent(0.9 * glow)));
        }
        canvas.draw_rrect(rr, &halo);
    }
    let mut core = stroke(fg(glow), 2.0 * k);
    if !reduced {
        core.set_shader(band(fg(glow)));
    }
    canvas.draw_rrect(rr, &core);
}

/// Top-left to bottom-right across `r`.
fn linear(r: &Rect, colors: &[Color4f], pos: Option<&[f32]>) -> Option<skia_safe::Shader> {
    let (a, b) = (Point::new(r.left, r.top), Point::new(r.right, r.bottom));
    gradient::shaders::linear_gradient(
        (a, b),
        &gradient::Gradient::new(
            gradient::Colors::new(colors, pos, TileMode::Clamp, None),
            gradient::Interpolation::default(),
        ),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OK down dips the plate around its centre, and the spring brings it back whole.
    #[test]
    fn a_press_dips_the_plate_and_it_springs_back() {
        let mut p = Plate::default();
        let id = super::super::Id::new("t", 0);
        let r = Rect::from_xywh(0.0, 0.0, 100.0, 50.0);
        let frame = |p: &mut Plate| p.step(id, r, 8.0, 1.0 / 60.0, None, (0.0, 0.0));
        frame(&mut p);
        p.press();
        frame(&mut p);
        let (dipped, _) = p.rect().unwrap();
        assert!(dipped.width() < 100.0 && dipped.center_x() == 50.0);
        assert!(p.busy(), "a dipping plate keeps the frames coming");
        for _ in 0..120 {
            frame(&mut p);
        }
        assert_eq!(p.rect().unwrap().0, r);
        assert!(p.press.is_none());
    }
}
