// The rules both VideoToolbox pumps follow per access unit. Neither pump had a test before this,
// which is how a straggler reached the decoder and how a recovery anchor never ended a wait.

import CoreMedia
import XCTest

@testable import PunktfunkKit

final class AUPumpStateTests: XCTestCase {
    /// A format description of a given size, built the cheap way (a made-up H.264 SPS/PPS pair
    /// would be a second parser to maintain — only the dimensions matter here).
    private func format(_ w: Int32, _ h: Int32) throws -> CMVideoFormatDescription {
        var f: CMVideoFormatDescription?
        let status = CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault, codecType: kCMVideoCodecType_HEVC,
            width: w, height: h, extensions: nil, formatDescriptionOut: &f)
        XCTAssertEqual(status, noErr)
        return try XCTUnwrap(f)
    }

    func testFirstIDRSeedsTheFormatAndReportsItsSize() throws {
        var state = AUPumpState()
        let step = state.note(frameIndex: 1, idrFormat: try format(1920, 1080))
        XCTAssertEqual(step.newSize, .init(width: 1920, height: 1080))
        XCTAssertFalse(step.straggler)
        XCTAssertFalse(state.awaitingIDR)
        XCTAssertNotNil(state.format)
    }

    func testDeltaBeforeAnyFormatAsksForOneExactlyOnce() {
        var state = AUPumpState()
        let first = state.note(frameIndex: 1, idrFormat: nil)
        XCTAssertTrue(first.startedFormatWait, "the first AU with no format starts the wait")
        XCTAssertTrue(state.awaitingIDR)
        let second = state.note(frameIndex: 2, idrFormat: nil)
        XCTAssertFalse(second.startedFormatWait, "the wait is announced once, not per AU")
        XCTAssertTrue(state.awaitingIDR)
    }

    func testSameSizeRecoveryIDRDoesNotReportASize() throws {
        var state = AUPumpState()
        _ = state.note(frameIndex: 1, idrFormat: try format(1920, 1080))
        let again = state.note(frameIndex: 2, idrFormat: try format(1920, 1080))
        XCTAssertNil(again.newSize, "a loss-recovery IDR at the same size is not a resize")
    }

    func testModeChangeIDRReportsTheNewSize() throws {
        var state = AUPumpState()
        _ = state.note(frameIndex: 1, idrFormat: try format(1920, 1080))
        let resized = state.note(frameIndex: 2, idrFormat: try format(2560, 1440))
        XCTAssertEqual(resized.newSize, .init(width: 2560, height: 1440))
    }

    func testStragglerIsSkippedAndLeavesTheStateAlone() throws {
        var state = AUPumpState()
        _ = state.note(frameIndex: 10, idrFormat: try format(1920, 1080))
        let late = state.note(frameIndex: 9, idrFormat: nil)
        XCTAssertTrue(late.straggler)
        // A newer frame still goes through, so one straggler does not stall the stream.
        XCTAssertFalse(state.note(frameIndex: 11, idrFormat: nil).straggler)
    }

    func testDuplicateIndexIsAStraggler() throws {
        var state = AUPumpState()
        _ = state.note(frameIndex: 5, idrFormat: try format(1280, 720))
        XCTAssertTrue(state.note(frameIndex: 5, idrFormat: nil).straggler)
    }

    func testStragglerFilterSurvivesIndexWraparound() throws {
        var state = AUPumpState()
        _ = state.note(frameIndex: .max - 1, idrFormat: try format(1280, 720))
        // Wrapping FORWARD past UInt32.max is a normal advance, not a rewind.
        XCTAssertFalse(state.note(frameIndex: 2, idrFormat: nil).straggler)
        // And a frame from before the wrap is still late.
        XCTAssertTrue(state.note(frameIndex: .max, idrFormat: nil).straggler)
    }

    func testRequireIDRDropsTheFormatUntilParameterSetsReturn() throws {
        var state = AUPumpState()
        _ = state.note(frameIndex: 1, idrFormat: try format(1920, 1080))
        state.requireIDR()
        XCTAssertNil(state.format)
        XCTAssertTrue(state.awaitingIDR)
        let recovered = state.note(frameIndex: 2, idrFormat: try format(1920, 1080))
        XCTAssertTrue(recovered.resumed, "the IDR that re-anchors decode ends the wait")
        XCTAssertFalse(state.awaitingIDR)
        XCTAssertNotNil(state.format)
    }

    func testADeltaNeverEndsTheWait() throws {
        var state = AUPumpState()
        _ = state.note(frameIndex: 1, idrFormat: try format(1920, 1080))
        state.requireIDR()
        let delta = state.note(frameIndex: 2, idrFormat: nil)
        XCTAssertFalse(delta.resumed)
        XCTAssertTrue(state.awaitingIDR, "only parameter sets can clear this want")
    }
}
