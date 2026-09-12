//! Portable console driver: the object a host holds (`android-skia-console-port.md`).
//! Owns the shell and fonts; host-facing vocabulary only: a canvas + [`Viewport`]
//! per frame, [`MenuEvent`]s, [`PointerInput`], [`Key`]s and text in;
//! [`OverlayAction`]s out; [`SessionPhase`] edges back. The Vulkan session's
//! [`crate::SkiaOverlay`] and the Android GL host both sit on this; nothing here
//! knows a `VkImage`, an SDL event, or a JNI env.

use crate::model::{ConsoleBus, ConsoleCmd, ConsoleShared, HostRow};
use crate::screens::Screen;
use crate::shell::{ConsoleOptions, Shell};
use crate::theme::Fonts;
use anyhow::Result;
use pf_client_core::console::{OverlayAction, PointerInput, SessionPhase};
use pf_client_core::menu_nav::{MenuEvent, MenuPulse, PadInfo};
use punktfunk_core::config::GamepadPref;
use skia_safe::Canvas;

pub use crate::input::Key;

/// Device family for the hint legend. Pointer has no source: a tap does not say
/// which buttons the other hand holds, so the legend stays as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSource {
    /// Glyphs follow the active pad's family.
    Pad,
    /// TV remote D-pad on Android; keyboard on desktop.
    Keys,
}

pub enum ConsoleEntry {
    /// Host list (`--browse`; Android Home).
    Home,
    /// Home with this host's library pushed (`--browse host`). B pops to Home.
    /// `Box` because `HostRow` is larger than the other variant.
    Library(Box<HostRow>),
    /// [`Self::Library`] plus one connect to the host's desktop, raised before the first
    /// frame (`start_in = stream`). Cancel or a refusal lands on the shelf underneath,
    /// and nothing retries.
    Stream(Box<HostRow>),
}

/// Host-side models and the command bus. Built before [`Console`]: handles are `Clone` +
/// thread-safe so the host can keep them on one thread and build the (not `Send`) console
/// on the draw thread.
#[derive(Clone, Default)]
pub struct ConsoleHandles {
    pub console: ConsoleShared,
    pub library: crate::library::LibraryShared,
    pub bus: ConsoleBus,
}

impl ConsoleHandles {
    pub fn new() -> ConsoleHandles {
        ConsoleHandles::default()
    }
}

/// Safe-area insets in device pixels. Chrome stays inside; the backdrop still paints
/// edge to edge. Zero on desktop.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub insets: Insets,
    /// Device pixels per design unit. `None` uses `(height / 800).clamp(0.75, 3.0)`
    /// (Deck 1×, 4K TV 2.7×). A phone in hand should pass a density floor.
    pub scale: Option<f64>,
}

impl Viewport {
    pub fn plain(width: u32, height: u32) -> Viewport {
        Viewport {
            width,
            height,
            insets: Insets::default(),
            scale: None,
        }
    }
}

pub struct Console {
    shell: Shell,
    fonts: Fonts,
}

impl Console {
    /// Not `Send` (Skia). Build on the thread that will draw.
    pub fn new(
        opts: ConsoleOptions,
        entry: ConsoleEntry,
        handles: &ConsoleHandles,
    ) -> Result<Console> {
        let stream = stream_intent(&entry);
        let fetch = entry_fetch(&entry);
        let stack = entry_stack(entry, &handles.library);
        // After the shelf samples the epoch: the host may drain this before the first frame.
        if let Some(cmd) = fetch {
            handles.bus.send(cmd);
        }
        let mut shell = Shell::new(
            handles.console.clone(),
            handles.library.clone(),
            handles.bus.clone(),
            opts,
            stack,
        )?;
        if let Some(intent) = stream {
            shell.start_connect(intent);
        }
        let fonts = crate::theme::build_fonts()?;
        Ok(Console { shell, fonts })
    }

    /// `pad` is the chip label; `None` means no controller.
    pub fn frame(
        &mut self,
        canvas: &Canvas,
        viewport: &Viewport,
        pad: Option<&str>,
        pad_pref: Option<GamepadPref>,
        pads: &[PadInfo],
    ) {
        self.shell
            .render_in(canvas, viewport, &self.fonts, pad, pad_pref, pads);
    }

    pub fn menu(&mut self, event: MenuEvent, source: InputSource) -> Option<MenuPulse> {
        self.shell.note_input_source(source);
        self.shell.handle_menu(event)
    }

    /// Pointer in surface pixels; the shell subtracts insets.
    pub fn pointer(&mut self, input: PointerInput) -> bool {
        self.shell.pointer_input(input)
    }

    pub fn key(&mut self, key: Key, shift: bool, repeat: bool) -> bool {
        self.shell.key(key, shift, repeat)
    }

    pub fn text(&mut self, text: &str) {
        self.shell.text_input(text);
    }

    /// True while a field is being edited: keep IME / SDL text-input started, and
    /// route printable keys as text, not [`Key`]s.
    pub fn editing(&self) -> bool {
        self.shell.editing()
    }

    pub fn session_phase(&mut self, phase: SessionPhase) {
        self.shell.session_phase(phase);
    }

    /// Drain after every input and every frame.
    pub fn take_action(&mut self) -> Option<OverlayAction> {
        self.shell.take_action()
    }

    /// What a screen reader should speak for the focused row: its label, then its value.
    ///
    /// Poll it and speak only when the string changes — a reader that repeats itself is
    /// worse than silence. `None` is a focus this driver does not describe (or a takeover
    /// holding the input), and the host then says nothing at all.
    pub fn focus_announcement(&mut self) -> Option<String> {
        self.shell.focus_announcement()
    }

    /// Console is off screen; the shell keeps its stack for return.
    pub fn in_stream(&self) -> bool {
        self.shell.in_stream
    }

    /// Replace the stack with `entry` (deep link, or return to the shelf a game launched from).
    pub fn navigate(&mut self, entry: ConsoleEntry) {
        let stream = stream_intent(&entry);
        let stack = entry_stack(entry, self.shell.library());
        self.shell.replace_stack(stack);
        if let Some(intent) = stream {
            self.shell.start_connect(intent);
        }
    }

    /// Skia resource-cache budget for the host `DirectContext`. The shell only carries it.
    pub fn gpu_cache_bytes(&self) -> usize {
        self.shell.gpu_cache_bytes
    }

    /// Shell and fonts for the Vulkan overlay: stream chrome uses the same fonts; the
    /// overlay holds the shell as `Option`.
    #[cfg(feature = "vulkan-overlay")]
    pub(crate) fn into_parts(self) -> (Shell, Fonts) {
        (self.shell, self.fonts)
    }
}

/// The desktop connect a [`ConsoleEntry::Stream`] carries. Built from the entry's own
/// row, never from `Shell::hosts` — that list is empty until the first `sync()`. Same
/// shape as the Options screen's "Connect to X": no launch, no profile override.
fn stream_intent(entry: &ConsoleEntry) -> Option<crate::screens::ConnectIntent> {
    let ConsoleEntry::Stream(host) = entry else {
        return None;
    };
    Some(crate::screens::ConnectIntent {
        addr: host.addr.clone(),
        port: host.port,
        fp_hex: host.fp_hex.clone(),
        launch: None,
        title: host.name.clone(),
        request_access: false,
        profile: None,
    })
}

/// The start shelf's library fetch, queued by [`Console::new`] like every other shelf push.
/// [`Console::navigate`] sends none: it re-roots on the list the model already holds.
fn entry_fetch(entry: &ConsoleEntry) -> Option<ConsoleCmd> {
    let (ConsoleEntry::Library(host) | ConsoleEntry::Stream(host)) = entry else {
        return None;
    };
    Some(ConsoleCmd::FetchLibrary {
        addr: host.addr.clone(),
        mgmt: host.mgmt_port,
        fp_hex: host.fp_hex.clone(),
    })
}

fn entry_stack(entry: ConsoleEntry, library: &crate::library::LibraryShared) -> Vec<Screen> {
    match entry {
        ConsoleEntry::Home => vec![Screen::Home(crate::screens::home::HomeScreen::new())],
        ConsoleEntry::Library(host) | ConsoleEntry::Stream(host) => vec![
            Screen::Home(crate::screens::home::HomeScreen::new()),
            // Snapshot the model's fetch epoch before the entry's `FetchLibrary` is queued;
            // that is how the shelf knows the result is its own.
            Screen::Library(crate::screens::library::LibraryScreen::new(
                &host,
                library.fetch_epoch(),
            )),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> HostRow {
        HostRow {
            key: "aa".into(),
            id: Some("rec-1".into()),
            name: "Desk".into(),
            addr: "10.0.0.5".into(),
            port: 9777,
            fp_hex: "aa".into(),
            paired: true,
            saved: true,
            online: true,
            mgmt_port: 47990,
            can_wake: false,
            clipboard_sync: false,
            last_used: None,
            os: String::new(),
            actions: Vec::new(),
            pin: None,
            bound_profile: None,
            running: String::new(),
            game_profiles: Default::default(),
        }
    }

    /// Stream is the Library stack plus a desktop connect. Nothing else raises one,
    /// and the connect launches no title and overrides no profile.
    #[test]
    fn only_a_stream_entry_carries_a_connect() {
        assert!(stream_intent(&ConsoleEntry::Home).is_none());
        assert!(stream_intent(&ConsoleEntry::Library(Box::new(row()))).is_none());

        let intent =
            stream_intent(&ConsoleEntry::Stream(Box::new(row()))).expect("a desktop connect");
        assert_eq!(intent.addr, "10.0.0.5");
        assert_eq!(intent.launch, None);
        assert_eq!(intent.profile, None);
        assert!(!intent.request_access);
        assert_eq!(intent.title, "Desk");
    }

    /// A host entry opens a shelf, and a shelf nobody fetches for spins forever.
    #[test]
    fn a_host_entry_queues_its_shelfs_fetch() {
        assert_eq!(entry_fetch(&ConsoleEntry::Home), None);
        for entry in [
            ConsoleEntry::Library(Box::new(row())),
            ConsoleEntry::Stream(Box::new(row())),
        ] {
            assert_eq!(
                entry_fetch(&entry),
                Some(ConsoleCmd::FetchLibrary {
                    addr: "10.0.0.5".into(),
                    mgmt: 47990,
                    fp_hex: "aa".into(),
                })
            );
        }
    }

    /// Both host entries land on the same two screens, so B leaves a cancelled stream
    /// on the shelf rather than on the host list.
    #[test]
    fn a_stream_entry_opens_the_same_stack_as_library() {
        let library = crate::library::LibraryShared::default();
        for entry in [
            ConsoleEntry::Library(Box::new(row())),
            ConsoleEntry::Stream(Box::new(row())),
        ] {
            let stack = entry_stack(entry, &library);
            assert!(matches!(
                stack.as_slice(),
                [Screen::Home(_), Screen::Library(_)]
            ));
        }
    }
}
