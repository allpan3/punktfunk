//! The client pad's power state, packed into whatever byte each virtual pad's input report
//! uses. One place, because the three families disagree about the same physical pad:
//! `hid-playstation` reads a DualSense status nibble, a DualShock 4 cable bit and capacity,
//! `hid-nintendo` a level/charging/host-powered field. Steam and the kernel draw a battery
//! icon from these, and a level nobody sets reads as ~5 % and warns.

use punktfunk_core::quic::{PAD_BATTERY_UNKNOWN, PAD_STATUS_CHARGING, PAD_STATUS_WIRED};

/// What the client says about the controller in the player's hands
/// ([`RichInput::PadStatus`](punktfunk_core::quic::RichInput::PadStatus)).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PadPower {
    /// Charge, 0..=100. `None` is no reading — a wired pad with no pack, or a client that
    /// cannot see one.
    pub level: Option<u8>,
    pub charging: bool,
    /// On a cable or a dock.
    pub wired: bool,
}

impl Default for PadPower {
    /// Wired, full, charging: what every backend claimed before the wire carried this, and
    /// what a pad whose client never samples must keep claiming. The alternative reads as an
    /// empty battery on a controller that is fine.
    fn default() -> PadPower {
        PadPower {
            level: None,
            charging: true,
            wired: true,
        }
    }
}

impl PadPower {
    /// One `PadStatus` from the wire. Unknown flag bits are reserved and ignored here.
    pub fn from_wire(battery: u8, flags: u8) -> PadPower {
        PadPower {
            level: (battery != PAD_BATTERY_UNKNOWN).then(|| battery.min(100)),
            charging: flags & PAD_STATUS_CHARGING != 0,
            wired: flags & PAD_STATUS_WIRED != 0,
        }
    }

    /// Percent for the report codecs. No reading means full: a pad that cannot measure its
    /// pack is one that runs off the cable.
    fn pct(&self) -> u8 {
        self.level.unwrap_or(100)
    }

    fn plugged(&self) -> bool {
        self.wired || self.charging
    }

    /// DualSense report `0x01`, struct offset 52: high nibble charge state (0 discharging,
    /// 1 charging, 2 full — the kernel forces 100 % on 2), low nibble capacity in 10 %
    /// steps. `hid-playstation.c` `dualsense_parse_report`, `SDL_hidapi_ps5.c`.
    pub fn ds5_byte(&self) -> u8 {
        let state = match (self.plugged(), self.pct() >= 100) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 2,
        };
        state << 4 | (self.pct() / 10).min(10)
    }

    /// DualShock 4 report, struct offset 29: bit 4 cable, low nibble capacity. On the cable
    /// the kernel reads the nibble as `n × 10 + 10` and 10/11 as full; on battery as
    /// `n × 10 + 5`. `hid-playstation.c` `dualshock4_parse_report`.
    pub fn ds4_byte(&self) -> u8 {
        if !self.plugged() {
            return (self.pct() / 10).min(10);
        }
        0x10 | if self.pct() >= 100 {
            0x0B
        } else {
            (self.pct() / 10).min(9)
        }
    }

    /// Switch Pro report byte 2: bits 7–5 level (0 critical … 4 full), bit 4 charging,
    /// bit 0 host-powered. `hid-nintendo` `joycon_parse_report`, `SDL_hidapi_switch.c`.
    pub fn switch_byte(&self) -> u8 {
        let level: u8 = match self.pct() {
            0..=9 => 0,
            10..=39 => 1,
            40..=69 => 2,
            70..=89 => 3,
            _ => 4,
        };
        level << 5 | u8::from(self.charging) << 4 | u8::from(self.wired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No client sample must keep each family's shipped claim byte-for-byte: DualSense
    /// `0x2A` (full), DualShock 4 `0x1B` (cable + full), Switch `0x91` (full, charging,
    /// USB). Anything else re-draws a battery icon on a pad nobody asked about.
    #[test]
    fn no_sample_keeps_the_wired_and_full_claim() {
        let d = PadPower::default();
        assert_eq!(
            (d.ds5_byte(), d.ds4_byte(), d.switch_byte()),
            (0x2A, 0x1B, 0x91)
        );
    }

    #[test]
    fn a_draining_pad_reports_its_level_on_every_family() {
        let p = PadPower::from_wire(45, 0);
        assert_eq!(p.ds5_byte(), 0x04, "discharging, 4 → 45 %");
        assert_eq!(p.ds4_byte(), 0x04, "no cable bit, 4 → 45 %");
        assert_eq!(
            p.switch_byte(),
            0x40,
            "level 2 of 4, not charging, on battery"
        );
    }

    #[test]
    fn charging_is_a_third_state_not_a_level() {
        let p = PadPower::from_wire(45, PAD_STATUS_CHARGING | PAD_STATUS_WIRED);
        assert_eq!(p.ds5_byte(), 0x14, "status 1 = charging");
        assert_eq!(p.ds4_byte(), 0x14, "cable bit + 4 → 50 %");
        assert_eq!(p.switch_byte(), 0x51);
        // Full on the cable is FULL, not charging-forever.
        assert_eq!(PadPower::from_wire(100, PAD_STATUS_WIRED).ds5_byte(), 0x2A);
    }

    /// An unknown level is the wired-and-full claim; a client that reads one wins over it.
    #[test]
    fn unknown_level_falls_back_but_flags_still_count() {
        let p = PadPower::from_wire(PAD_BATTERY_UNKNOWN, PAD_STATUS_WIRED);
        assert_eq!(p.level, None);
        assert_eq!(p.ds5_byte(), PadPower::default().ds5_byte());
        assert_eq!(PadPower::from_wire(200, 0).level, Some(100), "clamped");
    }
}
