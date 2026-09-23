// The quiet-period arithmetic behind `RetiredEngines`, driven with a synthetic clock. A parked
// engine is freed only after `hold` seconds with no hardware signal, counted from whichever came
// last: its own retirement or the signal.

import XCTest

@testable import PunktfunkKit

final class RetiredEnginesTests: XCTestCase {
    private final class Token {}

    private func makeHolder() -> RetiredEngines {
        RetiredEngines(queue: DispatchQueue(label: "io.unom.punktfunk.tests.retired"))
    }

    /// No hardware signal at all: the engine still waits out one full hold.
    func testAParkedEngineWaitsOutTheHold() {
        let holder = makeHolder()
        let token = Token()
        holder.retire(token, now: 100)
        XCTAssertTrue(holder.sweep(now: 100 + holder.hold - 0.01).isEmpty)
        let freed = holder.sweep(now: 100 + holder.hold)
        XCTAssertEqual(freed.count, 1)
        XCTAssertTrue(freed.first === token)
        XCTAssertTrue(holder.sweep(now: 1000).isEmpty)
    }

    /// A signal during the hold restarts it. One Bluetooth reconnect is several HAL signals one
    /// to two seconds apart, and the engine must outlive the last of them by a full hold.
    func testAHardwareSignalRestartsTheHold() {
        let holder = makeHolder()
        holder.retire(Token(), now: 100)
        holder.noteHardwareSignal(now: 102)
        XCTAssertTrue(holder.sweep(now: 102 + holder.hold - 0.01).isEmpty)
        XCTAssertEqual(holder.sweep(now: 102 + holder.hold).count, 1)
    }

    /// A signal BEFORE the retirement does not shorten the hold.
    func testAnEngineParkedAfterTheSignalStillWaitsAFullHold() {
        let holder = makeHolder()
        holder.noteHardwareSignal(now: 100)
        holder.retire(Token(), now: 102)
        XCTAssertTrue(holder.sweep(now: 102 + holder.hold - 0.01).isEmpty)
        XCTAssertEqual(holder.sweep(now: 102 + holder.hold).count, 1)
    }

    /// Engines parked at different times come due independently, in order.
    func testEnginesComeDueIndependently() {
        let holder = makeHolder()
        let first = Token()
        let second = Token()
        holder.retire(first, now: 100)
        holder.retire(second, now: 101)
        let freed = holder.sweep(now: 100 + holder.hold)
        XCTAssertEqual(freed.count, 1)
        XCTAssertTrue(freed.first === first)
        XCTAssertTrue(holder.sweep(now: 101 + holder.hold).first === second)
    }

    /// The queued sweep really frees the engine: with a short hold the token's last reference is
    /// gone soon after, and the holder itself survives the drop of the owner's reference.
    func testTheQueuedSweepFreesTheEngine() {
        weak var weakToken: Token?
        weak var weakHolder: RetiredEngines?
        do {
            let holder = RetiredEngines(
                queue: DispatchQueue(label: "io.unom.punktfunk.tests.retired"), hold: 0.05)
            let token = Token()
            weakToken = token
            weakHolder = holder
            holder.retire(token)
        }
        XCTAssertNotNil(weakToken, "the hold keeps the token alive")
        XCTAssertNotNil(weakHolder, "the pending sweep keeps the holder alive")
        let settled = expectation(description: "sweep ran")
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.5) { settled.fulfill() }
        wait(for: [settled], timeout: 2)
        XCTAssertNil(weakToken)
        XCTAssertNil(weakHolder)
    }
}
