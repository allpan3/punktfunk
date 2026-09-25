//! Keyboard release recovery on the lossy input plane
//!
//! Every key edge and snapshot shares a wrapping sequence, gated by HOST_CAP2_KEY_STATE
//! Snapshots release omitted keys but never synthesize a lost press or an autorepeat
//! Per-key sequence tracking rejects stale edges without discarding other keys' edges
//! Empty snapshots continue after keyboard use so recovery survives an arbitrary loss burst
//! A full snapshot is incomplete and cannot authorize releases
//! Tests exercise the sender and receiver together under loss and reordering

use super::{
    keys_held_codes, keys_held_event, InputEvent, InputKind, KEYS_HELD_MAX, KEY_FLAG_SEQUENCE,
};
use std::collections::{BTreeSet, HashSet};

/// Sequence and snapshot all keyboard events at the final outbound boundary
pub struct KeyStateSender {
    enabled: bool,
    active: bool,
    seq: u32,
    held: BTreeSet<u8>,
}

impl KeyStateSender {
    /// Leave legacy peers' key events unchanged
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            active: false,
            seq: 0,
            held: BTreeSet::new(),
        }
    }

    /// Track the event actually sent, including controller-generated keys
    pub fn prepare(&mut self, ev: &mut InputEvent) {
        if !self.enabled || !matches!(ev.kind, InputKind::KeyDown | InputKind::KeyUp) {
            return;
        }
        let Ok(vk @ 1..) = u8::try_from(ev.code) else {
            return;
        };
        self.active = true;
        if ev.kind == InputKind::KeyDown {
            self.held.insert(vk);
        } else {
            self.held.remove(&vk);
        }
        self.seq = self.seq.wrapping_add(1);
        ev.x = self.seq as i32;
        ev.flags |= KEY_FLAG_SEQUENCE;
    }

    /// Continue the empty state until disconnect so a lost final release can recover
    pub fn snapshot(&mut self) -> Option<InputEvent> {
        if !self.active {
            return None;
        }
        self.seq = self.seq.wrapping_add(1);
        Some(keys_held_event(self.seq, self.held.iter().copied()))
    }
}

/// Remember accepted state per VK so reordered datagrams cannot restore stale presses
pub struct KeyStateReceiver {
    seq: [Option<u32>; 256],
}

impl Default for KeyStateReceiver {
    /// Start without a sequence baseline for legacy clients
    fn default() -> Self {
        Self { seq: [None; 256] }
    }
}

/// Compare wrapping sequences within half their range
fn newer(seq: u32, previous: Option<u32>) -> bool {
    previous.is_none_or(|old| (seq.wrapping_sub(old) as i32) > 0)
}

impl KeyStateReceiver {
    /// Accept legacy edges or a newer sequenced edge for this key
    pub fn accept(&mut self, ev: &InputEvent) -> bool {
        if !matches!(ev.kind, InputKind::KeyDown | InputKind::KeyUp)
            || ev.flags & KEY_FLAG_SEQUENCE == 0
        {
            return true;
        }
        let Ok(vk @ 1..) = u8::try_from(ev.code) else {
            return false;
        };
        let slot = &mut self.seq[usize::from(vk)];
        if !newer(ev.x as u32, *slot) {
            return false;
        }
        *slot = Some(ev.x as u32);
        true
    }

    /// Advance only older key states; a full list supplies no evidence of absence
    pub fn releases(&mut self, snapshot: &InputEvent, held: &HashSet<u32>) -> Vec<u32> {
        let (codes, n) = keys_held_codes(snapshot);
        if n == KEYS_HELD_MAX {
            return Vec::new();
        }
        let mut released = Vec::new();
        for vk in 1..=u8::MAX {
            let slot = &mut self.seq[usize::from(vk)];
            if newer(snapshot.flags, *slot) {
                *slot = Some(snapshot.flags);
                if held.contains(&u32::from(vk)) && !codes[..n].contains(&vk) {
                    released.push(u32::from(vk));
                }
            }
        }
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Construct an edge through the production sender and wire codec
    fn edge(sender: &mut KeyStateSender, code: u32, down: bool) -> InputEvent {
        let mut ev = InputEvent {
            kind: if down {
                InputKind::KeyDown
            } else {
                InputKind::KeyUp
            },
            _pad: [0; 3],
            code,
            x: 0,
            y: 0,
            flags: 0,
        };
        sender.prepare(&mut ev);
        InputEvent::decode(&ev.encode()).unwrap()
    }

    // A short tap can lose its up and every early empty snapshot during an outage
    #[test]
    fn a_lost_release_recovers_after_a_long_loss_burst() {
        let mut sender = KeyStateSender::new(true);
        let mut receiver = KeyStateReceiver::default();
        assert!(sender.snapshot().is_none());
        assert!(receiver.accept(&edge(&mut sender, 0x41, true)));
        let held = HashSet::from([0x41]);
        let _lost_up = edge(&mut sender, 0x41, false);
        for _ in 0..100 {
            assert!(sender.snapshot().is_some());
        }
        assert_eq!(
            receiver.releases(&sender.snapshot().unwrap(), &held),
            [0x41]
        );
    }

    // A newer press survives an arbitrarily delayed empty snapshot
    #[test]
    fn an_old_snapshot_cannot_release_a_new_press() {
        let mut sender = KeyStateSender::new(true);
        let mut receiver = KeyStateReceiver::default();
        edge(&mut sender, 0x41, false);
        let old = sender.snapshot().unwrap();
        assert!(receiver.accept(&edge(&mut sender, 0x41, true)));
        assert!(receiver.releases(&old, &HashSet::from([0x41])).is_empty());
    }

    // Both an actual up and a recovery snapshot reject delayed downs and repeats
    #[test]
    fn released_keys_cannot_be_resurrected_by_delayed_edges() {
        for snapshot in [false, true] {
            let mut sender = KeyStateSender::new(true);
            let mut receiver = KeyStateReceiver::default();
            let down = edge(&mut sender, 0x41, true);
            let repeat = edge(&mut sender, 0x41, true);
            let up = edge(&mut sender, 0x41, false);
            if snapshot {
                receiver.releases(&sender.snapshot().unwrap(), &HashSet::new());
            } else {
                assert!(receiver.accept(&up));
            }
            assert!(!receiver.accept(&down));
            assert!(!receiver.accept(&repeat));
        }
    }

    // A late up from an earlier tap must not end a second press of that key
    #[test]
    fn a_delayed_release_cannot_end_a_second_press() {
        let mut sender = KeyStateSender::new(true);
        let mut receiver = KeyStateReceiver::default();
        edge(&mut sender, 0x41, true);
        let old_up = edge(&mut sender, 0x41, false);
        assert!(receiver.accept(&edge(&mut sender, 0x41, true)));
        assert!(!receiver.accept(&old_up));
    }

    // Receiving a different key first must not discard this key's newer transition
    #[test]
    fn sequences_are_tracked_per_key() {
        let mut sender = KeyStateSender::new(true);
        let mut receiver = KeyStateReceiver::default();
        let a = edge(&mut sender, 0x41, true);
        let b = edge(&mut sender, 0x42, true);
        assert!(receiver.accept(&b));
        assert!(receiver.accept(&a));
        assert!(!receiver.accept(&a));
        assert!(receiver
            .releases(&sender.snapshot().unwrap(), &HashSet::from([0x41, 0x42]))
            .is_empty());
    }

    // A truncated snapshot cannot release a held key omitted only for lack of space
    #[test]
    fn a_saturated_snapshot_does_not_advance_or_release_keys() {
        let mut receiver = KeyStateReceiver::default();
        let mut sender = KeyStateSender::new(true);
        let down = edge(&mut sender, 0xFE, true);
        let snap = keys_held_event(100, 1..=20);
        assert!(receiver.releases(&snap, &HashSet::from([0xFE])).is_empty());
        assert!(receiver.accept(&down));
    }

    // A wrapping sequence remains ordered through zero
    #[test]
    fn sequence_wrap_preserves_release_order() {
        let mut sender = KeyStateSender::new(true);
        sender.seq = u32::MAX - 1;
        let mut receiver = KeyStateReceiver::default();
        let down = edge(&mut sender, 0x41, true);
        assert!(receiver.accept(&down));
        assert!(receiver.accept(&edge(&mut sender, 0x41, false)));
        assert!(!receiver.accept(&down));
    }

    // Older peers retain ordinary key events and send no recovery datagrams
    #[test]
    fn legacy_keyboard_events_remain_unchanged() {
        let mut sender = KeyStateSender::new(false);
        let mut receiver = KeyStateReceiver::default();
        let down = edge(&mut sender, 0x41, true);
        assert_eq!((down.x, down.flags), (0, 0));
        assert!(receiver.accept(&down));
        assert!(receiver.accept(&down));
        assert!(sender.snapshot().is_none());
    }
}
