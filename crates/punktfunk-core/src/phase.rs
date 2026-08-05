//! Circular (directional) statistics for phase-locked capture (design/phase-locked-capture.md):
//! the client-side half of the controller's v2 error signal, plus the panel-grid learner every
//! vsync-aware presenter paces against. Pure math, no features — shared so every presenter
//! (Android today, iOS and the desktop session client next) computes the SAME statistic the host
//! controller was tuned against, and so the controller's simulation tests can generate their
//! synthetic reports through the identical code path.

/// Plausible panel periods: ~24 Hz to ~500 Hz. A spacing outside this is a clock glitch, not a
/// display mode, and must never reach the estimate.
const PANEL_PERIOD_RANGE_NS: std::ops::RangeInclusive<i64> = 2_000_000..=42_000_000;

/// Spacings within this of the estimate are the same grid — absorbs ordinary timeline jitter.
const PANEL_GRID_TOLERANCE_NS: i64 = 200_000;

/// Consecutive WIDER observations required before the estimate grows. One stray wide sample is a
/// scheduling hiccup; eight in a row (~66 ms at 120 Hz) is a display that really did slow down.
const PANEL_WIDEN_STREAK: u8 = 8;

/// The panel's true refresh period, learned from observed vsync/frame-timeline spacing.
///
/// A presenter subdivides its release targets onto this grid, so an estimate FINER than the panel
/// makes it aim at instants that never arrive and release faster than the display consumes —
/// which is why the estimate has to be able to move both ways.
///
/// Seeding is the reason this is not simply "believe the last sample". The platform's *configured*
/// mode is not the panel: under a per-uid frame-rate override a 120 Hz panel reports 60
/// (`Display.getRefreshRate` returns the override — observed on-glass, A024), and the app's own
/// choreographer callbacks arrive at the down-rated rate while the panel scans at its own. The
/// mode TABLE is honest about what the panel *can* do, so it is the seed; the timeline spacing is
/// honest about what it is *doing*, so it is the correction.
///
/// The asymmetry is deliberate. **Narrowing is immediate**: a finer real grid is always safe to
/// subdivide onto, and it is the down-rate case the seed most often gets wrong. **Widening needs
/// [`PANEL_WIDEN_STREAK`] consecutive agreeing observations** and then adopts the *narrowest* of
/// them, because a wide sample is far more likely to be a missed callback than a mode change.
///
/// ⚠ 0.23.0 shipped this learner as narrow-only, seeded from the display mode the app *requests*
/// (`preferredDisplayModeId` is a hint the system may refuse). A refused 120 Hz switch therefore
/// left the presenter pacing a 60 Hz panel on an 8.33 ms grid with no way back — permanently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PanelGrid {
    period_ns: i64,
    widen_streak: u8,
    /// Narrowest wider-than-estimate spacing seen during the current streak.
    widen_candidate: i64,
}

impl PanelGrid {
    /// Seed from the display mode's refresh rate (`0` = unknown — the first plausible observation
    /// then sets the estimate outright).
    pub fn seeded(hz: i32) -> PanelGrid {
        PanelGrid {
            period_ns: if hz > 0 { 1_000_000_000 / hz as i64 } else { 0 },
            widen_streak: 0,
            widen_candidate: 0,
        }
    }

    /// The learned period, or `0` while unknown.
    pub fn period_ns(&self) -> i64 {
        self.period_ns
    }

    /// Fold one observed grid spacing. Returns `true` when [`period_ns`](Self::period_ns) changed.
    pub fn observe(&mut self, spacing_ns: i64) -> bool {
        if !PANEL_PERIOD_RANGE_NS.contains(&spacing_ns) {
            return false; // implausible — a clock glitch, not a display mode
        }
        if self.period_ns == 0 {
            self.reset_streak();
            self.period_ns = spacing_ns;
            return true;
        }
        if spacing_ns < self.period_ns - PANEL_GRID_TOLERANCE_NS {
            self.reset_streak();
            self.period_ns = spacing_ns;
            return true;
        }
        if spacing_ns > self.period_ns + PANEL_GRID_TOLERANCE_NS {
            self.widen_streak = self.widen_streak.saturating_add(1);
            self.widen_candidate = if self.widen_candidate == 0 {
                spacing_ns
            } else {
                self.widen_candidate.min(spacing_ns)
            };
            if self.widen_streak >= PANEL_WIDEN_STREAK {
                self.period_ns = self.widen_candidate;
                self.reset_streak();
                return true;
            }
            return false;
        }
        self.reset_streak(); // this sample agreed — the run of wider ones is broken
        false
    }

    fn reset_streak(&mut self) {
        self.widen_streak = 0;
        self.widen_candidate = 0;
    }
}

/// Circular (vector-mean) statistics of latch samples against a display period: the mean latch
/// mod the period (ns) and the coherence (‰).
///
/// The mean is what a phase controller can actually steer under jitter — the MEDIAN of a
/// period-spanning distribution is immovable (shifting a uniform-mod-P distribution's mean
/// leaves its median untouched; the controller-v1 on-glass lesson, 2026-07-31). The coherence
/// (the resultant length `R` of the unit phasors, scaled to ‰) says whether ANY phase exists to
/// steer: 0 = arrivals uniformly smeared over the period (alignment is physically pointless),
/// 1000 = perfectly phase-locked.
///
/// `None` under 8 samples or a non-positive period — too little evidence to report a phase.
pub fn circular_latch(samples_us: &[u64], period_ns: i64) -> Option<(u64, u16)> {
    if samples_us.len() < 8 || period_ns <= 0 {
        return None;
    }
    let period_us = period_ns as f64 / 1000.0;
    let (mut x, mut y) = (0.0f64, 0.0f64);
    for &s in samples_us {
        let theta = (s as f64 % period_us) / period_us * std::f64::consts::TAU;
        x += theta.cos();
        y += theta.sin();
    }
    let n = samples_us.len() as f64;
    let r = (x * x + y * y).sqrt() / n;
    let mean_theta = y.atan2(x).rem_euclid(std::f64::consts::TAU);
    let mean_ns = (mean_theta / std::f64::consts::TAU * period_ns as f64) as u64;
    Some((mean_ns, (r * 1000.0) as u16))
}

/// Largest present spacing still treated as cadence. Anything wider is a stall (a stream pause,
/// an occluded window, a codec rebuild) and is counted separately: folding a 5-second gap in as
/// "one irregular interval" would be true but useless, and folding it in as several would make a
/// single hitch dominate the window.
const CADENCE_MAX_UNITS: usize = 8;

/// Minimum intervals before a cadence summary means anything — same evidence bar as
/// [`circular_latch`]. At any sane frame rate a 1 s window clears this many times over; it is
/// there so a window truncated by a reanchor does not publish a judder figure off three samples.
const CADENCE_MIN_SAMPLES: u32 = 8;

/// One window's present-cadence summary (see [`PresentIntervals`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentCadence {
    /// The most common spacing, in whole panel refreshes. This is the stream's cadence ratio:
    /// 1 when stream rate matches the panel, 2 for 60-on-120, 4 for 30-on-120.
    pub mode_units: u8,
    /// Fraction of intervals that were NOT the mode, in ‰ (same unit as the phase coherence).
    /// **This is the judder number.** 0 = a perfectly regular cadence at any ratio.
    pub judder_permille: u16,
    /// Intervals folded into the histogram (excludes stalls and disordered samples).
    pub samples: u32,
    /// Spacings wider than [`CADENCE_MAX_UNITS`] — stalls, not judder. Reported so a window that
    /// looks smooth *because the stream was paused* cannot be mistaken for a good one.
    pub stalls: u32,
    /// Present instants that did not advance (duplicate or out-of-order callbacks). A platform
    /// bookkeeping signal, not a display defect — kept out of the judder ratio deliberately.
    pub disordered: u32,
}

/// Present-interval distribution in whole panel refreshes — the cadence (judder) statistic.
///
/// Every other stat we publish is a latency: a difference between two points on one frame. No
/// latency can see judder, because judder is a property of the *sequence*. A stream that shows
/// each frame one refresh early and the next one late has excellent percentiles and looks
/// broken; a stream whose every interval is exactly two refreshes has worse latency than one
/// that alternates 1 and 3, and looks perfect. Quantising the spacing between consecutive
/// on-glass instants onto the panel grid measures the thing the eye actually reacts to.
///
/// Scale-free by construction: it needs no reference clock, and the *mode* absorbs the cadence
/// ratio, so 60-on-120 and 120-on-120 are both "smooth = one tall bucket" and comparable to each
/// other. That is what makes it usable as one ruler across clients, refresh rates and stream
/// rates — including for a feature-on/feature-off A/B on the same device.
///
/// Feed it the **measured on-glass instant**, never the instant a present was *requested*:
/// requested times would measure our own intent and report a perfect cadence no matter what the
/// display did with it. Every client has the real one (Android's `OnFrameRendered` system time,
/// the desktop's `VK_KHR_present_wait` stamp, Apple's drawable `presentedTime`).
///
/// Pure state and arithmetic — no clock, no allocation. The caller owns the window: drain with
/// [`take`](Self::take) on its own 1 s tumbling boundary, per `design/stats-unification.md`.
#[derive(Debug, Clone, Default)]
pub struct PresentIntervals {
    last_present_ns: i64,
    /// Counts indexed by whole refreshes, `0..=CADENCE_MAX_UNITS`.
    hist: [u32; CADENCE_MAX_UNITS + 1],
    samples: u32,
    stalls: u32,
    disordered: u32,
}

impl PresentIntervals {
    pub fn new() -> PresentIntervals {
        PresentIntervals::default()
    }

    /// Forget the previous instant without discarding the window's counts. Call on any
    /// discontinuity where the next present is not a continuation of this cadence (reanchor,
    /// codec rebuild, surface recreate) so the gap across it is not scored as a stall.
    pub fn split(&mut self) {
        self.last_present_ns = 0;
    }

    /// Fold one on-glass instant. `period_ns` is the learned panel period
    /// ([`PanelGrid::period_ns`]); a non-positive one means the grid is not known yet and the
    /// sample is held as the new predecessor without being scored.
    pub fn record(&mut self, present_ns: i64, period_ns: i64) {
        let prev = std::mem::replace(&mut self.last_present_ns, present_ns);
        if prev <= 0 || period_ns <= 0 {
            return; // first sample of a run, or no grid to quantise against
        }
        let spacing = present_ns - prev;
        if spacing <= 0 {
            // A repeated or out-of-order callback. Keep the LATER instant as the predecessor so
            // one disordered delivery cannot corrupt every following spacing.
            self.disordered += 1;
            self.last_present_ns = prev.max(present_ns);
            return;
        }
        // Round to the nearest whole refresh: a present is "on the grid" if it is closer to this
        // vblank than the next, which is exactly what the display did with it.
        let units = (spacing * 2 + period_ns) / (period_ns * 2);
        if units as usize > CADENCE_MAX_UNITS {
            self.stalls += 1;
            return;
        }
        self.hist[units as usize] += 1;
        self.samples += 1;
    }

    /// This window's summary, or `None` under [`CADENCE_MIN_SAMPLES`].
    pub fn summary(&self) -> Option<PresentCadence> {
        if self.samples < CADENCE_MIN_SAMPLES {
            return None;
        }
        let (mode_units, mode_count) = self
            .hist
            .iter()
            .enumerate()
            .max_by_key(|&(_, c)| *c)
            .map(|(i, &c)| (i as u8, c))?;
        Some(PresentCadence {
            mode_units,
            judder_permille: (u64::from(self.samples - mode_count) * 1000 / u64::from(self.samples))
                as u16,
            samples: self.samples,
            stalls: self.stalls,
            disordered: self.disordered,
        })
    }

    /// Drain the window: the summary (if it clears the evidence bar) and a reset of the counts.
    /// The previous instant SURVIVES the drain — the cadence continues across a window boundary,
    /// and dropping it would manufacture one unscored interval per window.
    pub fn take(&mut self) -> Option<PresentCadence> {
        let out = self.summary();
        self.hist = [0; CADENCE_MAX_UNITS + 1];
        self.samples = 0;
        self.stalls = 0;
        self.disordered = 0;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: i64 = 8_333_333; // 120 Hz in ns
    const P_US: u64 = 8_333; // …and in µs, the sample unit

    #[test]
    fn identical_samples_are_fully_coherent() {
        let (mean, coh) = circular_latch(&[4_000; 16], P).unwrap();
        assert!(coh >= 995, "identical phases must read ~1000‰, got {coh}");
        assert!(
            (mean as i64 - 4_000_000).abs() < 20_000,
            "mean {mean} ≉ 4.0 ms"
        );
    }

    #[test]
    fn uniform_grid_over_the_period_is_incoherent() {
        // 16 samples evenly spanning one period — the resultant vector cancels.
        let samples: Vec<u64> = (0..16).map(|i| i * P_US / 16).collect();
        let (_, coh) = circular_latch(&samples, P).unwrap();
        assert!(coh < 100, "a uniform phase smear must read ~0‰, got {coh}");
    }

    #[test]
    fn cluster_straddling_the_wrap_averages_at_the_boundary() {
        // Half the samples just below the period boundary, half just above 0: an ARITHMETIC
        // mean would report ~P/2 (maximally wrong); the circular mean must sit at the boundary.
        let samples = [
            P_US - 200,
            P_US - 100,
            P_US - 50,
            P_US - 150,
            100,
            50,
            150,
            200,
        ];
        let (mean, coh) = circular_latch(&samples, P).unwrap();
        let dist_to_boundary = (mean as i64).min((P - mean as i64).abs());
        assert!(
            dist_to_boundary < 500_000,
            "circular mean {mean} must hug the wrap boundary"
        );
        assert!(
            coh > 900,
            "a tight straddling cluster is still coherent, got {coh}"
        );
    }

    #[test]
    fn too_few_samples_report_nothing() {
        assert!(circular_latch(&[1_000; 7], P).is_none());
        assert!(circular_latch(&[1_000; 16], 0).is_none());
    }
}

#[cfg(test)]
mod panel_grid_tests {
    use super::*;

    const P120: i64 = 8_333_333;
    const P60: i64 = 16_666_666;

    #[test]
    fn seeds_from_the_mode_and_reports_unknown_without_one() {
        assert_eq!(PanelGrid::seeded(120).period_ns(), 8_333_333);
        assert_eq!(PanelGrid::seeded(0).period_ns(), 0);
        let mut g = PanelGrid::seeded(0);
        assert!(
            g.observe(P120),
            "the first plausible sample sets an unseeded grid"
        );
        assert_eq!(g.period_ns(), P120);
    }

    #[test]
    fn narrows_immediately_when_the_panel_is_faster_than_the_mode_said() {
        // The down-rate case: the mode table read 60, the timelines run at 120.
        let mut g = PanelGrid::seeded(60);
        assert!(g.observe(P120));
        assert_eq!(g.period_ns(), P120, "a finer real grid is adopted at once");
    }

    /// The 0.23.0 bug: `preferredDisplayModeId` is a request, so a refused 120 Hz switch seeds a
    /// 120 Hz grid on a panel that is really running 60. The narrow-only learner could never
    /// climb back, and the presenter aimed at instants the panel never reached.
    #[test]
    fn widens_back_out_when_the_requested_mode_was_refused() {
        let mut g = PanelGrid::seeded(120);
        for i in 0..PANEL_WIDEN_STREAK - 1 {
            assert!(!g.observe(P60), "sample {i} must not widen on its own");
            assert_eq!(g.period_ns(), P120);
        }
        assert!(
            g.observe(P60),
            "a sustained run of wider spacings widens the grid"
        );
        assert_eq!(g.period_ns(), P60);
    }

    #[test]
    fn one_stray_wide_sample_never_widens() {
        let mut g = PanelGrid::seeded(120);
        for _ in 0..40 {
            assert!(!g.observe(P60));
            assert!(!g.observe(P120)); // an agreeing sample breaks the run
        }
        assert_eq!(
            g.period_ns(),
            P120,
            "alternating samples must not accumulate"
        );
    }

    #[test]
    fn widening_adopts_the_narrowest_of_the_run() {
        let mut g = PanelGrid::seeded(120);
        // A run of wide spacings that includes some very wide outliers.
        let run = [
            P60,
            33_000_000,
            P60 + 400_000,
            41_000_000,
            P60,
            P60,
            P60,
            P60,
        ];
        for s in run {
            g.observe(s);
        }
        assert_eq!(
            g.period_ns(),
            P60,
            "the estimate takes the narrowest of the run, never an outlier"
        );
    }

    #[test]
    fn implausible_spacings_are_ignored_entirely() {
        let mut g = PanelGrid::seeded(120);
        for _ in 0..100 {
            assert!(!g.observe(0));
            assert!(!g.observe(-1));
            assert!(!g.observe(1_000_000)); // 1000 Hz — below the range floor
            assert!(!g.observe(100_000_000)); // 10 Hz — above the ceiling
        }
        assert_eq!(g.period_ns(), P120);
    }

    #[test]
    fn a_transient_narrow_glitch_self_heals() {
        // Narrowing is immediate, so a glitch DOES poison the estimate — the point is that it is
        // no longer permanent (0.23.0's learner had no way back).
        let mut g = PanelGrid::seeded(120);
        assert!(g.observe(2_100_000), "a glitch narrows the estimate");
        assert_eq!(g.period_ns(), 2_100_000);
        for _ in 0..PANEL_WIDEN_STREAK {
            g.observe(P120);
        }
        assert_eq!(g.period_ns(), P120, "and the real grid wins it back");
    }
}

#[cfg(test)]
mod cadence_tests {
    use super::*;

    const P: i64 = 8_333_333; // 120 Hz in ns

    /// Fold `n` presents spaced by `spacings` in rotation, starting at an arbitrary instant.
    fn cadence(spacings: &[i64], n: usize) -> PresentIntervals {
        let mut pi = PresentIntervals::new();
        let mut t = 1_000_000_000i64;
        pi.record(t, P);
        for i in 0..n {
            t += spacings[i % spacings.len()];
            pi.record(t, P);
        }
        pi
    }

    #[test]
    fn a_regular_cadence_has_no_judder() {
        let s = cadence(&[P], 60).summary().unwrap();
        assert_eq!((s.mode_units, s.judder_permille), (1, 0));
        assert_eq!(s.samples, 60);
    }

    /// The property that makes this one ruler across rates: a stream at half the panel rate is
    /// SMOOTH, not judder — the mode absorbs the cadence ratio.
    fn ratio_is_absorbed_not_penalised(mult: i64, expect_units: u8) {
        let s = cadence(&[P * mult], 40).summary().unwrap();
        assert_eq!((s.mode_units, s.judder_permille), (expect_units, 0));
    }

    #[test]
    fn sixty_on_onetwenty_reads_smooth() {
        ratio_is_absorbed_not_penalised(2, 2); // 60 fps on a 120 Hz panel
        ratio_is_absorbed_not_penalised(4, 4); // 30 fps on a 120 Hz panel
    }

    /// D3's signature: the same mean spacing as `sixty_on_onetwenty_reads_smooth`, delivered as
    /// alternating 1 and 3 refreshes. Identical average frame rate, identical latency
    /// percentiles — and this is the one that looks broken.
    #[test]
    fn the_sawtooth_that_latency_stats_cannot_see() {
        let s = cadence(&[P, P * 3], 40).summary().unwrap();
        assert_eq!(s.judder_permille, 500);
        assert!(matches!(s.mode_units, 1 | 3));
    }

    /// Sub-refresh jitter is not judder: the display quantises it away, so the metric must too.
    /// Only a spacing that crosses the half-refresh boundary changes which vblank was used.
    #[test]
    fn jitter_inside_a_refresh_is_not_judder() {
        let s = cadence(&[P + P * 2 / 5, P - P * 2 / 5], 40)
            .summary()
            .unwrap();
        assert_eq!((s.mode_units, s.judder_permille), (1, 0));
    }

    #[test]
    fn a_stall_is_counted_apart_from_judder() {
        let mut pi = PresentIntervals::new();
        let mut t = 1_000_000_000i64;
        pi.record(t, P);
        for _ in 0..20 {
            t += P;
            pi.record(t, P);
        }
        t += P * 400; // a pause, not a pacing defect
        pi.record(t, P);
        let s = pi.summary().unwrap();
        assert_eq!((s.judder_permille, s.stalls, s.samples), (0, 1, 20));
    }

    #[test]
    fn out_of_order_callbacks_do_not_corrupt_the_run() {
        let mut pi = PresentIntervals::new();
        let mut t = 1_000_000_000i64;
        pi.record(t, P);
        for _ in 0..10 {
            t += P;
            pi.record(t, P);
        }
        pi.record(t - P * 3, P); // a late/duplicate delivery
        for _ in 0..10 {
            t += P;
            pi.record(t, P);
        }
        let s = pi.summary().unwrap();
        assert_eq!(s.disordered, 1);
        assert_eq!(
            s.judder_permille, 0,
            "keeping the later instant means the following spacings stay on the grid"
        );
    }

    #[test]
    fn an_unknown_grid_scores_nothing() {
        let s = cadence(&[P], 60);
        let mut pi = PresentIntervals::new();
        let mut t = 1_000_000_000i64;
        for _ in 0..60 {
            t += P;
            pi.record(t, 0); // PanelGrid has not learned a period yet
        }
        assert!(pi.summary().is_none());
        assert!(s.summary().is_some(), "control");
    }

    #[test]
    fn a_short_window_publishes_nothing() {
        assert!(cadence(&[P], 5).summary().is_none());
    }

    /// The cadence continues across a window boundary — dropping the predecessor on drain would
    /// silently discard one interval per window, every window.
    #[test]
    fn take_resets_the_counts_but_not_the_cadence() {
        let mut pi = cadence(&[P], 20);
        assert!(pi.take().is_some());
        assert!(pi.summary().is_none(), "counts cleared");
        let mut t = 1_000_000_000 + P * 20;
        for _ in 0..10 {
            t += P;
            pi.record(t, P);
        }
        let s = pi.summary().unwrap();
        assert_eq!(
            s.samples, 10,
            "the first post-drain present scored against the pre-drain one"
        );
    }

    #[test]
    fn split_forgets_the_predecessor() {
        let mut pi = cadence(&[P], 20);
        pi.take();
        pi.split();
        let mut t = 5_000_000_000i64; // a reanchor: the gap across it is meaningless
        for _ in 0..10 {
            t += P;
            pi.record(t, P);
        }
        let s = pi.summary().unwrap();
        assert_eq!(
            (s.samples, s.stalls),
            (9, 0),
            "the gap was not scored at all"
        );
    }
}
