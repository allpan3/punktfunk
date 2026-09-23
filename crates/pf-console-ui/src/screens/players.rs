//! Players: the connected pads as cards, then the grants and tests only the platform can
//! perform. The fourth tab.
//!
//! Cards are a focus row, the grant rows a focus column below them; the tree moves focus
//! between them and the plate follows. OK on a card is its rumble test, on a row its grant
//! or test. Only devices the OS classifies as a gamepad are forwarded; adapters often
//! enumerate as something else, so each card's identity line is the support answer.
//!
//! Grant dialogs and a rumble pulse on a real device stay with the host via
//! [`ConsoleCmd::PadAction`]; the desktop has neither, so it lists pads and nothing more.
//! Seat order joins when `seats.rs` lands (`console-ui-redesign.md` §2).

use crate::el::{Axis, El, Group, Id, Tree};
use crate::glyphs::{device_icon, Hint};
use crate::model::ConsoleCmd;
use crate::platform::Platform;
use crate::pointer::{Pointer, PointerKind};
use crate::screens::{Ctx, Outbox};
use crate::theme::{accent, edge, fg, Fonts, PanelStroke, W};
use pf_client_core::menu_nav::{MenuEvent, MenuPulse, PadInfo};
use skia_safe::{Canvas, Rect};

/// Card size and gap, design units.
const CARD_W: f64 = 300.0;
const CARD_H: f64 = 164.0;
const CARD_GAP: f64 = 24.0;
const CARD_CORNER: f64 = 22.0;
/// Device mark box, dp: the size the tells inside each pad outline are drawn for.
const MARK: f64 = 44.0;
/// A grant row, design units.
const ROW_W: f64 = 560.0;
const ROW_H: f64 = 50.0;
const ROW_GAP: f64 = 8.0;
const ROW_CORNER: f64 = 14.0;

/// Host-only pad work: a permission dialog or a real device handle. Neither
/// exists on this side of the bridge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PadAction {
    /// Diagnostic: is the focused pad's motor wired.
    Rumble,
    /// Always shown: without `BLUETOOTH_CONNECT` a BLE SC2 is absent, not idle —
    /// detection cannot run, so hiding the row behind it would hide the grant.
    Sc2Bluetooth,
    Sc2Usb,
    DsUsb,
    /// Ungated by session: a stream that is not sending haptics must not hide
    /// the pad-audio test that rules the pad out.
    DsHaptics,
}

impl PadAction {
    /// Stable id the host matches on (JNI inside [`ConsoleCmd::PadAction`]).
    pub(crate) fn id(self) -> &'static str {
        match self {
            PadAction::Rumble => "rumble",
            PadAction::Sc2Bluetooth => "sc2_bluetooth",
            PadAction::Sc2Usb => "sc2_usb",
            PadAction::DsUsb => "ds_usb",
            PadAction::DsHaptics => "ds_haptics",
        }
    }
}

/// Grant/test rows, Android's alone: the desktop has no USB capture and no grants.
const PASSTHROUGH: [(PadAction, &str, &str); 4] = [
    (
        PadAction::Sc2Bluetooth,
        "Steam Controller 2 over Bluetooth",
        "Grant",
    ),
    (PadAction::Sc2Usb, "Steam Controller 2 over USB", "Grant"),
    (PadAction::DsUsb, "DualSense / DualShock over USB", "Grant"),
    (PadAction::DsHaptics, "DualSense haptics self-test", "Test"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Pad(usize),
    /// The inert card that stands in for no pads, so the row is never empty.
    NoPads,
    Passthrough(usize),
}

fn targets(ctx: &Ctx) -> Vec<Target> {
    let mut all: Vec<Target> = if ctx.pads.is_empty() {
        vec![Target::NoPads]
    } else {
        (0..ctx.pads.len()).map(Target::Pad).collect()
    };
    if ctx.platform == Platform::Android {
        all.extend((0..PASSTHROUGH.len()).map(Target::Passthrough));
    }
    all
}

/// The element id of a target: pads by key, so focus follows a pad through a reorder.
fn target_id(t: Target, pads: &[PadInfo]) -> Id {
    match t {
        Target::Pad(i) => Id::new(&pads[i].key, 0),
        Target::NoPads => Id::new("no-pads", 0),
        Target::Passthrough(i) => Id::new("grant", i),
    }
}

/// Only a host with a device handle can pulse a motor.
fn can_rumble(pad: &PadInfo, platform: Platform) -> bool {
    pad.rumble && matches!(platform, Platform::Android | Platform::Apple)
}

pub(crate) struct PlayersScreen {
    tree: Tree,
}

impl PlayersScreen {
    pub(crate) fn new() -> PlayersScreen {
        PlayersScreen { tree: Tree::new() }
    }

    /// The focused target: the tree's, or the first when focus left with its pad.
    fn focused(&self, ctx: &Ctx) -> Target {
        let all = targets(ctx);
        self.tree
            .focus()
            .and_then(|id| all.iter().copied().find(|t| target_id(*t, ctx.pads) == id))
            .unwrap_or(all[0])
    }

    pub(crate) fn menu(
        &mut self,
        ev: MenuEvent,
        ctx: &mut Ctx,
        fx: &mut Outbox,
    ) -> Option<MenuPulse> {
        let at = self.focused(ctx);
        self.tree.set_focus(Some(target_id(at, ctx.pads)));
        match ev {
            MenuEvent::Back => {
                fx.pop();
                None
            }
            // Up past the cards is the tab strip's: the shell reads the Boundary.
            MenuEvent::Move(dir) => match self.tree.move_focus(dir) {
                Some(_) => Some(MenuPulse::Move),
                None => Some(MenuPulse::Boundary),
            },
            MenuEvent::Confirm => activate(at, ctx, fx),
            _ => None,
        }
    }

    /// Hover focuses; a press acts on what it lands on.
    pub(crate) fn pointer(&mut self, p: Pointer, ctx: &mut Ctx, fx: &mut Outbox) -> bool {
        let Some(id) = self.tree.hit(p.x as f32, p.y as f32) else {
            return false;
        };
        let Some(t) = targets(ctx)
            .into_iter()
            .find(|t| target_id(*t, ctx.pads) == id)
        else {
            return false;
        };
        self.tree.set_focus(Some(id));
        if p.kind == PointerKind::Press {
            activate(t, ctx, fx);
        }
        true
    }

    pub(crate) fn announcement(&self, ctx: &Ctx) -> Option<String> {
        Some(match self.focused(ctx) {
            Target::Pad(i) => format!("{}, {}", ctx.pads[i].name, pad_detail(&ctx.pads[i])),
            Target::NoPads => "No controller connected".into(),
            Target::Passthrough(i) => format!("{}, {}", PASSTHROUGH[i].1, PASSTHROUGH[i].2),
        })
    }

    pub(crate) fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        Vec::new()
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
        let all = targets(ctx);
        // The first target takes focus once; a card that goes reseats it in the tree.
        if self.tree.focus().is_none() {
            self.tree.set_focus(Some(target_id(all[0], ctx.pads)));
        }
        let (platform, pads) = (ctx.platform, ctx.pads);
        let (cw, ch, gap) = (CARD_W * k, CARD_H * k, CARD_GAP * k);
        let cards: Vec<Target> = all
            .iter()
            .copied()
            .filter(|t| !matches!(t, Target::Passthrough(_)))
            .collect();
        let row_w = cards.len() as f64 * (cw + gap) - gap;
        let w = f64::from(rect.width());
        // The viewport reaches past the cards so the plate's lift is not clipped.
        let air = 28.0 * k;
        // On the margin; past the screen's width the row scrolls to keep focus in view.
        let x0 = air + edge(k);
        let strip = Id::new("cards", 0);
        let focus_x = cards
            .iter()
            .position(|t| Some(target_id(*t, pads)) == self.tree.focus())
            .map(|i| x0 + i as f64 * (cw + gap) + cw / 2.0);
        let max = (x0 * 2.0 + row_w - w).max(0.0);
        let offset = focus_x.map_or(0.0, |x| (x - w / 2.0).clamp(0.0, max));
        self.tree.set_offset(strip, offset as f32);
        let top = 24.0 * k;
        let row = El::scroll(strip, Axis::Horizontal)
            .group(Group::Row)
            .id(strip)
            .child(El::column().place(Rect::from_xywh(0.0, 0.0, (x0 * 2.0 + row_w) as f32, 1.0)))
            .children(cards.iter().enumerate().map(|(i, t)| {
                let r = Rect::from_xywh(
                    (x0 + i as f64 * (cw + gap)) as f32,
                    air as f32,
                    cw as f32,
                    ch as f32,
                );
                let t = *t;
                El::paint(move |canvas, r| card(canvas, fonts, t, pads, platform, r, k))
                    .id(target_id(t, pads))
                    .focusable((CARD_CORNER * k) as f32)
                    .place(r)
            }))
            .place(Rect::from_xywh(
                -air as f32,
                (top - air) as f32,
                (w + air) as f32,
                (ch + 2.0 * air) as f32,
            ));
        let grants_top = top + ch + 36.0 * k;
        let (rw, rh) = ((ROW_W * k).min(w - 2.0 * edge(k)), ROW_H * k);
        let grants = El::column()
            .id(Id::new("grants", 0))
            .group(Group::Column)
            .children(
                all.iter()
                    .filter_map(|t| match t {
                        Target::Passthrough(i) => Some(*i),
                        _ => None,
                    })
                    .map(|i| {
                        let r = Rect::from_xywh(
                            edge(k) as f32,
                            (grants_top + 28.0 * k + i as f64 * (rh + ROW_GAP * k)) as f32,
                            rw as f32,
                            rh as f32,
                        );
                        El::paint(move |canvas, r| grant_row(canvas, fonts, i, r, k))
                            .id(target_id(Target::Passthrough(i), pads))
                            .focusable((ROW_CORNER * k) as f32)
                            .place(r)
                    }),
            );
        let has_grants = all.iter().any(|t| matches!(t, Target::Passthrough(_)));
        let root = El::column().child(row).child(grants);
        let frame = self.tree.layout(root, rect);
        let cheap = super::settings::reduce_ui_res(ctx.settings, ctx.platform, ctx.fallback_ui);
        self.tree.paint_focus(canvas, frame, k as f32, dt, cheap);
        if has_grants {
            fonts.draw_tracked(
                canvas,
                "PASSTHROUGH",
                f64::from(rect.left) + edge(k) + 16.0 * k,
                f64::from(rect.top) + grants_top + 16.0 * k,
                W::SemiBold,
                12.0 * k,
                1.4 * k,
                fg(0.45),
            );
        }
        // The explainer reads on a tray: grant rows run under it on a short screen.
        let detail_top = rect.bottom - (40.0 * k) as f32;
        let band = Rect::from_ltrb(rect.left, detail_top, rect.right, rect.bottom);
        crate::widgets::tray(canvas, band, crate::widgets::Toward::Bottom, k);
        let detail = detail(self.focused(ctx), ctx);
        fonts.leading(
            canvas,
            &detail,
            W::Regular,
            13.0 * k,
            fg(0.55),
            f64::from(rect.left) + edge(k),
            f64::from(rect.bottom) - 28.0 * k,
            w - 2.0 * edge(k),
        );
    }
}

/// OK on `t`: a rumble pulse for a pad with a motor the host can reach, a grant or test
/// for a row. Anything else is a thud.
fn activate(t: Target, ctx: &Ctx, fx: &mut Outbox) -> Option<MenuPulse> {
    let (action, pad_key) = match t {
        Target::Pad(i) if can_rumble(&ctx.pads[i], ctx.platform) => {
            (PadAction::Rumble, ctx.pads[i].key.clone())
        }
        Target::Pad(_) | Target::NoPads => return Some(MenuPulse::Boundary),
        // Empty key: the device is not an input device yet (SC2 keyboard/mouse mode).
        Target::Passthrough(i) => (PASSTHROUGH[i].0, String::new()),
    };
    fx.cmds.push(ConsoleCmd::PadAction {
        action: action.id().to_string(),
        pad_key,
    });
    Some(MenuPulse::Confirm)
}

/// A pad card: its family mark, name, what it streams as, battery, and the test OK runs.
fn card(
    canvas: &Canvas,
    fonts: &Fonts,
    t: Target,
    pads: &[PadInfo],
    platform: Platform,
    r: Rect,
    k: f64,
) {
    crate::theme::panel(
        canvas,
        r,
        CARD_CORNER as f32,
        None,
        PanelStroke::Gradient,
        k as f32,
    );
    crate::theme::panel_highlight(canvas, r, CARD_CORNER as f32, k as f32);
    let pad = 20.0 * k;
    let (l, t0) = (f64::from(r.left) + pad, f64::from(r.top) + pad);
    let max_w = f64::from(r.width()) - 2.0 * pad;
    let base = f64::from(r.bottom) - pad;
    let mark_cy = t0 + 16.0 * k;
    let Target::Pad(i) = t else {
        // No pad: the key device that drives instead.
        let mark = device_icon(None, platform);
        crate::glyphs::pad_mark(canvas, mark, l, mark_cy, MARK * k, k, fg(0.5));
        fonts.draw_clipped(
            canvas,
            "No controller",
            l,
            base - 22.0 * k,
            W::Bold,
            21.0 * k,
            fg(0.8),
            max_w,
        );
        fonts.draw_clipped(
            canvas,
            "Connect one to see it here",
            l,
            base,
            W::Regular,
            13.0 * k,
            fg(0.55),
            max_w,
        );
        return;
    };
    let p = &pads[i];
    let mark = device_icon(Some(p.pref), platform);
    crate::glyphs::pad_mark(canvas, mark, l, mark_cy, MARK * k, k, accent(1.0));
    if let Some(b) = p.battery {
        crate::glyphs::battery_pip(
            canvas,
            f64::from(r.right) - pad - 22.0 * k,
            t0 + 16.0 * k,
            22.0 * k,
            k,
            b,
        );
    }
    let kind = p.kind_label();
    let status = match (p.forwarded, kind.is_empty()) {
        (false, _) => "Not forwarded".to_string(),
        (true, true) => "Streams as Xbox 360".to_string(),
        (true, false) => format!("Streams as {kind}"),
    };
    let test = if can_rumble(p, platform) {
        "OK tests rumble"
    } else {
        ""
    };
    fonts.draw_clipped(
        canvas,
        &p.name,
        l,
        base - 44.0 * k,
        W::Bold,
        17.0 * k,
        fg(1.0),
        max_w,
    );
    fonts.draw_clipped(
        canvas,
        &status,
        l,
        base - 22.0 * k,
        W::Regular,
        13.0 * k,
        fg(0.7),
        max_w,
    );
    fonts.draw_clipped(
        canvas,
        test,
        l,
        base,
        W::SemiBold,
        12.0 * k,
        accent(1.0),
        max_w,
    );
}

fn grant_row(canvas: &Canvas, fonts: &Fonts, i: usize, r: Rect, k: f64) {
    crate::theme::panel(
        canvas,
        r,
        ROW_CORNER as f32,
        None,
        PanelStroke::Plain(0.12),
        k as f32,
    );
    let (_, label, verb) = PASSTHROUGH[i];
    let cy = f64::from(r.center_y()) + 15.0 * k * 0.36;
    let pad = 16.0 * k;
    fonts.draw_clipped(
        canvas,
        label,
        f64::from(r.left) + pad,
        cy,
        W::SemiBold,
        15.0 * k,
        fg(1.0),
        f64::from(r.width()) * 0.7,
    );
    let vw = f64::from(fonts.measure(verb, W::SemiBold, 15.0 * k));
    fonts.draw(
        canvas,
        verb,
        f64::from(r.right) - pad - vw,
        cy,
        W::SemiBold,
        15.0 * k,
        accent(1.0),
    );
}

fn detail(t: Target, ctx: &Ctx) -> String {
    match t {
        Target::NoPads => "Punktfunk only forwards devices the system classifies as a gamepad or \
                           joystick — a pad behind an adapter or hub may enumerate with the \
                           adapter's identity, or not at all."
            .into(),
        Target::Pad(i) => pad_detail(&ctx.pads[i]),
        Target::Passthrough(i) => match PASSTHROUGH[i].0 {
            PadAction::Sc2Bluetooth => {
                "A Steam Controller 2 paired over Bluetooth can't be detected at all without \
                 Bluetooth access. Wired and Puck-dongle controllers need no permission."
                    .into()
            }
            PadAction::Sc2Usb => {
                "A wired or Puck-dongle Steam Controller 2 needs USB access to be captured; \
                 until then it stays in its built-in keyboard/mouse mode."
                    .into()
            }
            PadAction::DsUsb => {
                "A wired DualSense or DualShock 4 needs USB access to be captured — with it, \
                 streams drive rumble, adaptive triggers, lightbar and gyro directly."
                    .into()
            }
            PadAction::DsHaptics => {
                "Play a short tone through a wired DualSense's audio endpoint, to tell a pad \
                 that can't do haptics from a stream that is not sending them."
                    .into()
            }
            // Not a passthrough row; pads carry rumble.
            PadAction::Rumble => String::new(),
        },
    }
}

fn pad_detail(pad: &PadInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !pad.detail.is_empty() {
        parts.push(pad.detail.clone());
    }
    if !pad.forwarded {
        parts.push("not forwarded — not classified as a gamepad".into());
    }
    let kind = pad.kind_label();
    parts.push(format!(
        "streams as {}",
        if kind.is_empty() { "Xbox 360" } else { kind }
    ));
    if let Some(b) = pad.battery {
        parts.push(if b.charging {
            format!("battery {} %, charging", b.percent)
        } else {
            format!("battery {} %", b.percent)
        });
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pf_client_core::menu_nav::MenuDir;
    use pf_client_core::trust::Settings;
    use punktfunk_core::config::GamepadPref;

    fn pad(name: &str, rumble: bool) -> PadInfo {
        PadInfo {
            name: name.into(),
            key: format!("054c:0ce6:{name}"),
            pref: GamepadPref::DualSense,
            steam_virtual: false,
            battery: None,
            detail: "054C:0CE6 · gamepad".into(),
            forwarded: true,
            rumble,
        }
    }

    /// Render once so the tree has rects, then send `evs`; the outboxes, in order.
    fn drive(
        screen: &mut PlayersScreen,
        platform: Platform,
        pads: &[PadInfo],
        evs: &[MenuEvent],
    ) -> Vec<(Outbox, Option<MenuPulse>)> {
        let mut settings = Settings::default();
        let library = crate::library::LibraryShared::default();
        let mut ctx = Ctx {
            hosts: &[],
            library: &library,
            settings: &mut settings,
            store: crate::store::file_store(),
            platform,
            screen: None,
            pads,
            deck: false,
            fallback_ui: false,
            pyrowave_ok: true,
            av1_ok: true,
            device_name: "t",
            t: 0.0,
        };
        let fonts = crate::theme::build_fonts().unwrap();
        let mut surface = skia_safe::surfaces::raster_n32_premul((1280, 800)).unwrap();
        let rect = Rect::from_xywh(0.0, 64.0, 1280.0, 650.0);
        screen.render(surface.canvas(), rect, 1.0, 1.0 / 60.0, &fonts, &mut ctx);
        evs.iter()
            .map(|ev| {
                let mut fx = Outbox::default();
                let pulse = screen.menu(*ev, &mut ctx, &mut fx);
                (fx, pulse)
            })
            .collect()
    }

    fn rumble(key: &str) -> ConsoleCmd {
        ConsoleCmd::PadAction {
            action: "rumble".into(),
            pad_key: key.into(),
        }
    }

    #[test]
    fn ok_on_a_card_asks_the_host_for_a_rumble_pulse() {
        let pads = [pad("DualSense", true), pad("Edge", true)];
        let mut s = PlayersScreen::new();
        let out = drive(
            &mut s,
            Platform::Apple,
            &pads,
            &[
                MenuEvent::Confirm,
                MenuEvent::Move(MenuDir::Right),
                MenuEvent::Confirm,
            ],
        );
        assert_eq!(out[0].0.cmds, vec![rumble("054c:0ce6:DualSense")]);
        assert_eq!(
            out[2].0.cmds,
            vec![rumble("054c:0ce6:Edge")],
            "right reaches the next card"
        );
    }

    #[test]
    fn a_pad_the_host_cannot_pulse_thuds() {
        let mut s = PlayersScreen::new();
        let out = drive(
            &mut s,
            Platform::Android,
            &[pad("Adapter", false)],
            &[MenuEvent::Confirm],
        );
        assert!(out[0].0.cmds.is_empty());
        assert!(matches!(out[0].1, Some(MenuPulse::Boundary)));
        let mut s = PlayersScreen::new();
        let out = drive(
            &mut s,
            Platform::Desktop,
            &[pad("DualSense", true)],
            &[MenuEvent::Confirm],
        );
        assert!(
            out[0].0.cmds.is_empty(),
            "the desktop has no device handle to pulse"
        );
    }

    /// Down from the cards lands on the grant rows, Android's alone; up from the cards is
    /// the tab strip's.
    #[test]
    fn down_from_the_cards_reaches_the_grants() {
        let pads = [pad("DualSense", true)];
        let mut s = PlayersScreen::new();
        let out = drive(
            &mut s,
            Platform::Android,
            &pads,
            &[
                MenuEvent::Move(MenuDir::Up),
                MenuEvent::Move(MenuDir::Down),
                MenuEvent::Confirm,
            ],
        );
        assert!(matches!(out[0].1, Some(MenuPulse::Boundary)));
        assert_eq!(
            out[2].0.cmds,
            vec![ConsoleCmd::PadAction {
                action: "sc2_bluetooth".into(),
                pad_key: String::new(),
            }]
        );
        let mut s = PlayersScreen::new();
        let out = drive(
            &mut s,
            Platform::Apple,
            &pads,
            &[MenuEvent::Move(MenuDir::Down)],
        );
        assert!(
            matches!(out[0].1, Some(MenuPulse::Boundary)),
            "no grants off Android"
        );
    }

    #[test]
    fn with_no_pads_an_inert_card_stands_in() {
        let mut s = PlayersScreen::new();
        let out = drive(&mut s, Platform::Android, &[], &[MenuEvent::Confirm]);
        assert!(out[0].0.cmds.is_empty(), "the empty card does nothing");
        assert!(matches!(out[0].1, Some(MenuPulse::Boundary)));
    }
}
