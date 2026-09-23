//! The Games tab: the Mac Library's sections over the shelf's grid, and Customize.
//!
//! Host chips and a Customize chip lead. The enabled sections follow in `library_sections`
//! order; Games is the shelf's own grid, and an empty section hides. Desktops and Launchers
//! leave the grid while their bands show. Focus walks chips, bands and grid as lines; the
//! grid keeps its own cursor and hands off at its top and bottom rows.

use super::{desk_intent, draw_running_badge, paint_cover, LibraryScreen};
use crate::el::El;
use crate::glyphs::{Hint, HintKey};
use crate::library::{LibraryGame, LibraryView, Section, DESKTOP_ID, GRID_GAP};
use crate::model::{ConsoleCmd, HostRow};
use crate::pointer::Pointer;
use crate::screens::card_menu::CardMenu;
use crate::screens::{Ctx, Outbox, Screen};
use crate::theme::{accent, fg, fill, Fonts, PanelStroke, W};
use crate::widgets::{ListMsg, MenuList, RowSpec};
use pf_client_core::menu_nav::{MenuDir, MenuEvent, MenuPulse};
use skia_safe::{Canvas, RRect, Rect};
use std::cell::RefCell;

/// Recently played shows at most this many.
const RECENT_MAX: usize = 12;
// Design units. A band is its heading, its row, then air.
const HEADING_H: f64 = 30.0;
const BAND_AIR: f64 = 18.0;
const CAPTION_H: f64 = 24.0;
const CHIP_H: f64 = 40.0;
const TOP_AIR: f64 = 12.0;
const DESKTOP_W: f64 = 300.0;
const DESKTOP_H: f64 = 96.0;

/// Where the D-pad is on the Games tab. The sort bar keeps its own flag on the shelf.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Zone {
    Grid,
    /// A host chip; the one past the last is Customize.
    Chip(usize),
    Band {
        band: usize,
        item: usize,
    },
}

pub(super) enum Item {
    Desktop(Box<HostRow>),
    /// An index into the shelf's `games`.
    Game(usize),
}

pub(super) struct Band {
    pub section: Section,
    pub items: Vec<Item>,
}

/// One row of focus, top to bottom.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    Chips,
    Band(usize),
    Grid,
}

fn line_of(z: Zone) -> Line {
    match z {
        Zone::Chip(_) => Line::Chips,
        Zone::Band { band, .. } => Line::Band(band),
        Zone::Grid => Line::Grid,
    }
}

/// The hosts a chip opens: each paired host's own shelf.
fn chip_hosts(hosts: &[HostRow]) -> impl Iterator<Item = &HostRow> {
    hosts
        .iter()
        .filter(|h| h.paired && h.saved && h.pin.is_none())
}

impl LibraryScreen {
    /// The shelf lays out as the Games tab. A shelf drilled from Collections stays a grid.
    pub(super) fn sectioned(&self) -> bool {
        self.view_mode == LibraryView::Grid && !self.drilled && !self.embedded
    }

    /// `h` is this shelf's host; a pinned card's shelf counts its primary row.
    fn own(&self, h: &HostRow) -> bool {
        Some(h.key.as_str()) == self.host.key.split('\0').next()
    }

    fn shows(&self, s: Section) -> bool {
        self.sections.iter().any(|&(x, on)| x == s && on)
    }

    /// A band shows this title, so the grid does not. Under the Hosts row the card above
    /// is the desk, and launchers follow the Games tab.
    pub(super) fn banded(&self, g: &LibraryGame) -> bool {
        if self.embedded {
            return g.id == DESKTOP_ID || (g.launcher && self.shows(Section::Launchers));
        }
        self.sectioned()
            && (!self.shows(Section::Games)
                || (g.id == DESKTOP_ID && self.shows(Section::Desktops))
                || (g.launcher && self.shows(Section::Launchers)))
    }

    /// The bands with something in them, in order; `.1` of them sit above the grid.
    pub(super) fn bands(&self, ctx: &Ctx) -> (Vec<Band>, usize) {
        let mut out = Vec::new();
        let mut before = None;
        if !self.sectioned() {
            return (out, 0);
        }
        let favorites = crate::library::favorites(ctx.settings, &self.host.fp_hex);
        for &(section, on) in &self.sections {
            if section == Section::Games {
                before = Some(out.len());
                continue;
            }
            if !on {
                continue;
            }
            let items: Vec<Item> = match section {
                Section::Desktops => {
                    // This shelf's host first: its tile led the grid.
                    let mut hosts: Vec<&HostRow> = chip_hosts(ctx.hosts).collect();
                    hosts.sort_by_key(|h| !self.own(h));
                    if !hosts.iter().any(|h| self.own(h)) {
                        hosts.insert(0, &self.host);
                    }
                    hosts
                        .into_iter()
                        .map(|h| Item::Desktop(Box::new(h.clone())))
                        .collect()
                }
                Section::Recent => {
                    let mut played: Vec<(u64, usize)> = (self.games.iter().enumerate())
                        .filter(|(_, g)| !g.leads())
                        .filter_map(|(i, g)| Some((g.stats.as_ref()?.last_played_unix_ms, i)))
                        .filter(|&(at, _)| at > 0)
                        .collect();
                    played.sort_by_key(|&(at, _)| std::cmp::Reverse(at));
                    (played.into_iter().take(RECENT_MAX))
                        .map(|(_, i)| Item::Game(i))
                        .collect()
                }
                Section::Favorites => {
                    // The shelf's sort, then what a band keeps out of the grid.
                    let mut marked: Vec<usize> = (0..self.games.len())
                        .filter(|&i| favorites.contains(&self.games[i].id))
                        .collect();
                    marked.sort_by_key(|i| self.view.iter().position(|v| v == i));
                    marked.sort_by_key(|i| !self.view.contains(i));
                    marked.into_iter().map(Item::Game).collect()
                }
                Section::Launchers => (0..self.games.len())
                    .filter(|&i| self.games[i].launcher)
                    .map(Item::Game)
                    .collect(),
                Section::Games => unreachable!("handled above"),
            };
            if !items.is_empty() {
                out.push(Band { section, items });
            }
        }
        let before = before.unwrap_or(out.len());
        (out, before)
    }

    /// Focus on arrival: the first band above the grid, else the grid.
    pub(super) fn seat_zone(&mut self, ctx: &Ctx) {
        let (_, before) = self.bands(ctx);
        self.zone = if before > 0 {
            Zone::Band { band: 0, item: 0 }
        } else {
            Zone::Grid
        };
        self.seated = true;
    }

    /// The zone, moved onto something that still exists: bands come and go with the data.
    fn clamp_zone(&self, bands: &[Band], chips: usize, grid: bool) -> Zone {
        match self.zone {
            Zone::Chip(i) => Zone::Chip(i.min(chips - 1)),
            Zone::Band { band, item } if band < bands.len() => Zone::Band {
                band,
                item: item.min(bands[band].items.len() - 1),
            },
            _ if grid => Zone::Grid,
            _ if !bands.is_empty() => Zone::Band { band: 0, item: 0 },
            _ => Zone::Chip(0),
        }
    }

    /// The D-pad across chips, bands and the grid's edges. `None` leaves it to the grid.
    pub(super) fn zone_menu(
        &mut self,
        ev: MenuEvent,
        ctx: &mut Ctx,
        fx: &mut Outbox,
    ) -> Option<Option<MenuPulse>> {
        if !self.sectioned() {
            return None;
        }
        if matches!(ev, MenuEvent::Move(_)) {
            self.follow = true;
        }
        let (bands, before) = self.bands(ctx);
        let chips = chip_hosts(ctx.hosts).count() + 1;
        let grid = self.len() > 0;
        self.zone = self.clamp_zone(&bands, chips, grid);
        let mut lines = vec![Line::Chips];
        lines.extend((0..before).map(Line::Band));
        lines.extend(grid.then_some(Line::Grid));
        lines.extend((before..bands.len()).map(Line::Band));
        let at = lines.iter().position(|&l| l == line_of(self.zone))?;
        match ev {
            MenuEvent::Move(dir @ (MenuDir::Up | MenuDir::Down)) => {
                let down = dir == MenuDir::Down;
                // Undrawn, the grid has no rows to walk yet: it hands off whole.
                if let (Zone::Grid, Some(shape)) = (self.zone, self.grid_shape()) {
                    let row = shape.cell_of(self.cursor.max(0) as usize).0;
                    if (down && row + 1 < shape.rows()) || (!down && row > 0) {
                        return None;
                    }
                }
                let next = if down {
                    lines.get(at + 1).copied()
                } else {
                    at.checked_sub(1).map(|i| lines[i])
                };
                Some(match next {
                    Some(line) => {
                        self.enter(line, down, &bands, chips);
                        Some(MenuPulse::Move)
                    }
                    None if down => Some(MenuPulse::Boundary),
                    None => self.focus_bar().or(Some(MenuPulse::Boundary)),
                })
            }
            _ if self.zone == Zone::Grid => None,
            MenuEvent::Move(dir) => {
                let (i, len) = match self.zone {
                    Zone::Chip(i) => (i, chips),
                    Zone::Band { band, item } => (item, bands[band].items.len()),
                    Zone::Grid => return None,
                };
                let to = match dir {
                    MenuDir::Left => i.checked_sub(1),
                    _ => Some(i + 1).filter(|&j| j < len),
                };
                let Some(to) = to else {
                    return Some(Some(MenuPulse::Boundary));
                };
                self.zone = match self.zone {
                    Zone::Band { band, .. } => Zone::Band { band, item: to },
                    _ => Zone::Chip(to),
                };
                Some(Some(MenuPulse::Move))
            }
            MenuEvent::Confirm => Some(match self.zone {
                Zone::Band { band, item } => {
                    fx.connect = Some(match &bands[band].items[item] {
                        Item::Desktop(h) if self.own(h) => self.desktop_intent(),
                        Item::Desktop(h) => desk_intent(h),
                        Item::Game(i) => self.launch_intent(&self.games[*i]),
                    });
                    Some(MenuPulse::Confirm)
                }
                _ => self.chip_confirm(ctx, fx),
            }),
            MenuEvent::Secondary => {
                match self.zone {
                    Zone::Band { band, item } => match &bands[band].items[item] {
                        Item::Desktop(h) => fx.options(CardMenu::for_host(h)),
                        Item::Game(i) => {
                            let g = &self.games[*i];
                            let cover = self.art.get(&g.id).cloned();
                            fx.options(CardMenu::for_game(&self.host, g, cover));
                        }
                    },
                    _ => match self.chip_host(ctx) {
                        Some(h) => fx.options(CardMenu::for_host(h)),
                        None => return Some(Some(MenuPulse::Boundary)),
                    },
                }
                Some(Some(MenuPulse::Confirm))
            }
            MenuEvent::Back => {
                fx.pop();
                Some(None)
            }
            // Collections, from anywhere on the tab.
            MenuEvent::Tertiary => None,
            MenuEvent::JumpBack | MenuEvent::JumpForward | MenuEvent::Sector(_) => Some(None),
        }
    }

    fn chip_host<'h>(&self, ctx: &Ctx<'h>) -> Option<&'h HostRow> {
        let Zone::Chip(i) = self.zone else {
            return None;
        };
        chip_hosts(ctx.hosts).nth(i)
    }

    /// A host chip swaps this shelf for that host's; the last chip opens Customize.
    fn chip_confirm(&mut self, ctx: &Ctx, fx: &mut Outbox) -> Option<MenuPulse> {
        let Some(h) = self.chip_host(ctx) else {
            fx.push(Screen::Customize(CustomizeScreen::new()));
            return Some(MenuPulse::Confirm);
        };
        if self.own(h) {
            return Some(MenuPulse::Boundary);
        }
        // The epoch is read before the fetch is queued, as for the shell's own shelf.
        let mut shelf = LibraryScreen::new(h, ctx.library.fetch_epoch());
        shelf.zone = self.zone;
        shelf.seated = true;
        fx.cmds.push(ConsoleCmd::FetchLibrary {
            addr: h.addr.clone(),
            mgmt: h.mgmt_port,
            fp_hex: h.fp_hex.clone(),
        });
        fx.replace(Screen::Library(shelf));
        Some(MenuPulse::Confirm)
    }

    /// Focus `line`, on what was drawn nearest the old focus's centre; by index when
    /// nothing there was drawn.
    fn enter(&mut self, line: Line, down: bool, bands: &[Band], chips: usize) {
        let x = self.focus_x();
        let index = match self.zone {
            Zone::Chip(i) | Zone::Band { item: i, .. } => i,
            Zone::Grid => self.grid_col,
        };
        let nearest = |drawn: Vec<(usize, Rect)>| -> Option<usize> {
            let x = x?;
            (drawn.into_iter().filter(|(_, r)| !r.is_empty()))
                .min_by(|a, b| {
                    let (da, db) = ((a.1.center_x() - x).abs(), (b.1.center_x() - x).abs());
                    da.total_cmp(&db)
                })
                .map(|(i, _)| i)
        };
        let drawn = |want: Line| -> Vec<(usize, Rect)> {
            (self.hits.iter())
                .filter(|(z, _)| line_of(*z) == want)
                .map(|&(z, r)| match z {
                    Zone::Chip(i) | Zone::Band { item: i, .. } => (i, r),
                    Zone::Grid => (0, r),
                })
                .collect()
        };
        self.zone = match line {
            Line::Chips => Zone::Chip(nearest(drawn(line)).unwrap_or(index).min(chips - 1)),
            Line::Band(band) => Zone::Band {
                band,
                item: (nearest(drawn(line)).unwrap_or(index)).min(bands[band].items.len() - 1),
            },
            Line::Grid => {
                if let Some(shape) = self.grid_shape() {
                    let row = if down { 0 } else { shape.rows() - 1 };
                    let start = shape.row_start(row);
                    let cells = (start..start + shape.row_len(row))
                        .map(|i| (i, self.geom.get(i).copied().unwrap_or_else(Rect::new_empty)))
                        .collect();
                    let col = index.min(shape.row_len(row) - 1);
                    self.cursor = nearest(cells).unwrap_or(start + col) as i32;
                    self.seat_grid_col();
                    self.follow = true;
                }
                Zone::Grid
            }
        };
    }

    /// Drawn x-centre of what has focus, if it was drawn last frame.
    fn focus_x(&self) -> Option<f32> {
        let r = match self.zone {
            Zone::Grid => *self.geom.get(self.cursor.max(0) as usize)?,
            z => self.hits.iter().find(|(h, _)| *h == z)?.1,
        };
        (!r.is_empty()).then(|| r.center_x())
    }

    /// A pointer over a chip or a band item: hover focuses it, a press on the focused one
    /// is OK. `None` when it is over neither.
    pub(super) fn zone_pointer(&mut self, p: Pointer, press: bool) -> Option<bool> {
        let z = self.hits.iter().find(|(_, r)| p.hits(*r))?.0;
        if z == self.zone {
            return Some(press);
        }
        self.zone = z;
        Some(false)
    }

    /// What the focused chip or band item is called; `None` on the grid.
    pub(super) fn zone_title(&self, ctx: &Ctx) -> Option<String> {
        if !self.sectioned() {
            return None;
        }
        match self.zone {
            Zone::Grid => None,
            Zone::Chip(_) => Some(self.chip_host(ctx).map_or("Customize", |h| &h.name).into()),
            Zone::Band { band, item } => {
                let (bands, _) = self.bands(ctx);
                Some(match bands.get(band)?.items.get(item)? {
                    Item::Desktop(h) if h.running.is_empty() => {
                        format!("{} \u{b7} Desktop", h.name)
                    }
                    Item::Desktop(h) => format!("{} \u{b7} Resume {}", h.name, h.running),
                    Item::Game(i) => self.games[*i].title.clone(),
                })
            }
        }
    }

    /// The legend off the grid; `None` on it.
    pub(super) fn zone_hints(&self, ctx: &Ctx) -> Option<Vec<Hint>> {
        if !self.sectioned() {
            return None;
        }
        let (bands, _) = self.bands(ctx);
        let ok = match self.zone {
            Zone::Grid => return None,
            Zone::Chip(_) if self.chip_host(ctx).is_none() => "Customize",
            Zone::Chip(_) => "Open",
            Zone::Band { band, item } => match bands.get(band)?.items.get(item)? {
                Item::Desktop(h) if h.running.is_empty() => "Stream",
                Item::Desktop(_) => "Resume",
                Item::Game(i) if self.games[*i].running => "Resume",
                Item::Game(i) if self.games[*i].launcher => "Open",
                Item::Game(_) => "Play",
            },
        };
        let mut hints = vec![Hint::new(HintKey::Confirm, ok)];
        if matches!(self.zone, Zone::Band { .. }) || self.chip_host(ctx).is_some() {
            hints.push(Hint::new(HintKey::Secondary, "Options"));
        }
        hints.push(Hint::new(HintKey::Back, "Back"));
        Some(hints)
    }

    /// Scroll height of the chips line and of band `band`, at grid cell height `ch`.
    pub(super) fn chips_h(k: f64) -> f64 {
        (TOP_AIR + CHIP_H + BAND_AIR) * k
    }

    pub(super) fn band_h(band: &Band, ch: f64, k: f64) -> f64 {
        (HEADING_H + BAND_AIR) * k + row_h(band, ch, k)
    }

    /// Chase each band's scroll toward its focused item, centred, clamped to its ends.
    pub(super) fn step_bands(&mut self, bands: &[Band], width: f64, cw: f64, k: f64, snap: bool) {
        self.band_x
            .resize(bands.len(), crate::anim::Spring::rest(0.0));
        let Zone::Band { band, item } = self.zone else {
            return;
        };
        let Some(b) = bands.get(band) else { return };
        let (iw, pitch) = item_pitch(b, cw, k);
        let span = pitch * b.items.len() as f64 - (pitch - iw);
        let want =
            (item as f64 * pitch + iw / 2.0 - width / 2.0).clamp(0.0, (span - width).max(0.0));
        let s = &mut self.band_x[band];
        if snap || crate::theme::reduce_motion() {
            *s = crate::anim::Spring::rest(want);
        } else {
            s.step_spec(want, crate::anim::springs::FOCUS, 1.0 / 60.0);
            s.settle(want, 0.05, 0.5);
        }
    }

    /// The chips line, as a child of the grid's scroll.
    pub(super) fn chips_el<'a>(
        &'a self,
        hosts: &'a [HostRow],
        fonts: &'a Fonts,
        width: f64,
        k: f64,
        hits: &'a RefCell<Vec<(Zone, Rect)>>,
    ) -> El<'a> {
        El::paint(move |canvas, r| {
            let mut x = f64::from(r.left);
            let top = f64::from(r.top) + TOP_AIR * k;
            let names = chip_hosts(hosts).map(|h| (h.name.as_str(), h.os.as_str(), self.own(h)));
            // One line, no scroll: past about six hosts the last chips run off the edge.
            for (i, (label, os, mine)) in names.chain([("Customize", "", false)]).enumerate() {
                let mark = if os.is_empty() { 0.0 } else { 22.0 * k };
                let w = f64::from(fonts.measure(label, W::SemiBold, 15.0 * k)) + mark + 36.0 * k;
                let chip = Rect::from_xywh(x as f32, top as f32, w as f32, (CHIP_H * k) as f32);
                hits.borrow_mut().push((Zone::Chip(i), chip));
                draw_chip(
                    canvas,
                    fonts,
                    label,
                    os,
                    chip,
                    k,
                    mine,
                    self.zone == Zone::Chip(i),
                );
                x += w + 10.0 * k;
            }
        })
        .size(width as f32, Self::chips_h(k) as f32)
    }

    /// Band `b`, as a child of the grid's scroll: its heading, then its row at its own
    /// horizontal scroll. Items past `view`'s sides are not drawn.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn band_el<'a>(
        &'a self,
        band: &'a Band,
        b: usize,
        fonts: &'a Fonts,
        width: f64,
        (cw, ch): (f64, f64),
        view: Rect,
        k: f64,
        hits: &'a RefCell<Vec<(Zone, Rect)>>,
    ) -> El<'a> {
        let label = band.section.label().to_uppercase();
        El::paint(move |canvas, r| {
            fonts.draw_tracked(
                canvas,
                &label,
                f64::from(r.left),
                f64::from(r.top) + HEADING_H * 0.62 * k,
                W::SemiBold,
                12.0 * k,
                1.4 * k,
                fg(0.45),
            );
            let (iw, pitch) = item_pitch(band, cw, k);
            let top = f64::from(r.top) + HEADING_H * k;
            let off = self.band_x.get(b).map_or(0.0, |s| s.pos);
            for (i, it) in band.items.iter().enumerate() {
                let x = f64::from(r.left) + i as f64 * pitch - off;
                if x + iw < f64::from(view.left) || x > f64::from(view.right) {
                    continue;
                }
                let slot =
                    Rect::from_xywh(x as f32, top as f32, iw as f32, row_h(band, ch, k) as f32);
                let z = Zone::Band { band: b, item: i };
                hits.borrow_mut().push((z, slot));
                let on = self.zone == z;
                match it {
                    Item::Desktop(h) => desktop_tile(canvas, fonts, h, slot, k, on),
                    Item::Game(g) => {
                        self.band_poster(canvas, fonts, band.section, *g, slot, ch, k, on)
                    }
                }
            }
        })
        .size(width as f32, Self::band_h(band, ch, k) as f32)
    }

    /// A poster in a band: the grid's cover, with a caption under it — when it was
    /// played in Recently played, its title elsewhere.
    #[allow(clippy::too_many_arguments)]
    fn band_poster(
        &self,
        canvas: &Canvas,
        fonts: &Fonts,
        section: Section,
        i: usize,
        slot: Rect,
        ch: f64,
        k: f64,
        on: bool,
    ) {
        let g = &self.games[i];
        let scale = if on { 1.06 } else { 1.0 };
        let (w, h) = (f64::from(slot.width()) * scale, ch * scale);
        let cy = f64::from(slot.top) + ch / 2.0;
        let cell = Rect::from_xywh(
            (f64::from(slot.center_x()) - w / 2.0) as f32,
            (cy - h / 2.0) as f32,
            w as f32,
            h as f32,
        );
        crate::theme::focus_halo(canvas, cell, 12.0, k as f32, if on { 1.0 } else { 0.0 });
        paint_cover(canvas, fonts, g, self.art.get(&g.id), cell, k, 1.0);
        if on {
            crate::theme::focus_ring(canvas, cell, 12.0, k as f32);
        }
        if g.running {
            draw_running_badge(canvas, fonts, cell, k);
        }
        let played = g.stats.as_ref().map_or(0, |s| s.last_played_unix_ms);
        let caption = if section == Section::Recent && played > 0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            crate::library::ago(now.saturating_sub(played))
        } else {
            g.title.clone()
        };
        fonts.draw_clipped(
            canvas,
            &caption,
            f64::from(slot.left),
            f64::from(slot.top) + ch + 18.0 * k,
            W::Regular,
            12.0 * k,
            fg(if on { 0.9 } else { 0.55 }),
            f64::from(slot.width()),
        );
    }
}

/// A band's row height at grid cell height `ch`.
fn row_h(band: &Band, ch: f64, k: f64) -> f64 {
    if band.section == Section::Desktops {
        DESKTOP_H * k
    } else {
        ch + CAPTION_H * k
    }
}

/// An item's width and the distance to the next, at grid cell width `cw`.
fn item_pitch(band: &Band, cw: f64, k: f64) -> (f64, f64) {
    let w = if band.section == Section::Desktops {
        DESKTOP_W * k
    } else {
        cw
    };
    (w, w + GRID_GAP * k)
}

#[allow(clippy::too_many_arguments)]
fn draw_chip(
    canvas: &Canvas,
    fonts: &Fonts,
    label: &str,
    os: &str,
    r: Rect,
    k: f64,
    mine: bool,
    on: bool,
) {
    let rr = RRect::new_rect_xy(r, r.height() / 2.0, r.height() / 2.0);
    canvas.draw_rrect(rr, &fill(if mine { accent(0.30) } else { fg(0.08) }));
    if on {
        let corner = (f64::from(r.height()) / 2.0 / k) as f32;
        crate::theme::focus_halo(canvas, r, corner, k as f32, 1.0);
        crate::theme::focus_ring(canvas, r, corner, k as f32);
    }
    let ink = if mine || on { fg(1.0) } else { fg(0.7) };
    let mut x = f64::from(r.left) + 18.0 * k;
    let side = 16.0 * k;
    let mark = Rect::from_xywh(
        x as f32,
        (f64::from(r.center_y()) - side / 2.0) as f32,
        side as f32,
        side as f32,
    );
    if let Some(path) = (!os.is_empty())
        .then(|| crate::os_marks::os_mark(os, mark))
        .flatten()
    {
        canvas.draw_path(&path, &fill(ink));
    }
    if !os.is_empty() {
        x += 22.0 * k;
    }
    let size = 15.0 * k;
    fonts.draw(
        canvas,
        label,
        x,
        f64::from(r.center_y()) + size * 0.36,
        W::SemiBold,
        size,
        ink,
    );
}

/// A host's desk in the Desktops band: its badge, its name, what OK does, presence.
fn desktop_tile(canvas: &Canvas, fonts: &Fonts, h: &HostRow, r: Rect, k: f64, on: bool) {
    if on {
        crate::theme::focus_halo(canvas, r, 18.0, k as f32, 1.0);
    }
    crate::theme::panel(
        canvas,
        r,
        18.0,
        Some(accent(if on { 0.28 } else { 0.16 })),
        if on {
            PanelStroke::Brand(0.9)
        } else {
            PanelStroke::Gradient
        },
        k as f32,
    );
    let pad = 22.0 * k;
    let badge_y = f64::from(r.center_y()) - 26.0 * k;
    crate::screens::home::draw_badge(
        canvas,
        fonts,
        &h.name,
        &h.os,
        true,
        f64::from(r.left) + pad,
        badge_y,
        k,
    );
    let x = f64::from(r.left) + pad + 52.0 * k + 14.0 * k;
    let w = f64::from(r.right) - x - pad;
    let cy = f64::from(r.center_y());
    fonts.draw_clipped(
        canvas,
        &h.name,
        x,
        cy - 2.0 * k,
        W::Bold,
        18.0 * k,
        fg(1.0),
        w,
    );
    let (line, ink) = if h.running.is_empty() {
        ("Desktop".to_string(), fg(0.6))
    } else {
        (format!("Resume {}", h.running), crate::theme::ONLINE_GREEN)
    };
    fonts.draw_clipped(
        canvas,
        &line,
        x,
        cy + 18.0 * k,
        W::SemiBold,
        13.0 * k,
        ink,
        w,
    );
    if h.online {
        let dot = (
            (f64::from(r.right) - 14.0 * k) as f32,
            (f64::from(r.top) + 14.0 * k) as f32,
        );
        canvas.draw_circle(dot, (4.0 * k) as f32, &fill(crate::theme::ONLINE_GREEN));
    }
}

/// Customize: the sections' order and switches, stored as `library_sections`. OK picks a
/// row up, Up and Down carry it, OK or Back sets it down; Left hides a section, Right
/// shows it. A pointer press flips the switch.
pub(crate) struct CustomizeScreen {
    pub(crate) list: MenuList,
    held: bool,
}

impl CustomizeScreen {
    pub(crate) fn new() -> CustomizeScreen {
        CustomizeScreen {
            list: MenuList::new(),
            held: false,
        }
    }

    fn save(rows: &[(Section, bool)], ctx: &mut Ctx) {
        ctx.settings.library_sections = crate::library::stored_sections(rows);
        ctx.store.save(ctx.settings);
    }

    pub(crate) fn menu(
        &mut self,
        ev: MenuEvent,
        ctx: &mut Ctx,
        fx: &mut Outbox,
    ) -> Option<MenuPulse> {
        let mut rows = crate::library::sections(&ctx.settings.library_sections);
        let i = self.list.cursor.min(rows.len() - 1);
        match ev {
            MenuEvent::Move(dir @ (MenuDir::Up | MenuDir::Down)) if self.held => {
                let to = if dir == MenuDir::Up {
                    i.checked_sub(1)
                } else {
                    Some(i + 1).filter(|&j| j < rows.len())
                };
                let Some(to) = to else {
                    return Some(MenuPulse::Boundary);
                };
                rows.swap(i, to);
                Self::save(&rows, ctx);
                self.list.cursor = to;
                return Some(MenuPulse::Move);
            }
            MenuEvent::Confirm | MenuEvent::Back if self.held => {
                self.held = false;
                return Some(MenuPulse::Confirm);
            }
            MenuEvent::Back => {
                fx.pop();
                return None;
            }
            _ => {}
        }
        let (msg, pulse) = self.list.menu(ev, rows.len());
        match msg {
            ListMsg::Activate => {
                self.held = true;
                pulse
            }
            ListMsg::Adjust(d) => self.set(&mut rows, d > 0, ctx),
            ListMsg::None => pulse,
        }
    }

    fn set(&mut self, rows: &mut [(Section, bool)], on: bool, ctx: &mut Ctx) -> Option<MenuPulse> {
        let i = self.list.cursor.min(rows.len() - 1);
        if rows[i].1 == on {
            return Some(MenuPulse::Boundary);
        }
        rows[i].1 = on;
        Self::save(rows, ctx);
        Some(MenuPulse::Move)
    }

    pub(crate) fn pointer(&mut self, p: Pointer, ctx: &mut Ctx, _fx: &mut Outbox) -> bool {
        let mut rows = crate::library::sections(&ctx.settings.library_sections);
        let (msg, pulse) = self.list.pointer(p, rows.len());
        match msg {
            ListMsg::Activate => {
                let on = !rows[self.list.cursor.min(rows.len() - 1)].1;
                self.set(&mut rows, on, ctx);
                true
            }
            ListMsg::Adjust(_) => true,
            ListMsg::None => pulse.is_some(),
        }
    }

    pub(crate) fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        if self.held {
            return vec![
                Hint::new(HintKey::Confirm, "Set down"),
                Hint::new(HintKey::Back, "Set down"),
            ];
        }
        vec![
            Hint::new(HintKey::Confirm, "Move"),
            Hint::new(HintKey::Adjust, "Hide / Show"),
            Hint::new(HintKey::Back, "Done"),
        ]
    }

    pub(crate) fn announcement(&self, ctx: &Ctx) -> Option<String> {
        let rows = crate::library::sections(&ctx.settings.library_sections);
        let (s, on) = rows.get(self.list.cursor)?;
        let state = if *on { "shown" } else { "hidden" };
        Some(match self.held {
            true => format!("{}, {state}, moving", s.label()),
            false => format!("{}, {state}", s.label()),
        })
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
        let note_h = 34.0 * k;
        let list = Rect::from_ltrb(rect.left, rect.top, rect.right, rect.bottom - note_h as f32);
        let rows: Vec<RowSpec> = crate::library::sections(&ctx.settings.library_sections)
            .iter()
            .enumerate()
            .map(|(i, (s, on))| {
                RowSpec::toggle(s.label(), *on).with_handle(self.held && i == self.list.cursor)
            })
            .collect();
        self.list.render(canvas, list, &rows, fonts, k, dt, true);
        fonts.centered(
            canvas,
            "The Games tab shows these in this order. An empty section stays hidden.",
            W::Regular,
            13.0 * k,
            fg(0.55),
            f64::from(rect.center_x()),
            f64::from(rect.bottom) - note_h + 6.0 * k,
            f64::from(rect.width()) * 0.8,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OK picks a row up, Down carries it, OK sets it down; Left hides the section.
    /// Each step lands in `library_sections` in the Mac's format.
    #[test]
    fn customize_reorders_and_hides_with_the_remotes_keys() {
        crate::screens::settings::tests::fake_home();
        let library = crate::library::LibraryShared::default();
        let mut settings = pf_client_core::trust::Settings::default();
        let mut ctx = Ctx {
            hosts: &[],
            library: &library,
            settings: &mut settings,
            store: crate::store::file_store(),
            platform: crate::platform::Platform::Desktop,
            screen: None,
            pads: &[],
            deck: false,
            fallback_ui: false,
            pyrowave_ok: true,
            av1_ok: true,
            device_name: "test",
            t: 0.0,
        };
        let mut s = CustomizeScreen::new();
        let mut fx = Outbox::default();
        let mut press = |s: &mut CustomizeScreen, ev| s.menu(ev, &mut ctx, &mut fx);
        press(&mut s, MenuEvent::Confirm);
        press(&mut s, MenuEvent::Move(MenuDir::Down));
        press(&mut s, MenuEvent::Confirm);
        press(&mut s, MenuEvent::Move(MenuDir::Left));
        let pulse = press(&mut s, MenuEvent::Move(MenuDir::Left));
        assert!(matches!(pulse, Some(MenuPulse::Boundary)), "already hidden");
        assert_eq!(
            ctx.settings.library_sections,
            "recent,-desktops,favorites,launchers,games"
        );
    }
}
