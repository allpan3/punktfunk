//! The card menu: what a hold (OK held on a remote, Y, a long press, a right click) opens
//! on a host card or a poster, and Host details, where everything else a saved host
//! offers lives in sections. One screen in three modes; the subject names the object and
//! [`CardMenu::actions`] owns the verbs.
//!
//! A host card's menu is five rows at most, a pinned card's three, a discovered one's two;
//! every row carries an icon. Back leaves any of them. Tests in this module pin each
//! menu's rows, the arm-then-fire rule and the host-key NUL split. Rows and their order:
//! `console-ui-redesign.md` §2.

use crate::glyphs::{Hint, HintKey};
use crate::library::LibraryGame;
use crate::model::{ConsoleCmd, HostRow};
use crate::pointer::Pointer;
use crate::screens::{Ctx, Outbox, Screen};
use crate::shell::Tab;
use crate::store::SettingsStore;
use crate::theme::{fg, Fonts, EDGE_INSET, W};
use crate::widgets::{ListMsg, MenuList, RowSpec, ROW_MAX_W};
use pf_client_core::menu_nav::{MenuEvent, MenuPulse};
use pf_client_core::start;
use skia_safe::{Canvas, Rect};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    /// The presets, for one connect.
    ConnectWith,
    /// The Games tab on this card's shelf.
    Browse,
    Wake,
    CopyLink,
    Details,
    Unpin,
    Pair,
    /// Save a discovered host.
    AddHost,
    /// Stream the host itself, launching nothing — "Resume <title>" when it has a game up.
    /// A shelf's OK launches the focused title, so this is the way back into a game the
    /// host started on its own.
    Connect,
    /// [`Screen::BindPreset`] for the host, or for a library title.
    BindPreset,
    /// Pin or unpin the catalog's preset `i` as a card of its own.
    Pin(usize),
    /// Measure this host's link: a second connect, so it needs a paired host that answers.
    SpeedTest,
    /// Per-host clipboard share. On the host, not in Settings: the other end is this machine.
    Clipboard,
    Edit,
    /// Point `Settings::default_host` at this record, or clear it when it already does.
    MakeDefault,
    /// Indexed into [`HostRow::actions`]: this build renders labels it has never heard of.
    Host(usize),
    SendLogs,
    Forget,
    /// Connect once with preset `i`; `None` is the host's own binding.
    Preset(Option<usize>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Menu,
    Details,
    ConnectWith,
}

/// What the menu was raised on.
pub(crate) enum Subject {
    /// A host card: saved, pinned (the pin rides in the row) or discovered.
    Host(HostRow),
    /// A title on a shelf, with the serving host (pin included) so a link off a pinned
    /// card still streams as that card does.
    Game {
        host: HostRow,
        id: String,
        title: String,
    },
}

pub(crate) struct CardMenu {
    /// By value: discovery rewrites the carousel while this is up, and an index would
    /// retarget Forget onto whatever slid into the slot.
    subject: Subject,
    mode: Mode,
    pub(super) list: MenuList,
    /// A destructive row armed on the first press fires on the second. `Option<Action>`:
    /// arming Forget must not fire Restart if the cursor moved.
    armed: Option<Action>,
}

impl CardMenu {
    pub(crate) fn for_host(host: &HostRow) -> CardMenu {
        CardMenu::on(Subject::Host(host.clone()), Mode::Menu)
    }

    pub(crate) fn for_game(host: &HostRow, game: &LibraryGame) -> CardMenu {
        CardMenu::on(
            Subject::Game {
                host: host.clone(),
                id: game.id.clone(),
                title: game.title.clone(),
            },
            Mode::Menu,
        )
    }

    fn on(subject: Subject, mode: Mode) -> CardMenu {
        CardMenu {
            subject,
            mode,
            list: MenuList::new(),
            armed: None,
        }
    }

    fn host(&self) -> &HostRow {
        match &self.subject {
            Subject::Host(h) => h,
            Subject::Game { host, .. } => host,
        }
    }

    pub(crate) fn title(&self) -> String {
        let name = match &self.subject {
            Subject::Host(h) => match &h.pin {
                Some(p) => format!("{} \u{b7} {}", h.name, p.name),
                None => h.name.clone(),
            },
            Subject::Game { title, .. } => title.clone(),
        };
        match self.mode {
            Mode::Menu => name,
            Mode::Details => format!("{name} \u{b7} Details"),
            Mode::ConnectWith => format!("Connect to {name} with"),
        }
    }

    /// What the connecting takeover names for [`Action::Connect`]: the game being resumed
    /// if there is one, else the host, with a pinned card's preset.
    fn title_for_connect(&self) -> String {
        let host = self.host();
        let subject = if host.running.is_empty() {
            &host.name
        } else {
            &host.running
        };
        match &host.pin {
            Some(p) => format!("{subject} \u{b7} {}", p.name),
            None => subject.clone(),
        }
    }

    /// Pinned-card keys append the preset id past a NUL. Commands address the host half.
    fn host_key(&self) -> &str {
        let key = self.host().key.as_str();
        key.split('\0').next().unwrap_or(key)
    }

    /// The presets this host pins as cards, by id.
    fn pinned(&self, store: &dyn SettingsStore) -> Vec<String> {
        let h = self.host();
        store
            .known_hosts()
            .resolve(Some(&h.fp_hex), &h.addr, h.port)
            .map(|k| k.pinned_presets.clone())
            .unwrap_or_default()
    }

    fn actions(&self, store: &dyn SettingsStore) -> Vec<Action> {
        let host = match (&self.subject, self.mode) {
            (_, Mode::ConnectWith) => {
                return std::iter::once(Action::Preset(None))
                    .chain((0..store.presets().len()).map(|i| Action::Preset(Some(i))))
                    .collect();
            }
            // No Play row: the poster's OK launches it. Connect is the other press.
            (Subject::Game { .. }, _) => {
                return vec![Action::Connect, Action::CopyLink, Action::BindPreset]
            }
            (Subject::Host(h), _) => h,
        };
        let wake = host.can_wake && !host.online;
        if self.mode == Mode::Details {
            return self.details(host, store);
        }
        if host.pin.is_some() {
            return vec![Action::Browse, Action::CopyLink, Action::Unpin];
        }
        if !host.saved {
            return vec![Action::Pair, Action::AddHost];
        }
        let mut a = Vec::new();
        if host.paired {
            a.extend([Action::ConnectWith, Action::Browse]);
        } else {
            a.push(Action::Pair);
        }
        if wake {
            a.push(Action::Wake);
        }
        if host.paired {
            a.push(Action::CopyLink);
        }
        a.push(Action::Details);
        a
    }

    /// Host details, section by section. An empty section drops out.
    fn details(&self, host: &HostRow, store: &dyn SettingsStore) -> Vec<Action> {
        let mut a = vec![Action::BindPreset];
        a.extend((0..store.presets().len()).map(Action::Pin));
        if host.paired && host.online {
            a.push(Action::SpeedTest);
        }
        a.extend([Action::Clipboard, Action::Edit]);
        // A record to point at: a discovered row has no id.
        if host.paired && host.id.is_some() {
            a.push(Action::MakeDefault);
        }
        a.push(Action::Pair);
        if host.can_wake && !host.online {
            a.push(Action::Wake);
        }
        a.extend((0..host.actions.len()).map(Action::Host));
        // Upload authenticates with the streaming cert and needs a live host.
        if host.paired && host.online {
            a.push(Action::SendLogs);
        }
        a.push(Action::Forget);
        a
    }

    /// The section a details row sits in; its first row carries the header.
    fn section(a: Action) -> &'static str {
        match a {
            Action::BindPreset | Action::Pin(_) => "Presets",
            Action::SpeedTest | Action::Clipboard | Action::Edit | Action::MakeDefault => {
                "Connection"
            }
            Action::Pair => "Pairing",
            Action::Wake | Action::Host(_) => "Power",
            Action::SendLogs => "Logs",
            _ => "Remove",
        }
    }

    fn icon(&self, a: Action) -> &'static str {
        match a {
            Action::ConnectWith | Action::Connect | Action::Preset(_) => "play",
            Action::Browse => "gamepad-2",
            Action::Wake => "power",
            Action::CopyLink | Action::Clipboard => "copy",
            Action::Details => "info",
            Action::Unpin | Action::Pin(_) => "pin",
            Action::Pair => "lock",
            Action::AddHost => "plus",
            Action::BindPreset => "settings",
            Action::SpeedTest => "activity",
            Action::Edit => "pencil",
            Action::MakeDefault => "house",
            Action::Host(i) => match self.host().actions.get(i).map(|x| x.id.as_str()) {
                Some("power.sleep") => "moon",
                Some("power.reboot") => "rotate-cw",
                _ => "power",
            },
            Action::SendLogs => "send",
            Action::Forget => "trash-2",
        }
    }

    /// `default` (`Settings::default_host`) names this row. A derived default reads as unset.
    fn is_default(&self, default: Option<&str>) -> bool {
        let id = self.host().id.as_deref();
        id.is_some() && default == id
    }

    fn label(&self, a: Action, ctx: &Ctx) -> String {
        let presets = || ctx.store.presets();
        match a {
            Action::ConnectWith => "Connect with\u{2026}".into(),
            Action::Browse => "Browse games".into(),
            Action::Wake => "Wake host".into(),
            Action::CopyLink => "Copy link".into(),
            Action::Details => "Host details\u{2026}".into(),
            Action::Unpin => "Unpin card".into(),
            Action::Pair if self.host().paired => "Pair again\u{2026}".into(),
            Action::Pair => "Pair\u{2026}".into(),
            Action::AddHost => "Add host".into(),
            // Names the title: "Resume" alone would leave the player guessing which game.
            Action::Connect => match self.host().running.as_str() {
                "" => format!("Connect to {}", self.host().name),
                title => format!("Resume {title}"),
            },
            Action::BindPreset => match self.subject {
                Subject::Game { .. } => "Settings preset\u{2026}".into(),
                Subject::Host(_) => "Default preset\u{2026}".into(),
            },
            Action::Pin(i) => {
                let Some((id, name)) = presets().get(i).cloned() else {
                    return String::new();
                };
                let on = self.pinned(ctx.store).contains(&id);
                format!(
                    "\u{201c}{name}\u{201d} card: {}",
                    if on { "On" } else { "Off" }
                )
            }
            Action::SpeedTest => "Test network speed\u{2026}".into(),
            Action::Clipboard => format!(
                "Shared clipboard: {}",
                if self.host().clipboard_sync {
                    "On"
                } else {
                    "Off"
                }
            ),
            Action::Edit => "Edit name and address\u{2026}".into(),
            // Geist carries no U+2713, so a check mark here draws as a missing glyph.
            Action::MakeDefault if self.is_default(ctx.settings.default_host.as_deref()) => {
                "Default host: On".into()
            }
            Action::MakeDefault => "Default host: Off".into(),
            Action::Host(i) => match self.host().actions.get(i) {
                Some(act) if self.armed == Some(a) => {
                    format!("{} \u{2014} press again", act.label)
                }
                Some(act) => act.label.clone(),
                None => String::new(),
            },
            Action::SendLogs => "Send logs to host".into(),
            Action::Forget if self.armed == Some(Action::Forget) => {
                "Remove host \u{2014} press again".into()
            }
            Action::Forget => "Remove host".into(),
            Action::Preset(None) => "Default settings".into(),
            Action::Preset(Some(i)) => presets().get(i).map(|p| p.1.clone()).unwrap_or_default(),
        }
    }

    /// Host-reported verbs can be unavailable. The row stays; activating it says why.
    fn enabled(&self, a: Action) -> bool {
        match a {
            Action::Host(i) => self.host().actions.get(i).is_none_or(|act| act.available),
            _ => true,
        }
    }

    pub(crate) fn menu(
        &mut self,
        ev: MenuEvent,
        ctx: &mut Ctx,
        fx: &mut Outbox,
    ) -> Option<MenuPulse> {
        if ev == MenuEvent::Back {
            fx.pop();
            return None;
        }
        let actions = self.actions(ctx.store);
        let (msg, pulse) = self.list.menu(ev, actions.len());
        self.dispatch(msg, pulse, &actions, ctx, fx)
    }

    pub(crate) fn pointer(&mut self, p: Pointer, ctx: &mut Ctx, fx: &mut Outbox) -> bool {
        let actions = self.actions(ctx.store);
        let (msg, pulse) = self.list.pointer(p, actions.len());
        if matches!(msg, ListMsg::None) && pulse.is_none() {
            return false;
        }
        self.dispatch(msg, pulse, &actions, ctx, fx);
        true
    }

    fn dispatch(
        &mut self,
        msg: ListMsg,
        pulse: Option<MenuPulse>,
        actions: &[Action],
        ctx: &mut Ctx,
        fx: &mut Outbox,
    ) -> Option<MenuPulse> {
        let Some(action) = actions.get(self.list.cursor).copied() else {
            return pulse;
        };
        // Arming is per row: leaving it must not leave a live trigger on the next one.
        if !matches!(msg, ListMsg::Activate) && self.armed != Some(action) {
            self.armed = None;
        }
        match msg {
            ListMsg::Adjust(_) => Some(MenuPulse::Boundary),
            ListMsg::None => pulse,
            ListMsg::Activate => {
                self.run(action, ctx, fx);
                pulse
            }
        }
    }

    /// `punktfunk://` from the store at activation, never at open: the row may have left
    /// the store while the menu was up.
    fn link(&self, store: &dyn SettingsStore) -> Option<String> {
        match &self.subject {
            Subject::Host(h) => crate::screens::host_link(store, h),
            Subject::Game { host, id, .. } => crate::screens::saved_host_link(
                store,
                &host.fp_hex,
                &host.addr,
                host.port,
                host.pin.as_ref().map(|p| p.id.as_str()),
                Some(id.as_str()),
            ),
        }
    }

    fn connect(&self, preset: Option<String>) -> super::ConnectIntent {
        let host = self.host();
        super::ConnectIntent {
            addr: host.addr.clone(),
            port: host.port,
            fp_hex: host.fp_hex.clone(),
            launch: None,
            title: self.title_for_connect(),
            request_access: false,
            preset,
        }
    }

    fn run(&mut self, action: Action, ctx: &mut Ctx, fx: &mut Outbox) {
        let store = ctx.store;
        let key = self.host_key().to_string();
        match action {
            Action::ConnectWith => fx.replace(Screen::CardMenu(CardMenu::on(
                Subject::Host(self.host().clone()),
                Mode::ConnectWith,
            ))),
            Action::Details => fx.replace(Screen::CardMenu(CardMenu::on(
                Subject::Host(self.host().clone()),
                Mode::Details,
            ))),
            // The shell drops this menu as it switches; Games follows the focused card.
            Action::Browse => fx.tab = Some(Tab::Games),
            Action::Wake => {
                fx.cmds.push(ConsoleCmd::Wake {
                    key,
                    then_connect: false,
                });
                fx.pop();
            }
            Action::Pair => fx.replace(Screen::Pair(super::pair::PairScreen::new(
                self.host(),
                ctx.device_name,
            ))),
            Action::AddHost => {
                let host = self.host();
                fx.cmds.push(ConsoleCmd::SaveHost {
                    name: host.name.clone(),
                    addr: host.addr.clone(),
                    port: host.port,
                });
                fx.toast = Some(format!("Added {}", host.name));
                fx.pop();
            }
            // Whole-file writer: rebase on the store before mutating, or a setting another
            // screen just wrote is reverted.
            Action::MakeDefault => {
                let on = !self.is_default(ctx.settings.default_host.as_deref());
                let name = self.host().name.clone();
                let id = self.host().id.clone();
                *ctx.settings = ctx.store.load();
                ctx.settings.default_host = on.then_some(id).flatten();
                ctx.store.save(ctx.settings);
                let opens = start::StartIn::parse(&ctx.settings.start_in) != start::StartIn::Hosts;
                fx.toast = Some(match (on, opens) {
                    (true, true) => format!("{name} opens on launch"),
                    (true, false) => format!("{name} is the default host"),
                    (false, _) => format!("{name} is no longer the default host"),
                });
            }
            Action::SendLogs => {
                let host = self.host();
                fx.cmds.push(ConsoleCmd::SendLogs {
                    addr: host.addr.clone(),
                    mgmt: host.mgmt_port,
                    fp_hex: host.fp_hex.clone(),
                    host_name: host.name.clone(),
                });
                fx.toast = Some(format!("Sending logs to {}\u{2026}", host.name));
                fx.pop();
            }
            Action::SpeedTest => {
                let host = self.host();
                fx.cmds.push(ConsoleCmd::SpeedTest {
                    key,
                    addr: host.addr.clone(),
                    port: host.port,
                    fp_hex: host.fp_hex.clone(),
                    host_name: host.name.clone(),
                });
                // No toast: the takeover the service raises is the feedback.
                fx.pop();
            }
            Action::CopyLink => {
                match self.link(store) {
                    Some(url) => {
                        fx.copy = Some(url);
                        fx.toast = Some("Link copied".into());
                    }
                    None => fx.toast = Some("This host isn't saved any more".into()),
                }
                fx.pop();
            }
            // No launch id: the host is already showing whatever is up, and launching the
            // game it runs starts a second copy. Pop, so the session ends back here.
            Action::Connect => {
                fx.connect = Some(self.connect(self.host().pin.as_ref().map(|p| p.id.clone())));
                fx.pop();
            }
            Action::Preset(i) => {
                let preset = i.and_then(|i| store.presets().get(i).map(|p| p.0.clone()));
                fx.connect = Some(self.connect(preset));
                fx.pop();
            }
            Action::Edit => fx.replace(Screen::AddHost(super::add_host::AddHostScreen::edit(
                self.host(),
            ))),
            // Same screen either way; the subject decides which binding it writes.
            Action::BindPreset => {
                let host_name = self.host().name.clone();
                let screen = match &self.subject {
                    Subject::Game { id, title, .. } => {
                        super::bind_preset::BindPresetScreen::for_game(
                            key,
                            host_name,
                            super::bind_preset::GameSubject {
                                id: id.clone(),
                                title: title.clone(),
                            },
                            store.presets(),
                        )
                    }
                    Subject::Host(_) => {
                        super::bind_preset::BindPresetScreen::new(key, host_name, store.presets())
                    }
                };
                fx.replace(Screen::BindPreset(screen));
            }
            Action::Pin(i) => {
                if let Some((id, name)) = store.presets().get(i).cloned() {
                    let pin = !self.pinned(store).contains(&id);
                    fx.toast = Some(if pin {
                        format!("Pinned \u{201c}{name}\u{201d} as a card")
                    } else {
                        format!("Unpinned \u{201c}{name}\u{201d}")
                    });
                    fx.cmds.push(ConsoleCmd::SetPin {
                        key,
                        preset_id: id,
                        pin,
                    });
                }
            }
            Action::Clipboard => {
                let host = self.host();
                let on = !host.clipboard_sync;
                fx.toast = Some(if on {
                    format!("Clipboard shared with {}", host.name)
                } else {
                    format!("Clipboard no longer shared with {}", host.name)
                });
                fx.cmds.push(ConsoleCmd::SetClipboard { key, on });
                fx.pop();
            }
            Action::Forget if self.armed != Some(Action::Forget) => {
                self.armed = Some(Action::Forget)
            }
            Action::Forget => {
                fx.cmds.push(ConsoleCmd::ForgetHost { key });
                fx.toast = Some(format!("Removed {}", self.host().name));
                fx.pop();
            }
            Action::Host(i) => {
                let host = self.host();
                let Some(act) = host.actions.get(i) else {
                    return; // row list changed under the cursor
                };
                // The host says no: say why rather than send a request it will refuse.
                if !act.available {
                    let why = act.unavailable_reason.clone();
                    fx.toast = Some(if why.is_empty() {
                        format!("{} isn't available right now", act.label)
                    } else {
                        why
                    });
                    fx.pop();
                    return;
                }
                // `danger` (restart, shut down) arms then fires. Sleep undoes with Wake.
                if act.danger && self.armed != Some(action) {
                    self.armed = Some(action);
                    return;
                }
                fx.cmds.push(ConsoleCmd::HostAction {
                    addr: host.addr.clone(),
                    mgmt: host.mgmt_port,
                    fp_hex: host.fp_hex.clone(),
                    host_name: host.name.clone(),
                    action_id: act.id.clone(),
                    label: act.label.clone(),
                });
                fx.toast = Some(format!(
                    "{} \u{2014} asking {}\u{2026}",
                    act.label, host.name
                ));
                fx.pop();
            }
            Action::Unpin => {
                if let Some(p) = &self.host().pin {
                    fx.cmds.push(ConsoleCmd::SetPin {
                        key,
                        preset_id: p.id.clone(),
                        pin: false,
                    });
                    fx.toast = Some(format!("Unpinned {}", p.name));
                }
                fx.pop();
            }
        }
    }

    pub(crate) fn hints(&self, _ctx: &Ctx) -> Vec<Hint> {
        vec![
            Hint::new(HintKey::Confirm, "Choose"),
            Hint::new(HintKey::Back, "Close"),
        ]
    }

    fn blurb(&self) -> String {
        match (&self.subject, self.mode) {
            (_, Mode::ConnectWith) => "This connect only; the card keeps its own preset.".into(),
            (Subject::Host(h), _) if h.pin.is_some() => {
                "A shortcut to one preset on this host. Unpinning it changes nothing about the \
                 host or the preset."
                    .into()
            }
            (Subject::Host(h), _) if !h.saved => "Found on this network.".into(),
            (Subject::Host(h), Mode::Details) => format!("{}:{}", h.addr, h.port),
            (Subject::Host(_), _) => String::new(),
            (Subject::Game { host, .. }, _) => format!("On {}.", host.name),
        }
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
        // Air under the title so the first row does not sit on it.
        fonts.leading(
            canvas,
            &self.blurb(),
            W::Regular,
            13.0 * k,
            fg(0.55),
            f64::from(rect.left) + EDGE_INSET * k,
            f64::from(rect.top) + 2.0 * k,
            ROW_MAX_W * 0.72 * k,
        );
        let list_rect = Rect::from_ltrb(
            rect.left,
            rect.top + (34.0 * k) as f32,
            rect.right,
            rect.bottom,
        );
        let actions = self.actions(ctx.store);
        let mut last = "";
        let rows: Vec<RowSpec> = actions
            .iter()
            .map(|&a| {
                let row =
                    RowSpec::action(self.label(a, ctx), self.enabled(a)).with_icon(self.icon(a));
                if self.mode != Mode::Details || Self::section(a) == last {
                    return row;
                }
                last = Self::section(a);
                row.with_header(last)
            })
            .collect();
        self.list
            .render(canvas, list_rect, &rows, fonts, k, dt, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PresetChip;
    use crate::screens::settings::tests::fake_home;
    use crate::screens::Nav;

    fn with_ctx<R>(f: impl FnOnce(&mut Ctx) -> R) -> R {
        fake_home();
        let mut settings = crate::store::file_store().load();
        let library = crate::library::LibraryShared::default();
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
        f(&mut ctx)
    }

    /// Drive `run` over a scratch config dir. Settings come from the store, so a row that
    /// writes one reads its own last write.
    fn run_action(s: &mut CardMenu, action: Action, fx: &mut Outbox) {
        with_ctx(|ctx| s.run(action, ctx, fx));
    }

    fn rows(s: &CardMenu) -> Vec<Action> {
        s.actions(crate::store::file_store())
    }

    fn label(s: &CardMenu, a: Action) -> String {
        with_ctx(|ctx| s.label(a, ctx))
    }

    fn details(h: &HostRow) -> CardMenu {
        CardMenu::on(Subject::Host(h.clone()), Mode::Details)
    }

    fn host() -> HostRow {
        HostRow {
            key: "aa".into(),
            id: None,
            name: "Desk".into(),
            addr: "10.0.0.5".into(),
            port: 9777,
            fp_hex: "aa".into(),
            paired: true,
            saved: true,
            online: true,
            mgmt_port: 9778,
            can_wake: false,
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

    fn powered() -> HostRow {
        let act = |id: &str, label: &str, danger: bool, available: bool| crate::model::HostAction {
            id: id.into(),
            label: label.into(),
            danger,
            available,
            unavailable_reason: if available {
                String::new()
            } else {
                "this machine does not support sleep".into()
            },
        };
        HostRow {
            actions: vec![
                act("power.sleep", "Sleep host", false, true),
                act("power.reboot", "Restart host", true, true),
                act("power.shutdown", "Shut down host", true, true),
            ],
            ..host()
        }
    }

    fn pinned() -> HostRow {
        HostRow {
            key: "aa\u{0}prof-1".into(),
            pin: Some(PresetChip {
                id: "prof-1".into(),
                name: "4K".into(),
                accent: None,
                bitrate_kbps: None,
            }),
            ..host()
        }
    }

    fn game() -> LibraryGame {
        LibraryGame {
            id: "steam:367520".into(),
            title: "Hollow Knight".into(),
            store: "steam".into(),
            launcher: false,
            icon: "steam".into(),
            platform: None,
            developer: None,
            year: None,
            genres: Vec::new(),
            stats: None,
            running: false,
        }
    }

    /// The design's three card menus: five rows at most, three on a pin, two on a find.
    #[test]
    fn each_card_gets_its_own_short_menu() {
        use Action::*;
        let asleep = HostRow {
            can_wake: true,
            online: false,
            ..host()
        };
        assert_eq!(
            rows(&CardMenu::for_host(&asleep)),
            vec![ConnectWith, Browse, Wake, CopyLink, Details]
        );
        assert_eq!(
            rows(&CardMenu::for_host(&host())),
            vec![ConnectWith, Browse, CopyLink, Details],
            "an awake host is not offered a wake"
        );
        assert_eq!(
            rows(&CardMenu::for_host(&pinned())),
            vec![Browse, CopyLink, Unpin]
        );
        let found = HostRow {
            saved: false,
            paired: false,
            ..host()
        };
        assert_eq!(rows(&CardMenu::for_host(&found)), vec![Pair, AddHost]);
        let unpaired = HostRow {
            paired: false,
            ..host()
        };
        assert_eq!(rows(&CardMenu::for_host(&unpaired)), vec![Pair, Details]);
        // Commands address the host, not the pin's composite key.
        assert_eq!(CardMenu::for_host(&pinned()).host_key(), "aa");
    }

    #[test]
    fn browse_switches_to_games_without_popping_the_new_root() {
        let mut s = CardMenu::for_host(&host());
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Browse, &mut fx);
        assert_eq!(fx.tab, Some(Tab::Games));
        assert!(
            fx.nav.is_none(),
            "a pop after the switch would leave the console"
        );
    }

    #[test]
    fn details_and_connect_with_replace_the_menu() {
        for action in [Action::Details, Action::ConnectWith] {
            let mut s = CardMenu::for_host(&host());
            let mut fx = Outbox::default();
            run_action(&mut s, action, &mut fx);
            assert!(
                matches!(fx.nav, Some(Nav::Replace(ref sc)) if matches!(**sc, Screen::CardMenu(_)))
            );
        }
    }

    /// Connect with… connects once; the default row leaves the preset to the binding.
    #[test]
    fn connect_with_default_settings_names_no_preset() {
        let mut s = CardMenu::on(Subject::Host(host()), Mode::ConnectWith);
        assert_eq!(rows(&s).first(), Some(&Action::Preset(None)));
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Preset(None), &mut fx);
        let intent = fx.connect.expect("a connect");
        assert_eq!(intent.preset, None);
        assert_eq!(intent.launch, None);
        assert!(matches!(fx.nav, Some(Nav::Pop)));
    }

    #[test]
    fn a_discovered_card_adds_the_host() {
        let found = HostRow {
            saved: false,
            paired: false,
            ..host()
        };
        let mut s = CardMenu::for_host(&found);
        let mut fx = Outbox::default();
        run_action(&mut s, Action::AddHost, &mut fx);
        assert_eq!(
            fx.cmds,
            vec![ConsoleCmd::SaveHost {
                name: "Desk".into(),
                addr: "10.0.0.5".into(),
                port: 9777,
            }]
        );
    }

    /// Details groups the rest: the speed test, logs and wake need what they always
    /// needed, and removal comes last.
    #[test]
    fn details_holds_everything_else_in_sections() {
        let s = details(&HostRow {
            id: Some("rec-1".into()),
            ..host()
        });
        let r = rows(&s);
        for a in [
            Action::BindPreset,
            Action::SpeedTest,
            Action::Clipboard,
            Action::Edit,
            Action::MakeDefault,
            Action::Pair,
            Action::SendLogs,
        ] {
            assert!(r.contains(&a), "{a:?} missing");
        }
        assert_eq!(r.last(), Some(&Action::Forget));
        let sections: Vec<&str> = r.iter().map(|a| CardMenu::section(*a)).collect();
        let mut sorted = sections.clone();
        sorted.dedup();
        let mut unique = sorted.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            sorted.len(),
            unique.len(),
            "a section's rows sit together: {sections:?}"
        );

        let offline = details(&HostRow {
            online: false,
            can_wake: true,
            ..host()
        });
        let r = rows(&offline);
        assert!(r.contains(&Action::Wake));
        assert!(!r.contains(&Action::SpeedTest) && !r.contains(&Action::SendLogs));
        let unpaired = details(&HostRow {
            paired: false,
            id: Some("rec-1".into()),
            ..host()
        });
        assert!(!rows(&unpaired).contains(&Action::MakeDefault));
    }

    #[test]
    fn host_actions_appear_only_when_the_host_offered_them() {
        assert!(!rows(&details(&host()))
            .iter()
            .any(|a| matches!(a, Action::Host(_))));
        let s = details(&powered());
        assert_eq!(
            rows(&s)
                .iter()
                .filter(|a| matches!(a, Action::Host(_)))
                .count(),
            3
        );
        assert_eq!(label(&s, Action::Host(0)), "Sleep host");
        assert_eq!(s.icon(Action::Host(0)), "moon");
    }

    /// Sleep fires on one press. Restart and shut down arm; arming one must not leave the
    /// other live.
    #[test]
    fn destructive_host_actions_arm_before_they_fire() {
        let mut s = details(&powered());
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Host(0), &mut fx);
        assert!(matches!(
            fx.cmds.first(),
            Some(ConsoleCmd::HostAction { action_id, .. }) if action_id == "power.sleep"
        ));

        let mut s = details(&powered());
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Host(2), &mut fx);
        assert!(fx.cmds.is_empty(), "the first press only arms");
        assert_eq!(
            label(&s, Action::Host(2)),
            "Shut down host \u{2014} press again"
        );
        assert_eq!(label(&s, Action::Host(1)), "Restart host");
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Host(1), &mut fx);
        assert!(
            fx.cmds.is_empty(),
            "arming shut down must not leave restart armed"
        );
        let mut s = details(&powered());
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Host(2), &mut fx);
        run_action(&mut s, Action::Host(2), &mut fx);
        assert!(matches!(
            fx.cmds.first(),
            Some(ConsoleCmd::HostAction { action_id, .. }) if action_id == "power.shutdown"
        ));
    }

    #[test]
    fn an_unavailable_action_explains_itself_instead_of_firing() {
        let mut s = details(&HostRow {
            actions: vec![crate::model::HostAction {
                id: "power.sleep".into(),
                label: "Sleep host".into(),
                danger: false,
                available: false,
                unavailable_reason: "this machine does not support sleep".into(),
            }],
            ..host()
        });
        assert!(!s.enabled(Action::Host(0)));
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Host(0), &mut fx);
        assert!(fx.cmds.is_empty(), "no request the host would refuse");
        assert_eq!(
            fx.toast.as_deref(),
            Some("this machine does not support sleep")
        );
    }

    #[test]
    fn default_preset_opens_the_chooser_on_the_hosts_plain_key() {
        let mut s = details(&host());
        let mut fx = Outbox::default();
        run_action(&mut s, Action::BindPreset, &mut fx);
        match fx.nav {
            Some(Nav::Replace(screen)) => match *screen {
                Screen::BindPreset(b) => assert_eq!(b.host_name(), "Desk"),
                _ => panic!("expected the bind-preset chooser"),
            },
            _ => panic!("expected a replace"),
        }
    }

    #[test]
    fn the_clipboard_toggle_flips_the_stored_state() {
        let mut s = details(&host());
        assert!(label(&s, Action::Clipboard).ends_with("Off"));
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Clipboard, &mut fx);
        assert_eq!(
            fx.cmds,
            vec![ConsoleCmd::SetClipboard {
                key: "aa".into(),
                on: true,
            }]
        );
        let mut s = details(&HostRow {
            clipboard_sync: true,
            ..host()
        });
        assert!(label(&s, Action::Clipboard).ends_with("On"));
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Clipboard, &mut fx);
        assert_eq!(
            fx.cmds,
            vec![ConsoleCmd::SetClipboard {
                key: "aa".into(),
                on: false,
            }]
        );
    }

    #[test]
    fn remove_needs_two_presses() {
        let mut s = details(&host());
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Forget, &mut fx);
        assert!(fx.cmds.is_empty(), "the first press only arms");
        assert!(label(&s, Action::Forget).contains("press again"));
        run_action(&mut s, Action::Forget, &mut fx);
        assert_eq!(fx.cmds, vec![ConsoleCmd::ForgetHost { key: "aa".into() }]);
    }

    #[test]
    fn leaving_the_remove_row_disarms_it() {
        let mut s = details(&host());
        let actions = rows(&s);
        s.armed = Some(Action::Forget);
        s.list.cursor = 0;
        let mut fx = Outbox::default();
        with_ctx(|ctx| s.dispatch(ListMsg::None, None, &actions, ctx, &mut fx));
        assert_eq!(
            s.armed, None,
            "a cursor move off the row cancels the arming"
        );
    }

    /// Nothing the poster already does: its OK launches the title. Connect is the other press.
    #[test]
    fn a_title_offers_the_link_its_preset_and_nothing_its_cover_already_does() {
        let s = CardMenu::for_game(&host(), &game());
        assert_eq!(
            rows(&s),
            vec![Action::Connect, Action::CopyLink, Action::BindPreset]
        );
        assert_eq!(label(&s, Action::BindPreset), "Settings preset\u{2026}");
        assert_eq!(s.title(), "Hollow Knight");
    }

    #[test]
    fn a_titles_menu_keeps_the_shelfs_whole_host_so_a_pinned_cards_preset_survives() {
        let s = CardMenu::for_game(&pinned(), &game());
        let Subject::Game { host, id, .. } = &s.subject else {
            panic!("built as a title menu");
        };
        assert_eq!(id, "steam:367520", "the link's launch id");
        assert_eq!(host.pin.as_ref().map(|p| p.id.as_str()), Some("prof-1"));
        assert_eq!(s.host_key(), "aa");
    }

    #[test]
    fn copy_link_always_closes_the_menu_and_says_what_happened() {
        for mut s in [
            CardMenu::for_host(&host()),
            CardMenu::for_game(&host(), &game()),
        ] {
            let mut fx = Outbox::default();
            run_action(&mut s, Action::CopyLink, &mut fx);
            assert!(matches!(fx.nav, Some(Nav::Pop)));
            assert!(fx.toast.is_some());
        }
    }

    #[test]
    fn a_titles_menu_leads_with_resume_when_the_host_has_a_game_up() {
        let idle = CardMenu::for_game(&host(), &game());
        assert_eq!(label(&idle, Action::Connect), "Connect to Desk");
        let up = CardMenu::for_game(
            &HostRow {
                running: "Elden Ring".into(),
                ..host()
            },
            &game(),
        );
        assert_eq!(label(&up, Action::Connect), "Resume Elden Ring");
    }

    #[test]
    fn resume_streams_the_host_without_launching_anything() {
        let mut s = CardMenu::for_game(
            &HostRow {
                running: "Elden Ring".into(),
                ..pinned()
            },
            &game(),
        );
        let mut fx = Outbox::default();
        run_action(&mut s, Action::Connect, &mut fx);
        let intent = fx.connect.expect("a connect intent");
        assert_eq!(intent.launch, None, "resume must not re-launch the title");
        assert_eq!(intent.preset.as_deref(), Some("prof-1"));
        assert_eq!(intent.title, "Elden Ring \u{b7} 4K");
        assert!(matches!(fx.nav, Some(Nav::Pop)));
    }

    /// The label reads back the explicit pointer; pressing the row again clears it.
    #[test]
    fn make_default_sets_then_clears_the_pointer() {
        let saved = HostRow {
            id: Some("rec-1".into()),
            ..host()
        };
        let mut s = details(&saved);
        assert_eq!(label(&s, Action::MakeDefault), "Default host: Off");
        let mut fx = Outbox::default();
        run_action(&mut s, Action::MakeDefault, &mut fx);
        assert_eq!(
            crate::store::file_store().load().default_host.as_deref(),
            Some("rec-1")
        );
        assert_eq!(label(&s, Action::MakeDefault), "Default host: On");
        let mut fx = Outbox::default();
        run_action(&mut s, Action::MakeDefault, &mut fx);
        assert_eq!(crate::store::file_store().load().default_host, None);
    }
}
