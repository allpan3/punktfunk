//! Console home: a center-snapping carousel of host tiles plus trailing Add Host
//! and Rescan actions.
//!
//! Every tile is a focus target in an [`el::Tree`] row; Left and Right ask the tree,
//! and the focus plate travels behind the tile it lands on. The cursor is the index;
//! the sprung position chases it. Focus scale, brightness, and fade read off the live
//! sprung distance so the look matches the strip mid-motion. OK connects, wakes, or
//! pairs; Y (OK held on a remote) opens the card's menu; X jumps to Settings; Up is
//! the tab strip's; B at the root leaves.
//!
//! Discovery churns the list; focus follows the tile key, not the index. A
//! press on a side tile only retargets the cursor — Confirm starts a session.
//! Pin with the tests in this module: key-follow, confirm routing, padless
//! Settings/Options, pinned-card preset, trailing Add Host.

use crate::anim::{entrances, Entrance, EntranceAt, Spring};
use crate::el::{Axis, El, Group, Id, Tree};
use crate::glyphs::{Hint, HintKey};
use crate::library::{
    step_cursor, StepResult, BUMP_C, BUMP_K, BUMP_PX, ENTER_RISE, ENTER_SCALE, SPRING_C, SPRING_K,
};
use crate::model::{ConsoleCmd, HostRow};
use crate::pointer::{Pointer, PointerKind};
use crate::screens::{ConnectIntent, Ctx, Outbox, Screen};
use crate::theme::{accent, fg, fill, stroke, Fonts, PanelStroke, ONLINE_GREEN, W};
use pf_client_core::menu_nav::{MenuDir, MenuEvent, MenuPulse};
use skia_safe::{Canvas, Color4f, MaskFilter, PathBuilder, Point, RRect, Rect};

const TILE_W: f64 = 340.0;
const TILE_H: f64 = 224.0;
const TILE_GAP: f64 = 30.0;
const TILE_CORNER: f64 = 26.0;
/// Tiles further than this many pitches from the sprung centre are off screen.
const CULL: f64 = 2.6;

/// Sentinel. Host keys are fingerprints or `addr:port`; neither starts with `\0`.
const ADD_KEY: &str = "\0add";
/// Sentinel for the trailing Rescan tile; same `\0` prefix as [`ADD_KEY`].
const SCAN_KEY: &str = "\0scan";

/// The offscreen a tile needs: reduced rendering keeps only the entrance fade. Once mounted,
/// its veil and scale carry depth without rerasterizing the card through a colour-filter layer.
fn tile_layer(reduced: bool, entrance: f64, alpha: f64, recede: f64) -> Option<(f32, bool)> {
    if reduced {
        (entrance < 0.999).then_some((entrance as f32, false))
    } else {
        (alpha < 0.999 || recede > 0.001).then_some((alpha as f32, recede > 0.001))
    }
}

/// Do not use `hosts.get(i)`: `None` is both trailing actions.
enum Slot<'h> {
    Host(&'h HostRow),
    AddHost,
    /// Re-run discovery. A pad has no pull-to-refresh.
    Rescan,
}

fn slot_at(i: usize, hosts: &[HostRow]) -> Slot<'_> {
    match hosts.get(i) {
        Some(h) => Slot::Host(h),
        None if i == hosts.len() => Slot::AddHost,
        None => Slot::Rescan,
    }
}

pub(crate) struct HomeScreen {
    cursor: i32,
    anim: Spring,
    bump: Spring,
    /// Last-seen tile keys. Discovery churns the list; focus follows the key.
    keys: Vec<String>,
    /// Tiles at their drawn size, culled ones included: a direction reaches past the
    /// edge, and a pointer hits the (0.88) side-tile size, not its neighbour's.
    tree: Tree,
    /// Mount entrance. `None` until the first frame (no clock in the constructor)
    /// and again once finished. [`Self::entrance_armed`] stops it re-arming.
    entrance: Option<Entrance>,
    entrance_armed: bool,
}

impl HomeScreen {
    pub(crate) fn new() -> HomeScreen {
        HomeScreen {
            cursor: 0,
            anim: Spring::rest(0.0),
            bump: Spring::rest(0.0),
            keys: Vec::new(),
            tree: Tree::new(),
            entrance: None,
            entrance_armed: false,
        }
    }

    /// Focus follows the tile key, not the index.
    fn reconcile(&mut self, hosts: &[HostRow]) {
        let keys: Vec<String> = hosts
            .iter()
            .map(|h| h.key.clone())
            .chain([ADD_KEY.to_string(), SCAN_KEY.to_string()])
            .collect();
        if keys != self.keys {
            let followed = self
                .keys
                .get(self.cursor as usize)
                .and_then(|old| keys.iter().position(|k| k == old));
            self.cursor = followed.unwrap_or(self.cursor as usize).min(keys.len() - 1) as i32;
            // Leave the spring; render chases the new cursor so the strip animates.
            self.keys = keys;
        }
    }

    fn focused<'h>(&self, hosts: &'h [HostRow]) -> Option<&'h HostRow> {
        hosts.get(self.cursor as usize)
    }

    /// The focused tile's key: a host's, or an action tile's sentinel.
    pub(crate) fn focused_key(&self) -> Option<&str> {
        self.keys
            .get(self.cursor.max(0) as usize)
            .map(String::as_str)
    }

    fn slot<'h>(&self, hosts: &'h [HostRow]) -> Slot<'h> {
        slot_at(self.cursor.max(0) as usize, hosts)
    }

    fn len(hosts: &[HostRow]) -> usize {
        hosts.len() + 2
    }

    fn tile_id(key: &str) -> Id {
        Id::new(key, 0)
    }

    fn index_of(&self, id: Id) -> Option<usize> {
        self.keys.iter().position(|k| Self::tile_id(k) == id)
    }

    /// Left or Right through the tree. With no rects yet, or at an end, the index step
    /// moves or bumps.
    fn travel(&mut self, dir: MenuDir, len: usize) -> Option<MenuPulse> {
        let focused = self
            .keys
            .get(self.cursor.max(0) as usize)
            .map(|k| Self::tile_id(k));
        self.tree.set_focus(focused);
        match self.tree.move_focus(dir).and_then(|id| self.index_of(id)) {
            Some(i) => {
                self.cursor = i as i32;
                Some(MenuPulse::Move)
            }
            None => self.step(if dir == MenuDir::Left { -1 } else { 1 }, len, false),
        }
    }

    pub(crate) fn menu(
        &mut self,
        ev: MenuEvent,
        ctx: &mut Ctx,
        fx: &mut Outbox,
    ) -> Option<MenuPulse> {
        self.reconcile(ctx.hosts);
        let len = Self::len(ctx.hosts);
        match ev {
            MenuEvent::Move(dir @ (MenuDir::Left | MenuDir::Right)) => self.travel(dir, len),
            MenuEvent::JumpBack => self.step(-5, len, true),
            MenuEvent::JumpForward => self.step(5, len, true),
            MenuEvent::Confirm => {
                match self.slot(ctx.hosts) {
                    Slot::AddHost => {
                        fx.push(Screen::AddHost(super::add_host::AddHostScreen::new()))
                    }
                    Slot::Rescan => {
                        fx.cmds.push(ConsoleCmd::Probe);
                        fx.toast = Some("Scanning for hosts…".into());
                    }
                    Slot::Host(h) if !h.paired => fx.push(Screen::Pair(
                        super::pair::PairScreen::new(h, ctx.device_name),
                    )),
                    Slot::Host(h) if !h.online && h.can_wake => {
                        // Wake first; the overlay connects once the host answers.
                        fx.cmds.push(ConsoleCmd::Wake {
                            key: h.key.clone(),
                            then_connect: true,
                        });
                    }
                    Slot::Host(h) => {
                        // Dial even when the pips say offline: a routed or VPN host
                        // can miss mDNS and still answer.
                        fx.connect = Some(ConnectIntent {
                            addr: h.addr.clone(),
                            port: h.port,
                            fp_hex: h.fp_hex.clone(),
                            launch: None,
                            title: match &h.pin {
                                Some(p) => format!("{} · {}", h.name, p.name),
                                None => h.name.clone(),
                            },
                            request_access: false,
                            preset: h.pin.as_ref().map(|p| p.id.clone()),
                        });
                    }
                }
                Some(MenuPulse::Confirm)
            }
            // The card's menu: Y on a pad, OK held on a remote.
            MenuEvent::Secondary => match self.focused(ctx.hosts) {
                Some(h) if super::host_options::HostOptionsScreen::available(h) => {
                    fx.push(Screen::HostOptions(
                        super::host_options::HostOptionsScreen::new(h),
                    ));
                    Some(MenuPulse::Confirm)
                }
                _ => Some(MenuPulse::Boundary),
            },
            // Sector is the ring; this carousel steps on `Move`.
            MenuEvent::Sector(_) => None,
            MenuEvent::Tertiary => {
                fx.tab = Some(crate::shell::Tab::Settings);
                Some(MenuPulse::Confirm)
            }
            MenuEvent::Back => {
                fx.pop(); // root pop is quit (shell rule)
                None
            }
            // Up is the tab strip's; nothing sits below the row yet.
            MenuEvent::Move(MenuDir::Up | MenuDir::Down) => Some(MenuPulse::Boundary),
        }
    }

    /// Only the centre tile activates. A press that also connected would start a
    /// session for a host that was merely aimed at.
    pub(crate) fn pointer(&mut self, p: Pointer, ctx: &mut Ctx, fx: &mut Outbox) -> bool {
        self.reconcile(ctx.hosts);
        let len = Self::len(ctx.hosts);
        match p.kind {
            PointerKind::Scroll { up } => {
                self.step(if up { -1 } else { 1 }, len, false);
                true
            }
            // Hover focuses, so the press that follows is the one that OPENS the card rather
            // than the one that reaches it. The move-then-press fallback below stays for a
            // pointer that cannot hover: a touchscreen sends Press with no Move before it.
            PointerKind::Move => match self.pick(p, len) {
                Some(i) if i != self.cursor as usize => {
                    self.cursor = i as i32;
                    true
                }
                _ => false,
            },
            PointerKind::Press => match self.pick(p, len) {
                Some(i) if i == self.cursor as usize => {
                    self.menu(MenuEvent::Confirm, ctx, fx);
                    true
                }
                Some(i) => {
                    self.cursor = i as i32;
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    /// The painted tile under `p`, by key: discovery can reorder the strip between draw
    /// and press.
    fn pick(&self, p: Pointer, len: usize) -> Option<usize> {
        let i = self.index_of(self.tree.hit(p.x as f32, p.y as f32)?)?;
        (i < len && (i as f64 - self.anim.pos).abs() <= CULL).then_some(i)
    }

    fn step(&mut self, delta: i32, len: usize, clamp: bool) -> Option<MenuPulse> {
        match step_cursor(self.cursor, len, delta, clamp) {
            StepResult::Moved(to) => {
                self.cursor = to;
                Some(MenuPulse::Move)
            }
            StepResult::Boundary => {
                self.bump = Spring {
                    pos: -BUMP_PX * f64::from(delta.signum()),
                    vel: 0.0,
                };
                Some(MenuPulse::Boundary)
            }
        }
    }

    /// The focused tile as a screen reader speaks it: the name, then the line under it.
    pub(crate) fn announcement(&self, hosts: &[HostRow]) -> Option<String> {
        let say = |(title, sub): (&str, &str)| format!("{title}, {sub}");
        Some(match self.slot(hosts) {
            Slot::Host(h) => match (&h.pin, &h.bound_preset) {
                (Some(p), _) => format!("{}, {}", h.name, p.name),
                (None, Some(b)) => format!("{}, {}:{} · {}", h.name, h.addr, h.port, b.name),
                (None, None) => format!("{}, {}:{}", h.name, h.addr, h.port),
            },
            Slot::AddHost => say(action_text(ActionTile::AddHost)),
            Slot::Rescan => say(action_text(ActionTile::Rescan)),
        })
    }

    pub(crate) fn hints(&self, ctx: &Ctx) -> Vec<Hint> {
        let mut hints = Vec::new();
        match self.slot(ctx.hosts) {
            Slot::AddHost => hints.push(Hint::new(HintKey::Confirm, "Add Host")),
            Slot::Rescan => hints.push(Hint::new(HintKey::Confirm, "Scan Again")),
            Slot::Host(h) if !h.paired => hints.push(Hint::new(HintKey::Confirm, "Pair…")),
            Slot::Host(h) if !h.online && h.can_wake => {
                hints.push(Hint::new(HintKey::Confirm, "Wake & Connect"))
            }
            // Same press, honest word: a host with a game up is one you get back INTO,
            // and the tile is already naming the title above it.
            Slot::Host(h) if !h.running.is_empty() => {
                hints.push(Hint::new(HintKey::Confirm, "Resume"))
            }
            Slot::Host(_) => hints.push(Hint::new(HintKey::Confirm, "Connect")),
        }
        if self
            .focused(ctx.hosts)
            .is_some_and(super::host_options::HostOptionsScreen::available)
        {
            hints.push(Hint::new(HintKey::Secondary, "Options"));
        }
        hints.push(Hint::new(HintKey::Tertiary, "Settings"));
        hints.push(Hint::new(HintKey::Back, "Quit"));
        hints
    }

    pub(crate) fn render(
        &mut self,
        canvas: &Canvas,
        rect: Rect,
        k: f64,
        dt: f64,
        fonts: &Fonts,
        ctx: &mut Ctx,
    ) {
        self.reconcile(ctx.hosts);
        let reduced = super::settings::reduce_ui_res(ctx.settings, ctx.platform, ctx.fallback_ui);
        self.anim
            .step(f64::from(self.cursor), SPRING_K, SPRING_C, dt);
        self.anim.settle(f64::from(self.cursor), 0.001, 0.01);
        self.bump.step(0.0, BUMP_K, BUMP_C, dt);
        self.bump.settle(0.0, 0.3, 4.0);
        // Reduced motion drops bump travel, not the chase. Freezing the cursor
        // spring would jump the strip; the refusal is already a Boundary haptic.
        if crate::theme::reduce_motion() {
            self.bump = Spring::rest(0.0);
        }
        // Origin is the cursor, not 0: a restored selection must assemble in place.
        if !self.entrance_armed {
            self.entrance_armed = true;
            self.entrance = Some(Entrance::new(
                entrances::CARDS,
                self.cursor.max(0) as usize,
                ctx.t,
            ));
        }
        if self.entrance.is_some_and(|e| e.done(ctx.t)) {
            self.entrance = None;
        }

        let w = f64::from(rect.width());
        let tile_w = (TILE_W * k).min(w * 0.84);
        let tile_h = (TILE_H * k)
            .min(f64::from(rect.height()) - 48.0 * k)
            .max(118.0 * k);
        let pitch = tile_w + TILE_GAP * k;
        let cx0 = f64::from(rect.left) + w / 2.0 + self.bump.pos * k;
        let cy = f64::from(rect.top) + f64::from(rect.height()) / 2.0;

        let len = Self::len(ctx.hosts);
        let t = ctx.t;
        // A scroll the carousel spring drives, so the plate rides the strip and springs only
        // between tiles. Two pitches of slack each end hold the overshoot and the bump. The
        // viewport spans three widths: a scaled screen in a push must not show its clip.
        let slack = 2.0 * pitch;
        let offset = (slack + self.anim.pos * pitch - self.bump.pos * k) as f32;
        let strip = Id::new("hosts", 0);
        self.tree.set_offset(strip, offset);
        let content_w = 2.0 * slack + 3.0 * w + len.saturating_sub(1) as f64 * pitch;
        let viewport = Rect::from_xywh(-w as f32, 0.0, 3.0 * w as f32, rect.height());
        let origin = (rect.left + viewport.left - offset, rect.top);
        let mut row = El::scroll(strip, Axis::Horizontal)
            .group(Group::Row)
            .child(El::column().place(Rect::from_xywh(0.0, 0.0, content_w as f32, 1.0)));
        for i in 0..len {
            let d = i as f64 - self.anim.pos;
            let f = 1.0 - d.abs().min(1.0); // 1 at focus → 0 one slot out
            let ent = self
                .entrance
                .map_or(EntranceAt::SETTLED, |e| e.at(i, ctx.t));
            let arrive = ENTER_SCALE + (1.0 - ENTER_SCALE) * ent.travel;
            let scale = (0.88 + 0.12 * f) * arrive;
            let alpha = (0.78 + 0.22 * f) * ent.fade;
            let cx = cx0 + d * pitch;
            let cy = cy + (1.0 - ent.travel) * ENTER_RISE * k;
            let tile = Rect::from_xywh(
                (cx - tile_w / 2.0) as f32,
                (cy - tile_h / 2.0) as f32,
                tile_w as f32,
                tile_h as f32,
            );
            // The node is the drawn tile, entrance included: it is still offset by
            // ENTER_RISE while arriving.
            let drawn = Rect::from_xywh(
                (cx - tile_w * scale / 2.0) as f32,
                (cy - tile_h * scale / 2.0) as f32,
                (tile_w * scale) as f32,
                (tile_h * scale) as f32,
            );
            let node = if d.abs() > CULL {
                El::column()
            } else {
                let look = TileLook {
                    tile,
                    center: (cx, cy),
                    scale,
                    alpha,
                    fade: ent.fade,
                    f,
                    reduced,
                    k,
                };
                let slot = slot_at(i, ctx.hosts);
                El::paint(move |canvas, _| look.paint(canvas, fonts, &slot, t))
            };
            row = row.child(
                node.id(Self::tile_id(&self.keys[i]))
                    .focusable((TILE_CORNER * k * scale) as f32)
                    .place(drawn.with_offset((-origin.0, -origin.1))),
            );
        }
        let frame = self
            .tree
            .layout(El::column().child(row.place(viewport)), rect);
        let focused = self
            .keys
            .get(self.cursor.max(0) as usize)
            .map(|k| Self::tile_id(k));
        self.tree.set_focus(focused);
        // The plate is the focus mark: it lifts the tile, so the tile draws no halo.
        self.tree.paint_focus(canvas, frame, k as f32, dt, reduced);

        if ctx.hosts.is_empty() {
            fonts.centered(
                canvas,
                "Hosts on this network appear automatically — add one by address for everything else.",
                W::Regular,
                13.0 * k,
                fg(0.55),
                f64::from(rect.left) + w / 2.0,
                cy + tile_h / 2.0 + 24.0 * k,
                w * 0.7,
            );
        }
    }
}

/// One tile as the carousel draws it this frame.
#[derive(Clone, Copy)]
struct TileLook {
    tile: Rect,
    center: (f64, f64),
    scale: f64,
    alpha: f64,
    /// Entrance fade.
    fade: f64,
    /// 1 at focus, 0 one slot out.
    f: f64,
    reduced: bool,
    k: f64,
}

impl TileLook {
    fn paint(&self, canvas: &Canvas, fonts: &Fonts, slot: &Slot<'_>, t: f64) {
        let TileLook {
            tile,
            center: (cx, cy),
            scale,
            alpha,
            fade,
            f,
            reduced,
            k,
        } = *self;
        canvas.save();
        canvas.translate((cx as f32, cy as f32));
        canvas.scale((scale as f32, scale as f32));
        canvas.translate((-cx as f32, -cy as f32));
        // Rich tiles isolate alpha/recede and blur focus marks. The reduced path keeps the
        // entrance layer only; live card motion remains direct geometry on the main target.
        let recede = 1.0 - f;
        let layer = tile_layer(reduced, fade, alpha, recede);
        if let Some((layer_alpha, filter_recede)) = layer {
            let mut lp = crate::theme::layer();
            lp.set_alpha_f(layer_alpha);
            if filter_recede {
                lp.set_color_filter(skia_safe::color_filters::matrix_row_major(
                    &crate::theme::recede_matrix(recede),
                    None,
                ));
            }
            let bounds = tile.with_outset(((36.0 * k) as f32, (36.0 * k) as f32));
            canvas.save_layer(
                &skia_safe::canvas::SaveLayerRec::default()
                    .bounds(&bounds)
                    .paint(&lp),
            );
        }
        match slot {
            Slot::Host(h) => draw_host_tile(canvas, fonts, h, tile, k, t),
            Slot::AddHost => draw_action_tile(canvas, fonts, tile, k, ActionTile::AddHost),
            Slot::Rescan => draw_action_tile(canvas, fonts, tile, k, ActionTile::Rescan),
        }
        // The cheap path leans harder on the veil because it omits the recede matrix.
        if f < 1.0 {
            let veil = (1.0 - f) as f32 * if reduced { 0.16 } else { 0.07 };
            canvas.draw_rrect(
                RRect::new_rect_xy(tile, (TILE_CORNER * k) as f32, (TILE_CORNER * k) as f32),
                &fill(crate::theme::shade(veil)),
            );
        }
        if layer.is_some() {
            canvas.restore();
        }
        canvas.restore();
    }
}

fn draw_host_tile(canvas: &Canvas, fonts: &Fonts, h: &HostRow, rect: Rect, k: f64, _t: f64) {
    crate::theme::panel(
        canvas,
        rect,
        TILE_CORNER as f32,
        h.saved.then(|| accent(0.20)),
        if h.saved {
            PanelStroke::Gradient
        } else {
            PanelStroke::GradientDashed
        },
        k as f32,
    );
    crate::theme::panel_highlight(canvas, rect, TILE_CORNER as f32, k as f32);
    let pad = 20.0 * k;
    let (l, t) = (f64::from(rect.left) + pad, f64::from(rect.top) + pad);
    draw_badge(canvas, fonts, &h.name, &h.os, h.saved, l, t, k);

    let mut sx = f64::from(rect.right) - pad;
    if h.online {
        let r = 4.5 * k;
        let center = Point::new((sx - r) as f32, (t + 9.0 * k) as f32);
        let mut glow = fill(Color4f::new(
            ONLINE_GREEN.r,
            ONLINE_GREEN.g,
            ONLINE_GREEN.b,
            0.7,
        ));
        glow.set_mask_filter(MaskFilter::blur(
            skia_safe::BlurStyle::Normal,
            (5.0 * k) as f32,
            None,
        ));
        canvas.draw_circle(center, r as f32, &glow);
        canvas.draw_circle(center, r as f32, &fill(ONLINE_GREEN));
        sx -= 2.0 * r + 9.0 * k;
    }
    if h.paired {
        draw_lock(canvas, sx - 9.0 * k, t + 4.0 * k, k);
    }

    let max_w = f64::from(rect.width()) - 2.0 * pad;
    let sub_base = f64::from(rect.bottom) - pad;
    match (&h.pin, &h.bound_preset) {
        (Some(p), _) => {
            fonts.draw_clipped(
                canvas,
                &p.name,
                l,
                sub_base,
                W::SemiBold,
                13.0 * k,
                accent_color(p.accent.as_deref()),
                max_w,
            );
        }
        (None, Some(b)) => {
            let addr = format!("{}:{}", h.addr, h.port);
            let addr_w = f64::from(fonts.measure(&addr, W::Regular, 13.0 * k));
            fonts.draw_clipped(
                canvas,
                &addr,
                l,
                sub_base,
                W::Regular,
                13.0 * k,
                fg(0.55),
                max_w,
            );
            let x = l + addr_w + 8.0 * k;
            if x < l + max_w {
                fonts.draw_clipped(
                    canvas,
                    &format!("· {}", b.name),
                    x,
                    sub_base,
                    W::SemiBold,
                    13.0 * k,
                    accent_color(b.accent.as_deref()),
                    l + max_w - x,
                );
            }
        }
        (None, None) => {
            fonts.draw_clipped(
                canvas,
                &format!("{}:{}", h.addr, h.port),
                l,
                sub_base,
                W::Regular,
                13.0 * k,
                fg(0.55),
                max_w,
            );
        }
    }
    fonts.draw_clipped(
        canvas,
        &h.name,
        l,
        sub_base - 22.0 * k,
        W::Bold,
        23.0 * k,
        fg(1.0),
        max_w,
    );
    // What the host has up, above its name — the one thing you would otherwise have to
    // connect to find out. Green, like the shelf's RESUME pill and the online pip: on
    // this screen that colour already means "live over there".
    if !h.running.is_empty() {
        fonts.draw_clipped(
            canvas,
            &format!("\u{25b6} {}", h.running),
            l,
            sub_base - 48.0 * k,
            W::SemiBold,
            13.0 * k,
            ONLINE_GREEN,
            max_w,
        );
    }
}

/// `#RRGGBB` accent, or the palette accent. A malformed value falls back.
fn accent_color(hex: Option<&str>) -> skia_safe::Color4f {
    let Some(hex) = hex
        .and_then(|a| a.strip_prefix('#'))
        .filter(|h| h.len() == 6)
    else {
        return accent(1.0);
    };
    let Ok(v) = u32::from_str_radix(hex, 16) else {
        return accent(1.0);
    };
    skia_safe::Color4f::new(
        ((v >> 16) & 0xff) as f32 / 255.0,
        ((v >> 8) & 0xff) as f32 / 255.0,
        (v & 0xff) as f32 / 255.0,
        1.0,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActionTile {
    AddHost,
    Rescan,
}

/// The tile's title and the line under it. Drawn and spoken from the same pair.
fn action_text(kind: ActionTile) -> (&'static str, &'static str) {
    match kind {
        ActionTile::AddHost => ("Add Host", "Register a host by address"),
        ActionTile::Rescan => ("Rescan", "Look for hosts on this network again"),
    }
}

fn draw_action_tile(canvas: &Canvas, fonts: &Fonts, rect: Rect, k: f64, kind: ActionTile) {
    crate::theme::panel(
        canvas,
        rect,
        TILE_CORNER as f32,
        None,
        PanelStroke::GradientDashed,
        k as f32,
    );
    crate::theme::panel_highlight(canvas, rect, TILE_CORNER as f32, k as f32);
    let pad = 20.0 * k;
    let (l, t) = (f64::from(rect.left) + pad, f64::from(rect.top) + pad);
    let badge = Rect::from_xywh(l as f32, t as f32, (52.0 * k) as f32, (52.0 * k) as f32);
    canvas.draw_rrect(
        RRect::new_rect_xy(badge, (15.0 * k) as f32, (15.0 * k) as f32),
        &fill(accent(0.16)),
    );
    canvas.draw_rrect(
        RRect::new_rect_xy(badge, (15.0 * k) as f32, (15.0 * k) as f32),
        &stroke(accent(0.5), 1.0),
    );
    let (bcx, bcy) = (l + 26.0 * k, t + 26.0 * k);
    let mut p = stroke(accent(1.0), (3.0 * k) as f32);
    p.set_stroke_cap(skia_safe::PaintCap::Round);
    let r = 9.0 * k;
    match kind {
        ActionTile::AddHost => {
            canvas.draw_line(
                ((bcx - r) as f32, bcy as f32),
                ((bcx + r) as f32, bcy as f32),
                &p,
            );
            canvas.draw_line(
                (bcx as f32, (bcy - r) as f32),
                (bcx as f32, (bcy + r) as f32),
                &p,
            );
        }
        // Static refresh mark. A spinning tile would claim a sweep that is not running.
        ActionTile::Rescan => {
            let mut arc = PathBuilder::new();
            arc.add_arc(
                Rect::from_xywh(
                    (bcx - r) as f32,
                    (bcy - r) as f32,
                    (2.0 * r) as f32,
                    (2.0 * r) as f32,
                ),
                -45.0,
                280.0,
            );
            canvas.draw_path(&arc.detach(), &p);
            let head = 4.6 * k;
            let (hx, hy) = (bcx + r * 0.72, bcy - r * 0.72);
            let mut tip = PathBuilder::new();
            tip.move_to(((hx - head) as f32, (hy - head * 0.2) as f32));
            tip.line_to(((hx + head * 0.5) as f32, (hy - head * 1.1) as f32));
            tip.line_to(((hx + head * 0.2) as f32, (hy + head * 0.7) as f32));
            tip.close();
            canvas.draw_path(&tip.detach(), &fill(accent(1.0)));
        }
    }

    let (title, sub) = action_text(kind);
    let max_w = f64::from(rect.width()) - 2.0 * pad;
    let sub_base = f64::from(rect.bottom) - pad;
    fonts.draw_clipped(
        canvas,
        sub,
        l,
        sub_base,
        W::Regular,
        13.0 * k,
        fg(0.55),
        max_w,
    );
    fonts.draw_clipped(
        canvas,
        title,
        l,
        sub_base - 22.0 * k,
        W::Bold,
        23.0 * k,
        fg(1.0),
        max_w,
    );
}

/// OS mark when the advertised chain resolves; otherwise the host initial.
/// Substitution, not addition: unknown or empty `os` keeps the monogram. The
/// mark is decorative — the name beside it already states the host.
#[allow(clippy::too_many_arguments)]
fn draw_badge(
    canvas: &Canvas,
    fonts: &Fonts,
    name: &str,
    os: &str,
    filled: bool,
    x: f64,
    y: f64,
    k: f64,
) {
    let badge = Rect::from_xywh(x as f32, y as f32, (52.0 * k) as f32, (52.0 * k) as f32);
    let rr = RRect::new_rect_xy(badge, (15.0 * k) as f32, (15.0 * k) as f32);
    if filled {
        let mut p = crate::theme::shaded();
        let colors = [accent(1.0), accent(0.68)];
        p.set_shader(skia_safe::gradient::shaders::linear_gradient(
            (
                Point::new(badge.left, badge.top),
                Point::new(badge.left, badge.bottom),
            ),
            &skia_safe::gradient::Gradient::new(
                skia_safe::gradient::Colors::new_evenly_spaced(
                    &colors,
                    skia_safe::TileMode::Clamp,
                    None,
                ),
                skia_safe::gradient::Interpolation::default(),
            ),
            None,
        ));
        canvas.draw_rrect(rr, &p);
    } else {
        canvas.draw_rrect(rr, &fill(accent(0.16)));
        canvas.draw_rrect(rr, &stroke(accent(0.5), 1.0));
    }
    let ink = if filled { fg(1.0) } else { accent(1.0) };
    // ~54% of the badge so the mark sits on it, not cropped to it. `os_mark`
    // letterboxes a non-square master.
    let side = 28.0 * k;
    let inner = Rect::from_xywh(
        (x + 26.0 * k - side / 2.0) as f32,
        (y + 26.0 * k - side / 2.0) as f32,
        side as f32,
        side as f32,
    );
    if let Some(path) = crate::os_marks::os_mark(os, inner) {
        canvas.draw_path(&path, &fill(ink));
        return;
    }
    let letter: String = name
        .trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "•".to_string());
    let size = 25.0 * k;
    let tw = fonts.measure(&letter, W::Bold, size) as f64;
    fonts.draw(
        canvas,
        &letter,
        x + 26.0 * k - tw / 2.0,
        y + 26.0 * k + size * 0.36,
        W::Bold,
        size,
        ink,
    );
}

fn draw_lock(canvas: &Canvas, x: f64, y: f64, k: f64) {
    let ink = fg(0.5);
    let body_w = 11.0 * k;
    let body_h = 8.0 * k;
    let body_top = y + 5.0 * k;
    canvas.draw_rrect(
        RRect::new_rect_xy(
            Rect::from_xywh(x as f32, body_top as f32, body_w as f32, body_h as f32),
            (2.0 * k) as f32,
            (2.0 * k) as f32,
        ),
        &fill(ink),
    );
    let p = stroke(ink, (1.6 * k) as f32);
    let mut shackle = PathBuilder::new();
    let (cx, r) = (x + body_w / 2.0, 3.2 * k);
    shackle.move_to(((cx - r) as f32, body_top as f32));
    shackle.arc_to(
        Rect::from_xywh(
            (cx - r) as f32,
            (body_top - r) as f32,
            (2.0 * r) as f32,
            (2.0 * r) as f32,
        ),
        180.0,
        180.0,
        false,
    );
    canvas.draw_path(&shackle.detach(), &p);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_tiles_layer_only_for_the_entrance() {
        assert_eq!(tile_layer(true, 1.0, 0.78, 1.0), None);
        assert_eq!(tile_layer(true, 0.5, 0.39, 1.0), Some((0.5, false)));
        assert_eq!(tile_layer(false, 1.0, 0.78, 1.0), Some((0.78, true)));
        assert_eq!(tile_layer(false, 1.0, 1.0, 0.0), None);
    }

    fn host(key: &str, paired: bool, online: bool, can_wake: bool) -> HostRow {
        HostRow {
            key: key.into(),
            id: None,
            name: key.into(),
            addr: "10.0.0.9".into(),
            port: 9777,
            fp_hex: if paired { "ab".into() } else { String::new() },
            paired,
            saved: true,
            online,
            mgmt_port: 47990,
            can_wake,
            clipboard_sync: false,
            last_used: None,
            os: String::new(),
            actions: Vec::new(),
            pin: None,
            bound_preset: None,
            running: String::new(),
            game_presets: Default::default(),
        }
    }

    fn ctx_settings() -> pf_client_core::trust::Settings {
        pf_client_core::trust::Settings::default()
    }

    #[test]
    fn cursor_follows_the_key_through_churn() {
        let mut s = HomeScreen::new();
        let a = host("a", true, true, false);
        let b = host("b", true, true, false);
        s.reconcile(&[a.clone(), b.clone()]);
        s.cursor = 1;
        s.keys = vec!["a".into(), "b".into(), ADD_KEY.into()];
        // A new host inserted in front; focus must stay on "b".
        let c = host("c", false, true, false);
        s.reconcile(&[c, a, b]);
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn confirm_routes_by_host_state() {
        let mut settings = ctx_settings();
        let hosts = [
            host("paired-online", true, true, false),
            host("unpaired", false, true, false),
            host("asleep", true, false, true),
        ];
        let pads: Vec<pf_client_core::menu_nav::PadInfo> = Vec::new();

        let mut s = HomeScreen::new();
        let mut fx = Outbox::default();
        let library = crate::library::LibraryShared::default();
        let mut ctx = Ctx {
            hosts: &hosts,
            library: &library,
            settings: &mut settings,
            store: crate::store::file_store(),
            platform: crate::platform::Platform::Desktop,
            screen: None,
            pads: &pads,
            deck: false,
            fallback_ui: false,
            pyrowave_ok: true,
            av1_ok: true,
            device_name: "test",
            t: 0.0,
        };
        s.menu(MenuEvent::Confirm, &mut ctx, &mut fx);
        assert!(fx.connect.is_some());

        let mut fx = Outbox::default();
        s.cursor = 1;
        s.menu(MenuEvent::Confirm, &mut ctx, &mut fx);
        assert!(matches!(fx.nav, Some(crate::screens::Nav::Push(_))));
        assert!(fx.connect.is_none());

        let mut fx = Outbox::default();
        s.cursor = 2;
        s.menu(MenuEvent::Confirm, &mut ctx, &mut fx);
        assert!(matches!(
            fx.cmds.first(),
            Some(ConsoleCmd::Wake {
                then_connect: true,
                ..
            })
        ));
    }

    /// Up and Down are the shell's: Up lands on the tab strip, and nothing sits below the
    /// row. The host menu is the hold (Secondary), which a remote reaches by holding OK.
    #[test]
    fn up_and_down_leave_the_row_and_the_hold_opens_the_menu() {
        let mut settings = ctx_settings();
        let hosts = [host("paired", true, true, false)];
        let pads: Vec<pf_client_core::menu_nav::PadInfo> = Vec::new();
        let library = crate::library::LibraryShared::default();
        let mut ctx = Ctx {
            hosts: &hosts,
            library: &library,
            settings: &mut settings,
            store: crate::store::file_store(),
            platform: crate::platform::Platform::Android,
            screen: None,
            pads: &pads,
            deck: false,
            fallback_ui: true,
            pyrowave_ok: true,
            av1_ok: true,
            device_name: "test",
            t: 0.0,
        };
        let mut s = HomeScreen::new();
        for dir in [MenuDir::Up, MenuDir::Down] {
            let mut fx = Outbox::default();
            let pulse = s.menu(MenuEvent::Move(dir), &mut ctx, &mut fx);
            assert!(matches!(pulse, Some(MenuPulse::Boundary)) && fx.nav.is_none());
        }
        let mut fx = Outbox::default();
        s.menu(MenuEvent::Secondary, &mut ctx, &mut fx);
        assert!(
            matches!(fx.nav, Some(crate::screens::Nav::Push(ref sc)) if matches!(**sc, Screen::HostOptions(_))),
            "the hold opens the host options menu"
        );
    }

    /// A pin's Confirm connects with that preset (one-off); the overlay title
    /// names the host and the preset.
    #[test]
    fn pinned_card_connects_with_its_preset() {
        let mut settings = ctx_settings();
        let mut pinned = host("ab\0p1", true, true, false);
        pinned.name = "Tower".into();
        pinned.pin = Some(crate::model::PresetChip {
            id: "p1".into(),
            name: "Work".into(),
            accent: None,
            bitrate_kbps: None,
        });
        let hosts = [pinned];
        let pads: Vec<pf_client_core::menu_nav::PadInfo> = Vec::new();
        let library = crate::library::LibraryShared::default();
        let mut ctx = Ctx {
            hosts: &hosts,
            library: &library,
            settings: &mut settings,
            store: crate::store::file_store(),
            platform: crate::platform::Platform::Desktop,
            screen: None,
            pads: &pads,
            deck: false,
            fallback_ui: false,
            pyrowave_ok: true,
            av1_ok: true,
            device_name: "test",
            t: 0.0,
        };
        let mut s = HomeScreen::new();
        let mut fx = Outbox::default();
        s.menu(MenuEvent::Confirm, &mut ctx, &mut fx);
        let intent = fx.connect.expect("a pinned card connects");
        assert_eq!(intent.preset.as_deref(), Some("p1"));
        assert_eq!(intent.title, "Tower · Work");
    }

    #[test]
    fn add_tile_is_always_last() {
        let mut settings = ctx_settings();
        let pads: Vec<pf_client_core::menu_nav::PadInfo> = Vec::new();
        let library = crate::library::LibraryShared::default();
        let mut ctx = Ctx {
            hosts: &[],
            library: &library,
            settings: &mut settings,
            store: crate::store::file_store(),
            platform: crate::platform::Platform::Desktop,
            screen: None,
            pads: &pads,
            deck: false,
            fallback_ui: false,
            pyrowave_ok: true,
            av1_ok: true,
            device_name: "test",
            t: 0.0,
        };
        let mut s = HomeScreen::new();
        let mut fx = Outbox::default();
        s.menu(MenuEvent::Confirm, &mut ctx, &mut fx);
        assert!(
            matches!(fx.nav, Some(crate::screens::Nav::Push(b)) if matches!(*b, Screen::AddHost(_)))
        );
    }

    /// A host with a game up is one you get back INTO, and the tile says which game.
    /// Same press either way — only the word changes.
    #[test]
    fn a_running_host_relabels_connect_as_resume() {
        let mut settings = ctx_settings();
        let idle = host("idle", true, true, false);
        let busy = HostRow {
            running: "Elden Ring".into(),
            ..host("busy", true, true, false)
        };
        let hosts = [idle, busy];
        let pads: Vec<pf_client_core::menu_nav::PadInfo> = Vec::new();
        let library = crate::library::LibraryShared::default();
        let ctx = Ctx {
            hosts: &hosts,
            library: &library,
            settings: &mut settings,
            store: crate::store::file_store(),
            platform: crate::platform::Platform::Desktop,
            screen: None,
            pads: &pads,
            deck: false,
            fallback_ui: false,
            pyrowave_ok: true,
            av1_ok: true,
            device_name: "test",
            t: 0.0,
        };
        let confirm = |s: &HomeScreen| {
            s.hints(&ctx)
                .into_iter()
                .find(|h| h.key == HintKey::Confirm)
                .map(|h| h.label)
                .unwrap_or_default()
        };
        let mut s = HomeScreen::new();
        s.reconcile(&hosts);
        assert_eq!(confirm(&s), "Connect");
        s.cursor = 1;
        assert_eq!(confirm(&s), "Resume");
    }

    /// Right goes through the tree, three presses between frames included (the culled
    /// tiles are still targets), and the plate lands on the tile focus reached.
    #[test]
    fn the_row_moves_through_the_tree_and_the_plate_lands() {
        let mut settings = ctx_settings();
        let hosts = [
            host("a", true, true, false),
            host("b", true, true, false),
            host("c", true, true, false),
            host("d", true, true, false),
        ];
        let pads: Vec<pf_client_core::menu_nav::PadInfo> = Vec::new();
        let library = crate::library::LibraryShared::default();
        let mut ctx = Ctx {
            hosts: &hosts,
            library: &library,
            settings: &mut settings,
            store: crate::store::file_store(),
            platform: crate::platform::Platform::Desktop,
            screen: None,
            pads: &pads,
            deck: false,
            fallback_ui: false,
            pyrowave_ok: true,
            av1_ok: true,
            device_name: "test",
            t: 0.0,
        };
        let fonts = crate::theme::build_fonts().unwrap();
        let mut surface = skia_safe::surfaces::raster_n32_premul((1280, 800)).unwrap();
        let rect = Rect::from_xywh(0.0, 64.0, 1280.0, 650.0);
        let mut s = HomeScreen::new();
        let frame = |s: &mut HomeScreen, ctx: &mut Ctx, surface: &mut skia_safe::Surface| {
            ctx.t += 1.0 / 60.0;
            s.render(surface.canvas(), rect, 1.0, 1.0 / 60.0, &fonts, ctx);
        };
        for _ in 0..60 {
            frame(&mut s, &mut ctx, &mut surface);
        }
        let mut fx = Outbox::default();
        for _ in 0..3 {
            s.menu(MenuEvent::Move(MenuDir::Right), &mut ctx, &mut fx);
        }
        assert_eq!(
            s.cursor, 3,
            "three presses in one frame reach the fourth tile"
        );
        let mut landed = false;
        for _ in 0..120 {
            frame(&mut s, &mut ctx, &mut surface);
            landed |= !s.tree.plate_busy();
        }
        let (plate, _) = s.tree.plate_rect().unwrap();
        let tile = s.tree.rect(HomeScreen::tile_id("d")).unwrap();
        assert!(
            (plate.center_x() - tile.center_x()).abs() < 0.5,
            "{plate:?} vs {tile:?}"
        );
        assert!(
            landed,
            "the plate lands and its sweep ends within two seconds"
        );
    }
}
