#if os(macOS)
import XCTest

@testable import PunktfunkKit

// Verify lock transitions independently of the host's initial Caps Lock state
final class CapsLockStateTests: XCTestCase {
    // Both directions are presses of a latching key, not a held-key down/up pair
    func testEachStateChangeOwnsOneToggle() {
        var state = InputCapture.CapsLockState(rawFlags: 0x100)
        for _ in 0..<3 {
            XCTAssertTrue(state.takeTransition(keyCode: 57, rawFlags: 0x1_0100))
            XCTAssertFalse(state.takeTransition(keyCode: 57, rawFlags: 0x1_0100))
            XCTAssertTrue(state.takeTransition(keyCode: 57, rawFlags: 0x100))
            XCTAssertFalse(state.takeTransition(keyCode: 57, rawFlags: 0x100))
        }
    }

    // Capturing with Caps Lock already on must not toggle the host on arrival
    func testInitiallyEnabledLockIsOnlyABaseline() {
        var state = InputCapture.CapsLockState(rawFlags: 0x1_0100)
        XCTAssertFalse(state.takeTransition(keyCode: 57, rawFlags: 0x1_0100))
        XCTAssertTrue(state.takeTransition(keyCode: 57, rawFlags: 0x100))
    }

    // Local changes during released capture must not replay when capture resumes
    func testRecaptureBaselinesLocalChanges() {
        var state = InputCapture.CapsLockState(rawFlags: 0x100)
        XCTAssertTrue(state.takeTransition(keyCode: 57, rawFlags: 0x1_0100))
        state = InputCapture.CapsLockState(rawFlags: 0x100)
        XCTAssertFalse(state.takeTransition(keyCode: 57, rawFlags: 0x100))
        XCTAssertTrue(state.takeTransition(keyCode: 57, rawFlags: 0x1_0100))
    }

    // Another modifier carrying the lock bit cannot consume the pending Caps Lock transition
    func testOtherModifiersDoNotToggleOrUpdateLockState() {
        var state = InputCapture.CapsLockState(rawFlags: 0x100)
        XCTAssertFalse(state.takeTransition(keyCode: 59, rawFlags: 0x5_0101))
        XCTAssertTrue(state.takeTransition(keyCode: 57, rawFlags: 0x5_0101))
        XCTAssertFalse(state.takeTransition(keyCode: 55, rawFlags: 0x10_0108))
        XCTAssertTrue(state.takeTransition(keyCode: 57, rawFlags: 0x10_0108))
    }

    // Other held modifiers and device bits do not create extra Caps Lock toggles
    func testUnrelatedFlagChangesDoNotDuplicateAToggle() {
        var state = InputCapture.CapsLockState(rawFlags: 0x100)
        XCTAssertTrue(state.takeTransition(keyCode: 57, rawFlags: 0x5_0101))
        XCTAssertFalse(state.takeTransition(keyCode: 57, rawFlags: 0x11_0108))
        XCTAssertTrue(state.takeTransition(keyCode: 57, rawFlags: 0x10_0108))
        XCTAssertFalse(state.takeTransition(keyCode: 57, rawFlags: 0x100))
    }
}
#endif
