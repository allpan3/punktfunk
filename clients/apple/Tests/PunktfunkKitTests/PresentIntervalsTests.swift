// Parity tests for the Swift `PresentIntervals` port (Video/Stage2Pipeline.swift) against
// `punktfunk_core::phase::PresentIntervals` — the cadence (judder) statistic of
// design/presenter-cadence-rework.md WP1.
//
// These are deliberately the SAME cases and the SAME vectors as the Rust unit tests in
// crates/punktfunk-core/src/phase.rs (module `cadence_tests`). WP1's acceptance criterion is that
// all three clients emit the same numbers for the same synthetic input, and a hand-written port is
// exactly where that quietly stops being true — so the port is pinned here rather than trusted.
//
// If you change one side, change both, and keep the vectors identical.

import Foundation
import XCTest

@testable import PunktfunkKit

final class PresentIntervalsTests: XCTestCase {
    /// 120 Hz in ns — the Rust tests' `P`.
    private static let P: Int64 = 8_333_333

    /// Fold `n` presents spaced by `spacings` in rotation, starting at an arbitrary instant.
    /// Mirrors the Rust helper of the same shape.
    private func cadence(_ spacings: [Int64], _ n: Int, period: Int64 = P) -> PresentIntervals {
        var pi = PresentIntervals()
        var t: Int64 = 1_000_000_000
        pi.record(presentNs: t, periodNs: period)
        for i in 0..<n {
            t += spacings[i % spacings.count]
            pi.record(presentNs: t, periodNs: period)
        }
        return pi
    }

    func testARegularCadenceHasNoJudder() {
        let s = cadence([Self.P], 60).summary()
        XCTAssertEqual(s?.modeUnits, 1)
        XCTAssertEqual(s?.judderPermille, 0)
        XCTAssertEqual(s?.samples, 60)
    }

    /// The property that makes this one ruler across rates: a stream at half (or a quarter of)
    /// the panel rate is SMOOTH, not judder — the mode absorbs the cadence ratio.
    func testSixtyOnOneTwentyReadsSmooth() {
        for (mult, expected) in [(Int64(2), 2), (Int64(4), 4)] {
            let s = cadence([Self.P * mult], 40).summary()
            XCTAssertEqual(s?.modeUnits, expected)
            XCTAssertEqual(s?.judderPermille, 0)
        }
    }

    /// D3's signature: the same mean spacing as a steady 2, delivered as alternating 1 and 3.
    /// Identical average frame rate, identical latency percentiles — this is the broken-looking one.
    ///
    /// Also pins the TIE-BREAK. The histogram is 50/50 here, and Rust's `max_by_key` takes the
    /// last maximum while Swift's `max(by:)` takes the first, so both sides spell the rule out:
    /// ties resolve to the smallest spacing.
    func testTheSawtoothThatLatencyStatsCannotSee() {
        let s = cadence([Self.P, Self.P * 3], 40).summary()
        XCTAssertEqual(s?.judderPermille, 500)
        XCTAssertEqual(s?.modeUnits, 1, "a tied mode resolves to the smallest spacing")
    }

    /// Sub-refresh jitter is not judder: the display quantises it away, so the metric must too.
    func testJitterInsideARefreshIsNotJudder() {
        let s = cadence([Self.P + Self.P * 2 / 5, Self.P - Self.P * 2 / 5], 40).summary()
        XCTAssertEqual(s?.modeUnits, 1)
        XCTAssertEqual(s?.judderPermille, 0)
    }

    func testAStallIsCountedApartFromJudder() {
        var pi = PresentIntervals()
        var t: Int64 = 1_000_000_000
        pi.record(presentNs: t, periodNs: Self.P)
        for _ in 0..<20 {
            t += Self.P
            pi.record(presentNs: t, periodNs: Self.P)
        }
        t += Self.P * 400  // a pause, not a pacing defect
        pi.record(presentNs: t, periodNs: Self.P)
        let s = pi.summary()
        XCTAssertEqual(s?.judderPermille, 0)
        XCTAssertEqual(s?.stalls, 1)
        XCTAssertEqual(s?.samples, 20)
    }

    func testOutOfOrderCallbacksDoNotCorruptTheRun() {
        var pi = PresentIntervals()
        var t: Int64 = 1_000_000_000
        pi.record(presentNs: t, periodNs: Self.P)
        for _ in 0..<10 {
            t += Self.P
            pi.record(presentNs: t, periodNs: Self.P)
        }
        pi.record(presentNs: t - Self.P * 3, periodNs: Self.P)  // a late/duplicate delivery
        for _ in 0..<10 {
            t += Self.P
            pi.record(presentNs: t, periodNs: Self.P)
        }
        let s = pi.summary()
        XCTAssertEqual(s?.disordered, 1)
        XCTAssertEqual(
            s?.judderPermille, 0,
            "keeping the later instant means the following spacings stay on the grid")
    }

    func testAnUnknownGridScoresNothing() {
        var pi = PresentIntervals()
        var t: Int64 = 1_000_000_000
        for _ in 0..<60 {
            t += Self.P
            pi.record(presentNs: t, periodNs: 0)  // no learned period yet
        }
        XCTAssertNil(pi.summary())
        XCTAssertNotNil(cadence([Self.P], 60).summary(), "control")
    }

    func testAShortWindowPublishesNothing() {
        XCTAssertNil(cadence([Self.P], 5).summary())
    }

    /// The cadence continues across a window boundary — dropping the predecessor on drain would
    /// silently discard one interval per window, every window.
    func testTakeResetsTheCountsButNotTheCadence() {
        var pi = cadence([Self.P], 20)
        XCTAssertNotNil(pi.take())
        XCTAssertNil(pi.summary(), "counts cleared")
        var t: Int64 = 1_000_000_000 + Self.P * 20
        for _ in 0..<10 {
            t += Self.P
            pi.record(presentNs: t, periodNs: Self.P)
        }
        XCTAssertEqual(
            pi.summary()?.samples, 10,
            "the first post-drain present scored against the pre-drain one")
    }

    func testSplitForgetsThePredecessor() {
        var pi = cadence([Self.P], 20)
        _ = pi.take()
        pi.split()
        var t: Int64 = 5_000_000_000  // a discontinuity: the gap across it is meaningless
        for _ in 0..<10 {
            t += Self.P
            pi.record(presentNs: t, periodNs: Self.P)
        }
        let s = pi.summary()
        XCTAssertEqual(s?.samples, 9)
        XCTAssertEqual(s?.stalls, 0, "the gap was not scored at all")
    }
}
