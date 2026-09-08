// The per-access-unit bookkeeping both VideoToolbox pumps do: the straggler filter, the format
// and decoded-size tracking, and the keyframe WANT that only an IDR's parameter sets can end.
//
// Pure by design — the caller reads the connection and applies what `note` returns — so the rules
// are testable without a host. It exists as one type because the two pumps carried the same
// ~60 lines and had already drifted apart: both of the loss-recovery defects found in the Apple
// client had to be fixed twice, and the second copy is easy to miss.
//
// Loss recovery itself is NOT here. That runs through the shared `ReanchorGate`, whose credited
// arm is the whole reason a clean recovery-anchor P heals a stream without an IDR.

import CoreMedia
import Foundation

struct AUPumpState {
    /// The live format description. Only an IDR's parameter sets can set it.
    private(set) var format: CMVideoFormatDescription?
    /// The last size reported upward, so a loss-recovery IDR at the same size stays quiet.
    private var lastDims: CMVideoDimensions?
    /// Newest submitted frame index, for the straggler filter.
    private var newestIndex: UInt32?
    /// Persistent WANT for the two states only parameter sets can end: no decodable format yet,
    /// or a decoder reset. The caller re-asks (throttled) while it is true.
    private(set) var awaitingIDR = false

    /// What the caller should do about this access unit.
    struct Step: Equatable {
        /// Arrived behind one already submitted: decoding it rewinds the reference buffer, so
        /// skip it entirely.
        var straggler = false
        /// The decoded size changed — report it (a new-mode IDR, not a same-size recovery one).
        var newSize: Size?
        /// This AU ended a recovery the pump was waiting on.
        var resumed = false
        /// The wait for a decodable format began with this AU (log once, not per AU).
        var startedFormatWait = false

        struct Size: Equatable {
            var width: Int
            var height: Int
        }
    }

    /// Fold one access unit in. `idrFormat` is what the codec made of its parameter sets, or nil
    /// for a delta frame.
    mutating func note(frameIndex: UInt32, idrFormat: CMVideoFormatDescription?) -> Step {
        var step = Step()
        // Wraparound-safe: the index is a 32-bit counter, so compare the difference as signed.
        if let newest = newestIndex, Int32(bitPattern: frameIndex &- newest) <= 0 {
            step.straggler = true
            return step
        }
        newestIndex = frameIndex

        if let f = idrFormat {
            format = f // refreshed on every IDR, mode changes included
            let dims = CMVideoFormatDescriptionGetDimensions(f)
            if lastDims?.width != dims.width || lastDims?.height != dims.height {
                lastDims = dims
                step.newSize = .init(width: Int(dims.width), height: Int(dims.height))
            }
            if awaitingIDR { step.resumed = true }
            awaitingIDR = false
        }

        if format == nil {
            // Nothing decodable yet: the opening IDR's parameter sets never arrived or never
            // parsed, and under the host's infinite GOP nothing re-delivers them unless we ASK.
            // Without this every AU is dropped silently, forever.
            step.startedFormatWait = !awaitingIDR
            awaitingIDR = true
        }
        return step
    }

    /// A wedged decoder or a reset: drop the format and wait for the next parameter sets.
    mutating func requireIDR() {
        format = nil
        awaitingIDR = true
    }
}
