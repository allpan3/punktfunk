//! Shared console snapshot and command bus, sibling of [`crate::library::LibraryShared`].
//! Service threads (discovery, probing, pairing, waking, persistence) write snapshots;
//! the shell reads them per frame by generation stamp. Anything that touches the
//! network or disk rides a [`ConsoleCmd`] — the overlay never blocks.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

/// Resolved catalog entry. The service thread opens the profiles file; the shell never
/// does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileChip {
    pub id: String,
    pub name: String,
    /// `#RRGGBB`.
    pub accent: Option<String>,
    /// Bitrate this profile pins, if it pins one; `None` inherits the global. Only the
    /// speed test reads it, to name the layer the tested host actually resolves bitrate
    /// from. A producer that predates the field leaves it `None`, which reads as
    /// "inherits" — the safe half, since that is where the console writes.
    #[serde(default)]
    pub bitrate_kbps: Option<u32>,
}

/// Home carousel row, fully resolved by the service thread. The shell renders it
/// verbatim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostRow {
    /// Fingerprint when pinned, else `addr:port` — cursor identity across snapshot churn.
    pub key: String,
    /// The store record's stable id (`KnownHost::id`), what "Make default host" points at.
    /// `None` on a merely discovered row, and on any row a producer built before this
    /// field existed — `serde(default)` because Android's bridge is one of them.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub addr: String,
    pub port: u16,
    /// Lowercase hex fingerprint; empty = not pinned.
    pub fp_hex: String,
    pub paired: bool,
    /// In the known-hosts store, not merely discovered.
    pub saved: bool,
    /// mDNS advert or last probe succeeded.
    pub online: bool,
    /// Management API port (mDNS TXT or store).
    pub mgmt_port: u16,
    /// Offline with a stored MAC: Wake & Connect is offered.
    pub can_wake: bool,
    /// Per-host clipboard share while streaming (`KnownHost::clipboard_sync`).
    #[serde(default)]
    pub clipboard_sync: bool,
    /// Last successful connect, UNIX seconds.
    pub last_used: Option<u64>,
    /// OS-identity chain: live advert preferred, else stored. Empty = unknown.
    pub os: String,
    /// Host-reported extras (`design/host-actions.md`): sleep, restart, shut down.
    /// Empty when the host is unreachable or the route does not exist.
    #[serde(default)]
    pub actions: Vec<HostAction>,
    /// Pinned-profile shortcut after the host's primary tile, sharing its live state.
    /// `None` = this row is the primary tile.
    pub pin: Option<ProfileChip>,
    /// Default profile (`KnownHost::profile_id`). Always `None` on a pinned row — that
    /// profile is `pin`.
    pub bound_profile: Option<ProfileChip>,
    /// What this host has up right now (`GET /api/v1/status`), as a title to show.
    /// Empty = nothing running, unpaired, unreachable, or a host too old to ask —
    /// every one of which the tile renders the same way: no line.
    ///
    /// Host state, never store state: `serde(default)` so a producer predating the
    /// field still parses, and it is never persisted, which is what stops a carousel
    /// coming back claiming a game is up because it was up last night.
    #[serde(default)]
    pub running: String,
    /// Library title id → profile id (`KnownHost::game_profiles`), for the bind screen's
    /// checkmark. Ids, not chips: the shell only compares them, and a title's binding
    /// outranks `bound_profile` at launch, which the host resolves.
    #[serde(default)]
    pub game_profiles: BTreeMap<String, String>,
}

/// One host-offered action, resolved from `GET /api/v1/actions`
/// (`design/host-actions.md`). `label` is already chosen: this client's wording for a
/// known id, else the host's title, so a new host action renders without a console
/// release.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostAction {
    /// Invoke argument (`power.sleep`).
    pub id: String,
    pub label: String,
    /// Confirm twice: the action drops host state (restart, shut down).
    pub danger: bool,
    /// Host can run it now. `false` still shows the row, disabled — do not hide it.
    pub available: bool,
    #[serde(default)]
    pub unavailable_reason: String,
}

/// Pairing ceremony state. One at a time: the ceremony is modal.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub enum PairPhase {
    #[default]
    Idle,
    /// SPAKE2 in flight; can run ~90 s if the PIN is retried.
    Busy,
    Failed(String),
    /// Paired and persisted. `key` is the host's refreshed row.
    Paired {
        key: String,
    },
}

/// A wake-and-wait in progress (one at a time). The service thread re-sends magic
/// packets and probes; the shell renders the card and acts on `online`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WakeStatus {
    pub key: String,
    pub name: String,
    /// Seconds since the wake started.
    pub seconds: u32,
    pub timed_out: bool,
    /// Probe answered. The shell launches if `then_connect`.
    pub online: bool,
    /// Connect once awake, versus a bare wake.
    pub then_connect: bool,
}

/// A network speed test in progress (one at a time). The service thread connects, asks the
/// host to burst, and reports; the shell renders the takeover and applies the answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpeedStatus {
    /// [`HostRow::key`] of the tested host — the shell re-reads the row to name the layer
    /// Apply writes to, rather than trusting a copy taken when the test started.
    pub key: String,
    pub name: String,
    pub phase: SpeedPhase,
}

/// Where a speed test is: it connects, it measures, then it has an answer or a reason.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SpeedPhase {
    Connecting,
    Measuring,
    Failed(String),
    /// `recommended_kbps` keeps headroom under `throughput_kbps` for FEC and for the loss a
    /// real stream meets — [`pf_client_core::speed::recommended_kbps`], so every client
    /// recommends the same kilobit.
    Done {
        throughput_kbps: u32,
        loss_pct: f32,
        recommended_kbps: u32,
    },
}

#[derive(Default)]
struct ConsoleState {
    hosts: Vec<HostRow>,
    hosts_gen: u64,
    pair: PairPhase,
    wake: Option<WakeStatus>,
    speed: Option<SpeedStatus>,
    /// One-shot toast. The shell `take`s it on the next sync — unlike [`PairPhase`]
    /// there is no modal state, so a take-once string is the whole protocol.
    notice: Option<String>,
}

/// Service threads write; the shell polls per frame. Cheap locks; no GPU data.
#[derive(Clone, Default)]
pub struct ConsoleShared(Arc<Mutex<ConsoleState>>);

impl ConsoleShared {
    pub fn set_hosts(&self, hosts: Vec<HostRow>) {
        let mut s = self.0.lock().unwrap();
        if s.hosts != hosts {
            s.hosts = hosts;
            s.hosts_gen += 1;
        }
    }

    pub(crate) fn hosts_gen(&self) -> u64 {
        self.0.lock().unwrap().hosts_gen
    }

    pub(crate) fn hosts_snapshot(&self) -> (Vec<HostRow>, u64) {
        let s = self.0.lock().unwrap();
        (s.hosts.clone(), s.hosts_gen)
    }

    pub fn set_pair(&self, phase: PairPhase) {
        self.0.lock().unwrap().pair = phase;
    }

    pub(crate) fn pair(&self) -> PairPhase {
        self.0.lock().unwrap().pair.clone()
    }

    pub fn set_wake(&self, wake: Option<WakeStatus>) {
        self.0.lock().unwrap().wake = wake;
    }

    pub(crate) fn wake(&self) -> Option<WakeStatus> {
        self.0.lock().unwrap().wake.clone()
    }

    /// `None` closes the takeover. The shell also clears it when the player dismisses, so
    /// a service thread that reports a late phase must not resurrect a closed test — see
    /// [`Self::advance_speed`].
    pub fn set_speed(&self, speed: Option<SpeedStatus>) {
        self.0.lock().unwrap().speed = speed;
    }

    /// Report a new phase for the test on `key`. A no-op once the shell has cleared the slot,
    /// and a no-op for a different host: the burst outlives a dismiss, so its result must
    /// neither reopen the takeover nor land under the name of a test started since.
    pub fn advance_speed(&self, key: &str, phase: SpeedPhase) {
        let mut s = self.0.lock().unwrap();
        if let Some(sp) = s.speed.as_mut().filter(|sp| sp.key == key) {
            sp.phase = phase;
        }
    }

    pub(crate) fn speed(&self) -> Option<SpeedStatus> {
        self.0.lock().unwrap().speed.clone()
    }

    /// One-shot toast. A newer notice replaces an unshown older one.
    pub fn set_notice(&self, text: String) {
        self.0.lock().unwrap().notice = Some(text);
    }

    pub(crate) fn take_notice(&self) -> Option<String> {
        self.0.lock().unwrap().notice.take()
    }
}

/// Overlay→binary work. Every variant blocks on network or disk; never on the
/// render path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConsoleCmd {
    FetchLibrary {
        addr: String,
        mgmt: u16,
        fp_hex: String,
    },
    /// Re-read running titles (`GET /api/v1/status`) without touching the catalog.
    /// Not [`Self::FetchLibrary`]: that sets `Loading` and would replace the shelf
    /// with a spinner.
    RefreshRunning {
        addr: String,
        mgmt: u16,
        fp_hex: String,
    },
    /// SPAKE2 PIN ceremony; on success persist the pin and refresh hosts.
    Pair {
        addr: String,
        port: u16,
        pin: String,
        device_name: String,
    },
    /// Upload the log ring to this paired host's management API. Same transport as
    /// [`Self::FetchLibrary`]; the result is a notice toast. For platforms whose own
    /// logs are unreachable.
    SendLogs {
        addr: String,
        mgmt: u16,
        fp_hex: String,
        host_name: String,
    },
    /// Measure the path to this host over the real data plane: connect, ask it to burst,
    /// report goodput and loss. Progress arrives back as [`ConsoleShared::advance_speed`],
    /// not as a notice — the takeover narrates it and holds the Apply button.
    ///
    /// A second connect, not the running stream's: the console offers this out of session,
    /// and a burst down a live stream is what the host's keyframe-at-probe-end guard exists
    /// to survive rather than something to invite.
    SpeedTest {
        key: String,
        addr: String,
        port: u16,
        fp_hex: String,
        host_name: String,
    },
    /// Save a manually entered host, unpaired, and refresh the rows.
    SaveHost {
        name: String,
        addr: String,
        port: u16,
    },
    /// Rename or re-address a saved host. Fingerprint, pins, and MACs stay — this
    /// edits the row, it does not replace it.
    UpdateHost {
        key: String,
        name: String,
        addr: String,
        port: u16,
    },
    /// Drop a saved host. The next connect to that address has no pin, pairing, or
    /// pinned cards.
    ForgetHost {
        key: String,
    },
    Wake {
        key: String,
        then_connect: bool,
    },
    /// Stop the wake loop and clear its status.
    CancelWake,
    Probe,
    /// Pin or unpin a profile card on a saved host (`KnownHost::pinned_profiles`).
    /// `key` is the host row. Presentation only: does not touch the default binding
    /// or the profile. Idempotent.
    SetPin {
        key: String,
        profile_id: String,
        pin: bool,
    },
    /// Bind or clear a profile. `game` names a library title
    /// (`KnownHost::game_profiles`); `None` binds the host's own default
    /// (`KnownHost::profile_id`). [`Self::SetPin`] is presentation; this is the
    /// binding. `profile_id: None` clears. Idempotent.
    BindProfile {
        key: String,
        #[serde(default)]
        game: Option<String>,
        profile_id: Option<String>,
    },
    /// Per-host clipboard share while streaming (`KnownHost::clipboard_sync`).
    /// Never global.
    SetClipboard {
        key: String,
        on: bool,
    },
    /// Open a platform-owned overlay (`design/android-skia-console-port.md`).
    /// `id` is [`crate::platform::PlatformScreen::id`]. The host draws it and holds
    /// input; the console never sees the pixels. Desktop raises none.
    OpenPlatformScreen {
        id: String,
    },
    /// Platform-only pad work. `action` is [`crate::screens::controllers::PadAction::id`];
    /// `pad_key` indexes [`crate::screens::Ctx::pads`] and is empty when the pad list
    /// cannot name the device. One command, not one per button: the host's answer is
    /// always "do it, report as a notice", and a command per grant would span three crates.
    PadAction {
        action: String,
        pad_key: String,
    },
    /// Invoke a host action (`design/host-actions.md`). Same lane as [`Self::SendLogs`].
    /// Parameterised by `action_id` like [`Self::PadAction`]: a command per verb would
    /// span three crates. Outcome is a notice toast.
    HostAction {
        addr: String,
        mgmt: u16,
        fp_hex: String,
        host_name: String,
        /// Stable id (`power.sleep`).
        action_id: String,
        /// Resolved label for the toast — the service thread must not re-derive wording
        /// the screen already settled.
        label: String,
    },
}

/// Overlay→binary command queue. Same locking as the shared models. Drain cadence
/// is not latency-critical: every effect arrives via a model snapshot.
#[derive(Clone, Default)]
pub struct ConsoleBus(Arc<Mutex<VecDeque<ConsoleCmd>>>);

impl ConsoleBus {
    /// Queue a command. The binary may seed one too (direct-entry library fetch) —
    /// same lane, same handler.
    pub fn send(&self, cmd: ConsoleCmd) {
        self.0.lock().unwrap().push_back(cmd);
    }

    pub fn drain(&self) -> Vec<ConsoleCmd> {
        self.0.lock().unwrap().drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_generation_bumps_only_on_change() {
        let shared = ConsoleShared::default();
        let row = HostRow {
            key: "aa".into(),
            id: None,
            name: "Tower".into(),
            addr: "10.0.0.2".into(),
            port: 9777,
            fp_hex: "aa".into(),
            paired: true,
            saved: true,
            online: false,
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
        };
        shared.set_hosts(vec![row.clone()]);
        let g1 = shared.hosts_gen();
        shared.set_hosts(vec![row.clone()]);
        assert_eq!(shared.hosts_gen(), g1, "identical snapshot doesn't churn");
        shared.set_hosts(vec![HostRow {
            online: true,
            ..row
        }]);
        assert_eq!(shared.hosts_gen(), g1 + 1);
    }

    #[test]
    fn bus_drains_in_order() {
        let bus = ConsoleBus::default();
        bus.send(ConsoleCmd::Probe);
        bus.send(ConsoleCmd::CancelWake);
        assert_eq!(bus.drain(), vec![ConsoleCmd::Probe, ConsoleCmd::CancelWake]);
        assert!(bus.drain().is_empty());
    }
}
