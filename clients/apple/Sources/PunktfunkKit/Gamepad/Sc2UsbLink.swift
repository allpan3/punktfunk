// IOKit HID transport for a Steam Controller 2 attached over USB — wired (`28DE:1302`) or
// through the wireless Puck dongle (`1304`/`1305`). The macOS sibling of Android's
// `Sc2UsbLink.kt` + `HidUsbLink.kt`, and the USB half of `Sc2Capture`'s two transports; it
// presents the SAME surface as `Sc2BleLink` (`start` / `stop` / `writeRaw`) so the capture holds
// either without caring which.
//
// **Why this is so much smaller than the Android pair (~200 lines against ~580).** IOKit absorbs
// nearly everything `HidUsbLink` hand-rolls: there is no runtime permission to request (the
// sandbox's `device.usb` entitlement covers it), no interface to claim, no `UsbRequest` read loop
// to multiplex, and no EP0 fallback — `IOHIDDeviceRegisterInputReportCallback` delivers reports
// on a dispatch queue and `IOHIDDeviceSetReport` performs both output and feature writes.
//
// **The match dictionary is the load-bearing design decision.** macOS splits one multi-collection
// USB HID interface into one `IOHIDDevice` PER TOP-LEVEL COLLECTION (bench census: a single
// Keychron VID/PID surfaces as five devices, one per collection). The SC2 declares three —
// lizard mouse (01:02, report 0x40), lizard keyboard (01:06, report 0x41), and the controller
// (vendor page FF00:01, reports 0x42/0x43/0x45/0x79 and outputs 0x80…0x89). Matching on VID/PID
// alone would open the KEYBOARD collection too, which puts the whole app behind the Input
// Monitoring TCC gate; pinning `Sc2Device.usagePageVendor`/`usageController` opens exactly the
// controller collection and leaves the keylogging-class surface untouched.
//
// **The Puck opens every match, not one.** The dongle hosts up to four pads on interfaces 2…5
// and nothing says which slot a controller bonded to — Android's first on-glass run claimed only
// interface 2 and read silence. So every matched collection is opened, and whichever one streams
// reports becomes the write target for rumble and settings.
//
// **Keep-alive.** The firmware watchdog re-enables lizard mode after a few seconds of silence, so
// `disableLizard` + `normalizeJoysticks` are re-sent on SDL's ~3 s cadence — the same contract
// the BLE link keeps, plus the joystick normalization a USB host's leftover raw mode requires.
// The client NEVER self-enables the gyro: Steam's own forwarded write is what opens `Sc2ImuGate`.
//
// macOS-only: IOKit HID device access is not available to apps on iOS/tvOS, which is why the SC2
// over USB is a Mac capability and BLE remains the only iOS transport.

#if os(macOS)

import Foundation
import IOKit
import IOKit.hid

private let log = ClientLog(category: "gamepad")

final class Sc2UsbLink {
    /// Per-report diagnostics. Lifecycle milestones always log; flip this only to debug the seam.
    private static let verbose = false

    /// The queue every IOKit callback and every mutation below runs on — owned by `Sc2Capture`,
    /// USER_INTERACTIVE for the same reason the BLE link's is (a stalled consumer drops reports).
    private let queue: DispatchQueue
    /// One incoming report, id-first — on `queue`. USB reports carry their id out of band, so
    /// this link prepends it; the wire contract is id-first on every transport.
    private let onReport: ([UInt8]) -> Void
    /// Every opened collection went away (unplug / dongle pulled) — on `queue`.
    private let onClosed: () -> Void

    // All state below is touched ONLY on `queue`.
    private var manager: IOHIDManager?
    /// Every opened controller collection, by IOKit registry entry id. The Puck contributes up
    /// to four; a wired pad exactly one.
    private var open: [UInt64: IOHIDDevice] = [:]
    /// The collection that last streamed a report — where `writeRaw` sends. Nil until the first
    /// report, which is also when `Sc2Capture` claims its wire slot, so no write can precede it.
    private var target: IOHIDDevice?
    /// Per-device input buffer, kept alive for as long as the device is open:
    /// `IOHIDDeviceRegisterInputReportCallback` writes into this memory for the device's whole
    /// lifetime, so a Swift array's storage would be a dangling pointer the moment it moved.
    private var buffers: [UInt64: UnsafeMutablePointer<UInt8>] = [:]
    private var keepAlive: DispatchSourceTimer?
    private var started = false
    /// Report ids seen so far — one line each, for remote diagnosis of what the pad emits.
    private var seenIds: Set<UInt8> = []

    /// True while the opened device is a Puck dongle. Read by `Sc2Capture` to pick the declared
    /// wire kind and to decide whether wireless-status reports are authoritative — a WIRED pad
    /// emits them too, truthfully reporting "no radio link".
    private(set) var isDongle = false

    /// `IOHIDDeviceRegisterInputReportCallback`'s report buffer size. The Triton's longest input
    /// report is the 54-byte 0x42 state; 64 is the HID report bound the ABI already clamps to
    /// (`PUNKTFUNK_HID_REPORT_MAX`).
    private static let reportBufSize = 64

    init(
        queue: DispatchQueue,
        onReport: @escaping ([UInt8]) -> Void,
        onClosed: @escaping () -> Void
    ) {
        self.queue = queue
        self.onReport = onReport
        self.onClosed = onClosed
    }

    /// Whether any SC2 controller collection is attached right now — the cheap pre-flight
    /// `Sc2Capture` uses to pick USB over BLE. Opens nothing, so it cannot prompt or disturb a
    /// device another app holds. Safe from any thread.
    static func attached() -> Bool {
        let mgr = IOHIDManagerCreate(kCFAllocatorDefault, IOOptionBits(kIOHIDOptionsTypeNone))
        IOHIDManagerSetDeviceMatchingMultiple(mgr, matchingCriteria() as CFArray)
        let devices = IOHIDManagerCopyDevices(mgr) as? Set<IOHIDDevice> ?? []
        return !devices.isEmpty
    }

    /// One match dictionary per USB product id, each pinned to the controller collection's usage
    /// pair (see the file header — this is what keeps the lizard keyboard out of our hands).
    private static func matchingCriteria() -> [CFDictionary] {
        Sc2Device.usbPIDs.map { pid in
            [
                kIOHIDVendorIDKey: Sc2Device.vidValve,
                kIOHIDProductIDKey: pid,
                kIOHIDPrimaryUsagePageKey: Sc2Device.usagePageVendor,
                kIOHIDPrimaryUsageKey: Sc2Device.usageController,
            ] as CFDictionary
        }
    }

    /// Begin acquisition. Idempotent; safe from any thread. Devices attached later are picked up
    /// by the manager's matching callback, so a pad plugged in mid-session simply joins.
    func start() {
        queue.async { [self] in
            guard manager == nil else { return }
            started = true
            let mgr = IOHIDManagerCreate(kCFAllocatorDefault, IOOptionBits(kIOHIDOptionsTypeNone))
            IOHIDManagerSetDeviceMatchingMultiple(mgr, Self.matchingCriteria() as CFArray)
            let ctx = Unmanaged.passUnretained(self).toOpaque()
            IOHIDManagerRegisterDeviceMatchingCallback(
                mgr,
                { ctx, _, _, device in
                    guard let ctx else { return }
                    Unmanaged<Sc2UsbLink>.fromOpaque(ctx).takeUnretainedValue().adopt(device)
                }, ctx)
            IOHIDManagerRegisterDeviceRemovalCallback(
                mgr,
                { ctx, _, _, device in
                    guard let ctx else { return }
                    Unmanaged<Sc2UsbLink>.fromOpaque(ctx).takeUnretainedValue().drop(device)
                }, ctx)
            IOHIDManagerSetDispatchQueue(mgr, queue)
            IOHIDManagerActivate(mgr)
            manager = mgr
        }
    }

    /// Close every collection and tear the manager down. Idempotent; safe from any thread. Does
    /// not fire `onClosed` — the caller is the one tearing down.
    func stop() {
        queue.async { [self] in
            started = false
            stopKeepAlive()
            for (id, dev) in open { cancel(id: id, device: dev) }
            open.removeAll()
            target = nil
            isDongle = false
            seenIds.removeAll()
            if let mgr = manager {
                // Dispatch-queue mode: Cancel, never Close/UnscheduleFromRunLoop — mixing the
                // run-loop teardown API with a queue-scheduled manager is undefined and crashes.
                IOHIDManagerCancel(mgr)
            }
            manager = nil
        }
    }

    /// Retire one opened collection: stop delivery, then free its report buffer — in that order,
    /// and only once IOKit says delivery has actually stopped.
    ///
    /// The ordering is the whole point. `IOHIDDeviceRegisterInputReportCallback` keeps writing
    /// into `buffers[id]` until the device is cancelled, and `IOHIDDeviceCancel` is ASYNCHRONOUS.
    /// Deallocating on the calling side would therefore race a callback already in flight and
    /// scribble on freed memory. The cancel handler is the one place IOKit guarantees no further
    /// callback can arrive, so the close and the free both live there.
    private func cancel(id: UInt64, device: IOHIDDevice) {
        let buf = buffers.removeValue(forKey: id)
        IOHIDDeviceSetCancelHandler(device) {
            IOHIDDeviceClose(device, IOOptionBits(kIOHIDOptionsTypeNone))
            buf?.deallocate()
        }
        IOHIDDeviceCancel(device)
    }

    /// Replay one raw host report on the physical pad. `kind` is the C ABI's
    /// `PUNKTFUNK_HID_RAW_OUTPUT` (0) / `PUNKTFUNK_HID_RAW_FEATURE` (1); `frame` is id-first,
    /// exactly as Steam wrote it. Safe from any thread.
    ///
    /// Unlike the BLE link there is no per-report characteristic to resolve and NO trimming:
    /// `IOHIDDeviceSetReport` takes the frame as the device's own HID stack expects it, which is
    /// precisely what the host already sent. (`Sc2Device.strippedOutputLen` exists to undo the
    /// GATT transport's id-stripping; USB has no such transform to undo.)
    func writeRaw(kind: UInt8, frame: [UInt8]) {
        queue.async { [self] in
            guard let dev = target, let id = frame.first else { return }
            let type: IOHIDReportType = kind == 1 ? kIOHIDReportTypeFeature : kIOHIDReportTypeOutput
            let rc = frame.withUnsafeBufferPointer { buf in
                IOHIDDeviceSetReport(dev, type, CFIndex(id), buf.baseAddress!, buf.count)
            }
            if rc != kIOReturnSuccess {
                // Not fatal and deliberately not retried: rumble is re-sent by Steam every
                // 25–40 ms and settings every ~3 s, so the next frame self-heals. Worth a line —
                // a persistently failing write is invisible otherwise.
                log.error(
                    "SC2 USB: SetReport id 0x\(String(id, radix: 16), privacy: .public) failed (0x\(String(format: "%08x", rc), privacy: .public))"
                )
            }
        }
    }

    // MARK: - Device lifecycle (queue)

    /// Open one newly matched controller collection and start its report callback.
    private func adopt(_ device: IOHIDDevice) {
        guard started else { return }
        let id = Self.registryID(device)
        guard open[id] == nil else { return }
        let rc = IOHIDDeviceOpen(device, IOOptionBits(kIOHIDOptionsTypeNone))
        guard rc == kIOReturnSuccess else {
            // kIOReturnNotPermitted here would mean the usage-pair match failed to keep us off a
            // keyboard collection (see the file header) — say so plainly rather than leaving a
            // bare hex code, because the fix is a TCC grant, not a retry.
            let hint = rc == kIOReturnNotPermitted ? " — not permitted (Input Monitoring)" : ""
            log.error(
                "SC2 USB: open failed (0x\(String(format: "%08x", rc), privacy: .public))\(hint, privacy: .public)"
            )
            return
        }
        let pid = (IOHIDDeviceGetProperty(device, kIOHIDProductIDKey as CFString) as? Int) ?? 0
        let buf = UnsafeMutablePointer<UInt8>.allocate(capacity: Self.reportBufSize)
        buf.initialize(repeating: 0, count: Self.reportBufSize)
        buffers[id] = buf
        open[id] = device
        isDongle = Sc2Device.isDongle(pid: pid)
        IOHIDDeviceRegisterInputReportCallback(
            device, buf, Self.reportBufSize,
            { ctx, _, sender, _, reportID, report, len in
                guard let ctx, let sender else { return }
                let link = Unmanaged<Sc2UsbLink>.fromOpaque(ctx).takeUnretainedValue()
                let dev = Unmanaged<IOHIDDevice>.fromOpaque(sender).takeUnretainedValue()
                link.handle(device: dev, reportID: reportID, report: report, len: len)
            }, Unmanaged.passUnretained(self).toOpaque())
        IOHIDDeviceSetDispatchQueue(device, queue)
        IOHIDDeviceActivate(device)
        log.info(
            "SC2 USB: opened \(Sc2Device.isDongle(pid: pid) ? "Puck" : "wired", privacy: .public) collection 0x\(String(format: "%04x", pid), privacy: .public) (\(self.open.count, privacy: .public) open)"
        )
        startKeepAlive()
    }

    /// A collection went away. The link reports closed only when the LAST one does — a Puck
    /// losing one of its four slots is not an unplug.
    private func drop(_ device: IOHIDDevice) {
        let id = Self.registryID(device)
        guard open.removeValue(forKey: id) != nil else { return }
        cancel(id: id, device: device)
        if target === device { target = nil }
        guard open.isEmpty else { return }
        stopKeepAlive()
        isDongle = false
        seenIds.removeAll()
        log.info("SC2 USB: last collection removed — link closed")
        onClosed()
    }

    /// One input report from IOKit. USB delivers the id out of band (`reportID`) and the payload
    /// without it, so the id is prepended here: the punktfunk wire — and `Sc2Device.parseState`,
    /// and `Sc2ImuGate` — are id-first on every transport.
    private func handle(
        device: IOHIDDevice, reportID: UInt32, report: UnsafeMutablePointer<UInt8>, len: CFIndex
    ) {
        guard len > 0 else { return }
        // Whichever collection streams becomes the write target — the Puck's bonded slot is not
        // knowable in advance (Android's on-glass lesson, mirrored).
        if target !== device { target = device }
        let id = UInt8(truncatingIfNeeded: reportID)
        if seenIds.insert(id).inserted {
            log.info(
                "SC2 USB: report id=0x\(String(id, radix: 16), privacy: .public) seen (len=\(len, privacy: .public))"
            )
        }
        var framed = [UInt8](repeating: 0, count: min(Int(len), Self.reportBufSize - 1) + 1)
        framed[0] = id
        for i in 1..<framed.count { framed[i] = report[i - 1] }
        if Self.verbose {
            log.debug("SC2 USB in: \(framed.map { String(format: "%02x", $0) }.joined(), privacy: .public)")
        }
        onReport(framed)
    }

    // MARK: - Lizard keep-alive

    /// Re-send the initialization features on SDL's cadence, and once immediately: without it the
    /// firmware watchdog restores lizard mode a few seconds in, and the pad reverts to driving
    /// the desktop cursor instead of streaming controller reports.
    private func startKeepAlive() {
        guard keepAlive == nil else { return }
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: Sc2Device.lizardRefreshSeconds)
        timer.setEventHandler { [weak self] in self?.sendInitFeatures() }
        keepAlive = timer
        timer.resume()
    }

    private func stopKeepAlive() {
        keepAlive?.cancel()
        keepAlive = nil
    }

    /// Both initialization features, to EVERY open collection — before the first report there is
    /// no known target, and on a Puck the bonded slot is exactly what we are trying to discover.
    private func sendInitFeatures() {
        for (_, dev) in open {
            for frame in [Sc2Device.disableLizard, Sc2Device.normalizeJoysticks] {
                _ = frame.withUnsafeBufferPointer { buf in
                    IOHIDDeviceSetReport(
                        dev, kIOHIDReportTypeFeature, CFIndex(frame[0]), buf.baseAddress!,
                        buf.count)
                }
            }
        }
    }

    // MARK: - Helpers

    /// A stable per-collection key. `IOHIDDevice` is a CF type with no usable identity in a
    /// Swift dictionary, and the Puck's four collections share VID/PID, so the IOKit registry
    /// entry id is what tells them apart.
    private static func registryID(_ device: IOHIDDevice) -> UInt64 {
        let service = IOHIDDeviceGetService(device)
        var id: UInt64 = 0
        if service != MACH_PORT_NULL, IORegistryEntryGetRegistryEntryID(service, &id) == KERN_SUCCESS {
            return id
        }
        // No registry entry (should not happen for a matched USB device). The pointer is still a
        // stable per-collection identity for as long as we hold the device, which is all this key
        // is for.
        return UInt64(UInt(bitPattern: Unmanaged.passUnretained(device).toOpaque()))
    }
}

#endif
