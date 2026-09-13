//! Slot-family RFI recovery **policy** for the three backends that answer a loss
//! with a re-reference to a known-good older frame instead of an IDR: native AMF
//! (user-LTR bitfield), native QSV (`mfxExtRefListCtrl` LTR), and Vulkan Video
//! (app-owned DPB slot table).
//!
//! Policy only. Callers feed **currently-trusted** `(slot, wire)` pairs and apply
//! the returned taints through their own persistent marker. How a force is applied
//! and how distrust is stored (AMF clears the mirror slot, QSV sets `ltr_tainted`,
//! Vulkan blanks `slot_wire` to `-1`) stays in the backend; the caller-side filter
//! is what makes those schemes equivalent under one pure function.
//!
//! Decline is also the backend's: AMF/QSV drop an un-consumed `pending_force`;
//! Vulkan leaves `pending_loss` armed so frame-build can re-pick or force an IDR.
//! Do not harmonize them here. NVENC's range policy is
//! [`super::nvenc_core::plan_range_recovery`].

pub struct SlotPlan {
    /// Slots with `wire >= loss_first`. Persist the distrust in the backend marker:
    /// without it, the next loss treats these as pre-loss anchors.
    pub tainted: u32,
    /// Newest trusted `(slot, wire)` strictly older than the loss. `None` → the
    /// caller declines and recovers via its keyframe path.
    pub anchor: Option<(usize, i64)>,
}

/// Taint and pick from one snapshot of currently-trusted `(slot, wire)` pairs
/// (caller already dropped previously-distrusted entries). `wire >= loss_first`
/// taints; `wire < loss_first` is the only eligible anchor, so this call cannot
/// pick a slot it just tainted.
pub fn plan_slot_recovery(refs: &[(usize, i64)], loss_first: i64) -> SlotPlan {
    // Callers gate `first < 0` before they get here; `-1`/`None` sentinels are
    // "untrusted". Plain `assert`: `--release` lint runs, and a compiled-out
    // check would drop taints instead of failing.
    assert!(
        loss_first >= 0,
        "loss_first must be validity-gated by the caller"
    );
    let mut tainted = 0u32;
    for &(slot, wire) in refs {
        if wire >= loss_first {
            assert!(slot < 32, "slot table exceeds the u32 taint mask");
            tainted |= 1 << slot;
        }
    }
    SlotPlan {
        tainted,
        anchor: pick_anchor(refs, loss_first),
    }
}

/// Newest trusted `wire` strictly older than the loss. Ties keep the first
/// `refs` entry (callers feed ascending slot order; the backends used `>`).
/// Vulkan re-picks at frame-build against the table as it stands then.
pub fn pick_anchor(refs: &[(usize, i64)], loss_first: i64) -> Option<(usize, i64)> {
    let mut best: Option<(usize, i64)> = None;
    for &(slot, wire) in refs {
        if wire < loss_first && best.is_none_or(|(_, b)| wire > b) {
            best = Some((slot, wire));
        }
    }
    best
}

/// An on-demand intra refresh wave in flight, the rung between an RFI anchor and the IDR:
/// where no anchor survives, the picture heals over `cycle` frames with no bitrate spike.
/// `index` is the frame about to be encoded. The start AU and the close AU carry
/// `recovery_point`, which the client counts as its two-mark lift; every wave picture but the
/// close is part dirty and never an RFI anchor. The backend places the stripe (Vulkan, VAAPI)
/// or the driver does (NVENC); the bookkeeping here is the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wave {
    pub cycle: u32,
    pub index: u32,
}

impl Wave {
    pub fn start(cycle: u32) -> Wave {
        Wave { cycle, index: 0 }
    }

    /// This frame closes the wave: the picture is fully swept once it is encoded.
    pub fn closes(self) -> bool {
        self.index + 1 >= self.cycle
    }

    /// This frame's AU carries `recovery_point`: the start and the close.
    pub fn marks(self) -> bool {
        self.index == 0 || self.closes()
    }

    /// Past the frame just encoded; `None` once the wave closed.
    pub fn next(self) -> Option<Wave> {
        (!self.closes()).then_some(Wave {
            cycle: self.cycle,
            index: self.index + 1,
        })
    }

    /// Row-based stripe for this frame: `(first_row, rows)`, one region per frame plus one
    /// row of overlap for the deblocking filter, clipped to the picture. `rows` is the
    /// picture height in the driver's row unit.
    pub fn stripe(self, rows: u32) -> (u32, u32) {
        let region = rows.div_ceil(self.cycle.max(1));
        let first = (region * self.index).min(rows);
        (first, (region + 1).min(rows - first))
    }
}

/// Frames per wave: a quarter second at most (the freeze the client holds during the wave),
/// one row per frame at most, the driver's ceiling, never below 2, and then the number of
/// whole regions the rows make at that size: a cycle that does not divide the rows would
/// refresh nothing on its last indices and close late. `pinned` is [`pinned_cycle`].
pub fn wave_cycle(rows: u32, fps: u32, max_cycle: u32, pinned: Option<u32>) -> u32 {
    let rows = rows.max(2);
    let wanted = pinned
        .unwrap_or((fps / 4).max(2))
        .min(rows)
        .min(max_cycle)
        .max(2);
    rows.div_ceil(rows.div_ceil(wanted)).max(2)
}

/// `PUNKTFUNK_INTRA_REFRESH=0` keeps the IDR on every backend. The same knob at `1` opts the
/// Windows periodic wave in (`policy::intra_refresh_requested`) and NVENC's on-demand wave
/// ([`nvenc_wave_enabled`]); unset is the VCN wave alone.
pub fn wave_enabled() -> bool {
    crate::knobs::get().intra_refresh != 2
}

/// NVENC answers a declined RFI with the IDR unless `PUNKTFUNK_INTRA_REFRESH=1` opts its wave
/// in for a measurement. A client lifts on the wave's marks for good and forgets every damage
/// mark, so only a sweep that decodes bit-exact may mark. NVENC's does not: an invalidate call
/// mid-sweep stops it while the host still marks the close, and its bands bleed a few rows
/// under motion. Both leave a grey smear that only the next IDR clears.
pub fn nvenc_wave_enabled() -> bool {
    nvenc_wave_opted_in(crate::knobs::get().intra_refresh)
}

/// `1` alone waves; unset (`0`) and off (`2`) both keep the IDR.
fn nvenc_wave_opted_in(intra_refresh: u8) -> bool {
    intra_refresh == 1
}

/// `PUNKTFUNK_IR_PERIOD_FRAMES=<frames>` pins the cycle for a measurement: the one wave-length
/// knob, shared with the periodic wave's length (`policy::intra_refresh_period`).
pub fn pinned_cycle() -> Option<u32> {
    match crate::knobs::get().ir_period_frames {
        n if n >= 2 => Some(u32::from(n)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{nvenc_wave_opted_in, pick_anchor, plan_slot_recovery, wave_cycle, Wave};

    /// The NVENC wave is opt-in: the default knob keeps the IDR, `1` waves, off never does.
    #[test]
    fn nvenc_waves_only_when_opted_in() {
        assert!(!nvenc_wave_opted_in(0), "unset: IDR");
        assert!(nvenc_wave_opted_in(1));
        assert!(!nvenc_wave_opted_in(2), "PUNKTFUNK_INTRA_REFRESH=0: IDR");
    }

    /// Rows cap the cycle (1080p = 17 CTB rows), a quarter second caps it at 60 fps, the
    /// driver ceiling and the pin override, never below 2.
    #[test]
    fn wave_cycle_caps() {
        // 17 rows in two-row regions: 9 frames, not the 15 a quarter second would allow.
        assert_eq!(wave_cycle(17, 60, 256, None), 9);
        assert_eq!(wave_cycle(17, 120, 256, None), 17);
        assert_eq!(wave_cycle(34, 120, 256, None), 17);
        assert_eq!(wave_cycle(17, 60, 8, None), 6);
        assert_eq!(wave_cycle(17, 60, 256, Some(4)), 4);
        assert_eq!(wave_cycle(17, 60, 256, Some(0)), 2);
        assert_eq!(wave_cycle(1, 60, 256, None), 2);
        assert_eq!(wave_cycle(4, 60, 256, None), 4);
        assert_eq!(wave_cycle(16, 60, 256, None), 8);
    }

    /// Marks on the start and the close only, dirty until the close, stripes that walk the
    /// picture with one overlap row and never past its end.
    #[test]
    fn wave_marks_and_stripes() {
        let mut w = Some(Wave::start(4));
        let mut seen = Vec::new();
        while let Some(wave) = w {
            seen.push((wave.index, wave.marks(), wave.closes(), wave.stripe(4)));
            w = wave.next();
        }
        assert_eq!(
            seen,
            [
                (0, true, false, (0, 2)),
                (1, false, false, (1, 2)),
                (2, false, false, (2, 2)),
                (3, true, true, (3, 1)),
            ]
        );
        // 17 rows over 9 frames: two rows per region plus the overlap, the last one clipped.
        assert_eq!(Wave { cycle: 9, index: 0 }.stripe(17), (0, 3));
        assert_eq!(Wave { cycle: 9, index: 7 }.stripe(17), (14, 3));
        assert_eq!(Wave { cycle: 9, index: 8 }.stripe(17), (16, 1));
        // A two-frame wave both starts and closes within two frames.
        assert!(Wave::start(2).marks() && !Wave::start(2).closes());
        assert!(Wave::start(2).next().unwrap().closes());
    }

    fn view(wires: &[i64]) -> Vec<(usize, i64)> {
        wires
            .iter()
            .enumerate()
            .filter_map(|(s, &w)| (w >= 0).then_some((s, w)))
            .collect()
    }

    fn apply(wires: &mut [i64], tainted: u32) {
        for (s, w) in wires.iter_mut().enumerate() {
            if tainted & (1 << s) != 0 {
                *w = -1;
            }
        }
    }

    #[test]
    fn picks_newest_pre_loss() {
        let wires = [8i64, 9, 10, 11, 12, 5, 6, 7];
        assert_eq!(pick_anchor(&view(&wires), 9), Some((0, 8)));
        assert_eq!(pick_anchor(&view(&wires), 5), None);
        assert_eq!(pick_anchor(&view(&[-1, 3, -1, 4]), 5), Some((3, 4)));
        assert_eq!(pick_anchor(&view(&[-1; 8]), 5), None);
        // `wire == loss_first` is inside the corrupt window: strictly older only.
        assert_eq!(pick_anchor(&view(&[9, 8]), 9), Some((1, 8)));
        // Tie keeps the first `refs` entry — the backends used `>`, not `>=`.
        assert_eq!(pick_anchor(&[(2, 7), (5, 7)], 9), Some((2, 7)));
        assert_eq!(pick_anchor(&[], 9), None);
    }

    /// A slot from an earlier unrepaired loss must not become a later loss's
    /// "known-good" anchor: without persisted distrust it is still resident and
    /// below the second start, so the picker would serve it as `recovery_anchor`.
    #[test]
    fn taint_sweep_excludes_slots_from_an_earlier_loss() {
        // Loss at 4 taints 4..7; a second report at 6 still sees them resident.
        let tainted_wires = [4i64, 5, 6, 7];

        let unswept = [0i64, 1, 2, 3, 4, 5, 6, 7];
        let (_, picked_wire) = pick_anchor(&view(&unswept), 6).expect("unswept picks something");
        assert!(
            tainted_wires.contains(&picked_wire),
            "precondition: without the sweep the anchor comes from the earlier loss window"
        );

        let mut wires = unswept;
        let plan = plan_slot_recovery(&view(&wires), 4);
        assert_eq!(plan.tainted, 0b1111_0000);
        assert_eq!(plan.anchor, Some((3, 3)));
        apply(&mut wires, plan.tainted);
        assert_eq!(wires, [0, 1, 2, 3, -1, -1, -1, -1]);
        let (slot, wire) = pick_anchor(&view(&wires), 6).expect("clean wires remain");
        assert_eq!((slot, wire), (3, 3), "newest clean survivor is wire 3");

        // Post-recovery refill: a later loss at 10 may anchor on 9; do not over-taint.
        wires[4] = 8;
        wires[5] = 9;
        wires[6] = 10;
        wires[7] = 11;
        let plan = plan_slot_recovery(&view(&wires), 10);
        assert_eq!(plan.anchor, Some((5, 9)), "wire 9 is post-recovery, clean");
        apply(&mut wires, plan.tainted);

        let mut all = [5i64, 6, 7, 8, 9, 10, 11, 12];
        let plan = plan_slot_recovery(&view(&all), 5);
        assert_eq!(plan.tainted, 0b1111_1111);
        assert_eq!(plan.anchor, None);
        apply(&mut all, plan.tainted);
        assert_eq!(pick_anchor(&view(&all), 5), None);
    }

    /// Wholesale withdrawal (`Encoder::distrust_references`) has no loss range,
    /// so every resident ref is dropped. The next pick must decline rather than
    /// serve an anchor over unrepaired damage.
    #[test]
    fn distrusting_every_reference_makes_the_next_anchor_pick_decline() {
        let mut wires = [4i64, 5, 6, 7, -1, -1, -1, -1];
        assert_eq!(
            pick_anchor(&view(&wires), 9),
            Some((3, 7)),
            "precondition: this table would happily anchor"
        );

        apply(&mut wires, u32::MAX);
        assert_eq!(
            pick_anchor(&view(&wires), 9),
            None,
            "every reference withdrawn → no anchor, caller falls through to its keyframe path"
        );
        // Persisted: any later loss, not only this one, still finds nothing.
        assert_eq!(pick_anchor(&view(&wires), 100), None);
    }

    /// Withdrawal is per-slot, not the session: a slot re-marked with a fresh
    /// frame (after the IDR flush that emptied the table) is a legal anchor again.
    #[test]
    fn a_re_marked_slot_restores_anchor_trust_after_a_full_withdrawal() {
        let mut wires = [4i64, 5, 6, 7, -1, -1, -1, -1];
        apply(&mut wires, u32::MAX);
        assert_eq!(pick_anchor(&view(&wires), 20), None);

        wires[0] = 14;
        wires[1] = 15;
        assert_eq!(
            pick_anchor(&view(&wires), 20),
            Some((1, 15)),
            "a re-marked slot is trusted again — the suppression is a few frames, not the session"
        );
    }

    /// A report never arrives at the loss: the client waits for the next frame to
    /// spot the gap, and the ask crosses the link. The ring keeps rolling. A ring
    /// shallower than that latency has overwritten every pre-loss picture by the
    /// time the ask lands, so it declines every loss and the session pays an IDR.
    #[test]
    fn the_ring_must_outlast_the_loss_report() {
        // Slot `w % depth` holds wire `w`, the newest `depth` frames.
        let ring = |depth: usize, newest: i64| -> Vec<(usize, i64)> {
            (0..depth)
                .map(|back| {
                    let w = newest - back as i64;
                    ((w as usize) % depth, w)
                })
                .collect()
        };
        // 1440p100 over Wi-Fi: frames 10213 and 10214 are lost, the client sees the
        // gap at 10215, and the ask reaches the encoder around 10217.
        let (loss, when_asked) = (10_213i64, 10_217i64);
        assert_eq!(
            pick_anchor(&ring(4, when_asked), loss),
            None,
            "four slots hold 10214..10217 — the anchor is already evicted"
        );
        assert_eq!(
            pick_anchor(&ring(8, when_asked), loss),
            Some(((10_212usize) % 8, 10_212)),
            "eight slots still hold the picture before the loss"
        );
    }
}
