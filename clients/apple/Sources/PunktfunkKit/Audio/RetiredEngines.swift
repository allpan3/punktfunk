// Stopped audio engines, held until the hardware has been quiet and then freed off the main
// thread.
//
// AVFAudio answers a property change on an engine's IO unit with a block on its own serial queue.
// That block names the engine without retaining it, and `-[AVAudioEngine dealloc]` does not wait
// for it: an engine freed while such a block is still queued crashes the process in
// `objc_msgSend`. Property changes come from the hardware (a Bluetooth headset switching
// profile, a device leaving), which is exactly when `SessionAudio` drops engines. So a dropped
// engine is parked here and released only after `hold` seconds with no hardware signal.
//
// Pinned by RetiredEnginesTests (synthetic clock). Evidence: the crash reports on the PR that
// added this file.

import AVFoundation
import Foundation

/// Parks stopped engines and frees them on `queue` once the hardware has been quiet for `hold`,
/// counted from the later of the engine's retirement and the last hardware signal. Any object
/// can be parked (tests pass tokens). Thread-safe. A pending release retains the holder, so it
/// outlives the session that made it.
final class RetiredEngines {
    /// Quiet period before a parked engine is freed. Longer than the gap between the HAL signals
    /// of one device switch, which is about two seconds for a Bluetooth headset changing profile.
    static let defaultHold: TimeInterval = 3

    let hold: TimeInterval
    /// Where releases run. Never the main thread: an engine's dealloc can block on the audio
    /// server.
    private let queue: DispatchQueue
    private let lock = NSLock()
    /// Parked objects with the uptime they were parked at. Guarded by `lock`.
    private var held: [(object: AnyObject, at: TimeInterval)] = []
    /// Uptime of the last hardware signal. Guarded by `lock`.
    private var lastSignal: TimeInterval = -.infinity
    /// A sweep is already queued. Guarded by `lock`.
    private var sweepQueued = false
    private var configObserver: NSObjectProtocol?

    init(queue: DispatchQueue, hold: TimeInterval = RetiredEngines.defaultHold) {
        self.queue = queue
        self.hold = hold
        // Every engine in the process, not only the owner's live ones: a parked engine's own IO
        // unit still posts, and the owner's identity filter drops that notification.
        configObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: nil, queue: nil
        ) { [weak self] _ in self?.noteHardwareSignal() }
    }

    deinit {
        if let configObserver { NotificationCenter.default.removeObserver(configObserver) }
    }

    /// The hardware moved (device change, route change, an engine's configuration change).
    /// Listener blocks may be queued, so every parked engine waits another `hold` from `now`.
    func noteHardwareSignal(now: TimeInterval = ProcessInfo.processInfo.systemUptime) {
        lock.lock()
        lastSignal = max(lastSignal, now)
        lock.unlock()
    }

    /// Park `object`, which is already stopped, until the hardware has been quiet for `hold`.
    func retire(_ object: AnyObject, now: TimeInterval = ProcessInfo.processInfo.systemUptime) {
        lock.lock()
        held.append((object, now))
        let schedule = !sweepQueued
        sweepQueued = true
        lock.unlock()
        if schedule { scheduleSweep(after: hold) }
    }

    /// Take every parked object whose quiet period has passed at `now`. The caller frees them by
    /// dropping the array; tests inspect it.
    @discardableResult
    func sweep(now: TimeInterval = ProcessInfo.processInfo.systemUptime) -> [AnyObject] {
        lock.lock()
        defer { lock.unlock() }
        var due: [AnyObject] = []
        held.removeAll { entry in
            let release = now - max(entry.at, lastSignal) >= hold
            if release { due.append(entry.object) }
            return release
        }
        return due
    }

    /// `self` is captured strongly on purpose: the pending sweep keeps the holder, and with it
    /// the parked engines, alive until they are due.
    private func scheduleSweep(after delay: TimeInterval) {
        queue.asyncAfter(deadline: .now() + delay) {
            let now = ProcessInfo.processInfo.systemUptime
            let freed = self.sweep(now: now)
            self.lock.lock()
            let next = self.held.map { max($0.at, self.lastSignal) + self.hold - now }.min()
            self.sweepQueued = next != nil
            self.lock.unlock()
            if let next { self.scheduleSweep(after: max(next, 0.05)) }
            withExtendedLifetime(freed) {} // released here, on `queue`
        }
    }
}
