// App Store screenshot harness — the in-app "shot mode" root.
//
// Launched with PUNKTFUNK_SHOT_SCENE=<name> (one of ShotScenes.all), the app shows that single
// mock-populated scene full-bleed instead of ContentView, so the OS can screenshot the REAL,
// fully-rendered UI (materials, NavigationStack, glass — all the things ImageRenderer can't
// rasterize offscreen). tools/screenshots.sh drives one launch per scene per device.
//
// Capture per platform:
//   • iOS / tvOS simulator → `xcrun simctl io booted screenshot` (native pixels = exact size).
//   • macOS → the app captures its own windows through the window server
//     (PUNKTFUNK_SHOT_SELFCAPTURE=<dir>, see MacSelfCapture): no Screen Recording grant.
//
// Every screen prints `PF_SHOT_READY scene=<name>` to stdout once it has settled, so the driver
// can wait for layout instead of guessing with a fixed sleep.

#if DEBUG
import PunktfunkKit
import SwiftUI
#if os(macOS)
import AppKit
import ImageIO
#endif

@MainActor
enum ScreenshotMode {
    /// This process was launched to capture a screenshot. Cheap enough to consult from the
    /// stores' persistence paths (`HostStore` / `ProfileStore`), which must NOT write their
    /// mock contents back into a real user's App Group when the harness runs on a dev Mac.
    static var isActive: Bool {
        !(ProcessInfo.processInfo.environment["PUNKTFUNK_SHOT_SCENE"] ?? "").isEmpty
    }

    /// The scene requested via PUNKTFUNK_SHOT_SCENE, or nil for a normal launch.
    static var requestedScene: ShotScene? {
        let name = ProcessInfo.processInfo.environment["PUNKTFUNK_SHOT_SCENE"] ?? ""
        guard !name.isEmpty else { return nil }
        return ShotScenes.all.first { $0.name == name }
    }
}

/// Full-bleed host for a single scene, with per-platform window sizing / orientation and a
/// readiness ping for the capture script.
struct ScreenshotHostView: View {
    let scene: ShotScene

    init(scene: ShotScene) {
        self.scene = scene
        // Pin the palette for the capture. The aurora screens read the LIVE `uiPalette` default,
        // and a reused Simulator (or a dev Mac) carries whatever was last picked there — the
        // Apple TV set once shipped out on a sunset palette that a test device had persisted.
        // Idempotent, and only ever runs in shot mode (this view exists behind that gate).
        UserDefaults.standard.set(
            ProcessInfo.processInfo.environment["PUNKTFUNK_SHOT_PALETTE"] ?? "violet",
            forKey: DefaultsKey.uiPalette)
    }
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var hSizeClass
    @Environment(\.verticalSizeClass) private var vSizeClass
    #endif

    /// The gamepad UI's form-metric tier, published here for the same reason ContentView does it:
    /// this harness mounts those screens DIRECTLY, with no ContentView in the tree, so without it
    /// an iPad capture renders every gamepad screen at iPhone scale — a capture that doesn't look
    /// like the app.
    private var gamepadMetrics: GamepadFormMetrics {
        #if os(iOS)
        .forWindow(h: hSizeClass, v: vSizeClass)
        #else
        .platformDefault
        #endif
    }

    #if os(macOS)
    /// The scene mounts once the window sits on the canvas: laid out at the launch size and then
    /// resized, a carousel keeps whichever card sat at its old offset.
    @State private var placed = false
    #endif

    @ViewBuilder private var sceneContent: some View {
        #if os(macOS)
        if placed { scene.make() } else { Color.clear }
        #else
        scene.make()
        #endif
    }

    var body: some View {
        sceneContent
            .environment(\.colorScheme, scene.colorScheme)
            .environment(\.gamepadMetrics, gamepadMetrics)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // The scene keeps its safe area, so the HUD clears the Dynamic Island; the streamed
            // frame ignores it itself. Black matches the dark iOS window. tvOS and macOS keep the
            // system backdrop and window background the real app sits on.
            #if os(iOS)
            .background(Color.black.ignoresSafeArea())
            #endif
            #if os(macOS)
            .background(MacShotWindowConfigurator(scene: scene) { placed = true })
            #elseif os(iOS)
            .background(IOSOrientationConfigurator(orientation: scene.orientation))
            #endif
            .task {
                // Let layout + materials settle, then signal the driver. PUNKTFUNK_SHOT_DELAY
                // (milliseconds) moves that moment: a scene that ANIMATES — the launch hold's
                // cover leaving its tile — is only capturable by choosing when to look at it.
                let ms = ProcessInfo.processInfo.environment["PUNKTFUNK_SHOT_DELAY"]
                    .flatMap(UInt64.init) ?? 900
                try? await Task.sleep(nanoseconds: ms * 1_000_000)
                announceReady()
            }
    }

    private func announceReady() {
        print("PF_SHOT_READY scene=\(scene.name)")
        fflush(stdout)
        #if os(macOS)
        MacSelfCapture.captureIfRequested(scene: scene)
        #endif
    }
}

#if os(macOS)
/// Puts the hosting window on the mac canvas and hands its rect to `MacSelfCapture`.
///
/// On a 2× display the canvas (1440×900 pt) is exactly the App Store pixels. A scene is the app's
/// own titled window, its frame on the canvas just below the menu bar, so the display needs that
/// much room under it. A `macFullScreen` scene drops the chrome, as the full-screen stream does,
/// and may sit under the menu bar: the capture takes only this app's windows. Without a 2×
/// display the window floats at 1×.
private struct MacShotWindowConfigurator: NSViewRepresentable {
    let scene: ShotScene
    let onPlaced: () -> Void

    func makeNSView(context: Context) -> NSView { NSView() }

    func updateNSView(_ view: NSView, context: Context) {
        DispatchQueue.main.async {
            guard let window = view.window, !context.coordinator.configured else { return }
            context.coordinator.configured = true
            // NavigationStack / Form / material chrome follow the appearance, not the SwiftUI
            // colorScheme. App-wide, so the Settings window and sheets a scene opens match.
            NSApp.appearance = NSAppearance(named: scene.colorScheme == .dark ? .darkAqua : .aqua)
            let size = ShotDevice.mac.points(scene.orientation)
            let name = scene.name
            let area = NSScreen.screens.lazy
                .filter { $0.backingScaleFactor == ShotDevice.mac.scale }
                .map { scene.macFullScreen ? $0.frame : $0.visibleFrame }
                .first { $0.width >= size.width && $0.height >= size.height }
            if scene.macFullScreen {
                window.styleMask = [.borderless]
                window.hasShadow = false // its rim would sit inside the canvas
            }
            var canvas: NSRect?
            if let area {
                // A borderless window leaves its outermost point clear, so the chromeless canvas
                // sits one point inside it. The top edge stays on screen, or macOS pushes it down.
                let pad: CGFloat = scene.macFullScreen ? 1 : 0
                let top = area.maxY.rounded(.down)
                let frame = NSRect(x: area.minX, y: top - size.height - 2 * pad,
                                   width: size.width + 2 * pad, height: size.height + 2 * pad)
                window.setFrame(frame, display: true)
                canvas = frame.insetBy(dx: pad, dy: pad)
            } else {
                window.setFrame(NSRect(origin: .zero, size: size), display: true)
                window.center()
            }
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            MacSelfCapture.canvas = Self.globalRect(canvas ?? window.frame)
            Self.announce(window, name, size)
            onPlaced()
        }
    }

    /// `frame` in the top-left global space that window captures take.
    private static func globalRect(_ frame: NSRect) -> CGRect {
        let top = (NSScreen.screens.first?.frame.height ?? 0) - frame.maxY
        return CGRect(x: frame.minX, y: top, width: frame.width, height: frame.height)
    }

    private static func announce(_ window: NSWindow, _ name: String, _ size: CGSize) {
        print("PF_SHOT_WINDOW=\(window.windowNumber) scene=\(name) "
            + "size=\(Int(size.width))x\(Int(size.height))pt")
        fflush(stdout)
    }

    func makeCoordinator() -> Coordinator { Coordinator() }
    final class Coordinator { var configured = false }
}

/// PUNKTFUNK_SHOT_SELFCAPTURE=<dir>: once the scene is ready the app captures its own windows
/// through the window server and exits. A process may always read its own windows, so there is
/// no Screen Recording grant, and materials come out as on screen. Only this process's windows go
/// in, front to back over the canvas: sheets and the Settings window land in the shot, the menu
/// bar, the desktop and other apps do not.
enum MacSelfCapture {
    /// The canvas in the top-left global space, set by the window configurator.
    static var canvas: CGRect?

    static func captureIfRequested(scene: ShotScene) {
        guard let dir = ProcessInfo.processInfo.environment["PUNKTFUNK_SHOT_SELFCAPTURE"],
              !dir.isEmpty else { return }
        let outDir = URL(fileURLWithPath: (dir as NSString).expandingTildeInPath, isDirectory: true)
        try? FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)
        let url = outDir.appendingPathComponent("\(ShotDevice.mac.id)-\(scene.name).png")
        let pid = ProcessInfo.processInfo.processIdentifier
        let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID)
            as? [[String: Any]] ?? []
        let ids = windows.filter { ($0[kCGWindowOwnerPID as String] as? pid_t) == pid }
            .compactMap { $0[kCGWindowNumber as String] as? CGWindowID }
        // The list holds raw window numbers in pointer slots; bridged NSNumbers yield no image.
        var slots = ids.map { UnsafeRawPointer(bitPattern: UInt($0)) }
        if let list = CFArrayCreate(nil, &slots, slots.count, nil),
           let shot = CGImage(windowListFromArrayScreenBounds: canvas ?? .null,
                              windowArray: list, imageOption: [.bestResolution]),
           let flat = flatten(shot),
           let dest = CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil) {
            CGImageDestinationAddImage(dest, flat, nil)
            CGImageDestinationFinalize(dest)
            print("PF_SHOT_SAVED \(url.path) \(flat.width)x\(flat.height)px")
        } else {
            print("PF_SHOT_CAPTURE_FAILED scene=\(scene.name) windows=\(ids.count)")
        }
        fflush(stdout)
        exit(0)
    }

    /// A window's round corners leave transparent pixels; fill them with the window background
    /// they sit on, so the image is opaque.
    private static func flatten(_ image: CGImage) -> CGImage? {
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let ctx = CGContext(data: nil, width: image.width, height: image.height,
                                  bitsPerComponent: 8, bytesPerRow: 0, space: space,
                                  bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
        else { return nil }
        var fill = NSColor.black.cgColor
        NSApp.effectiveAppearance.performAsCurrentDrawingAppearance {
            fill = NSColor.windowBackgroundColor.cgColor
        }
        let rect = CGRect(x: 0, y: 0, width: image.width, height: image.height)
        ctx.setFillColor(fill)
        ctx.fill(rect)
        ctx.draw(image, in: rect)
        return ctx.makeImage()
    }
}
#endif

#if os(iOS)
/// Orientation lock for the requested scene (landscape for the stream hero, portrait for chrome).
/// Requires the app to allow those orientations in Info.plist — it does, for both.
private struct IOSOrientationConfigurator: UIViewControllerRepresentable {
    let orientation: ShotOrientation

    func makeUIViewController(context: Context) -> ShotOrientationController {
        ShotOrientationController(mask: mask)
    }

    func updateUIViewController(_ vc: ShotOrientationController, context: Context) {
        vc.mask = mask
        vc.applyGeometry()
    }

    private var mask: UIInterfaceOrientationMask {
        orientation == .landscape ? .landscapeRight : .portrait
    }
}

/// Asks the window scene to rotate, from a place where there IS a window.
///
/// The previous version made the request inside `updateUIViewController`, where `view.window` is
/// still nil: SwiftUI makes exactly one update pass for a representable mounted as a `.background`,
/// before the hierarchy is in a window, so the `guard` fell through and nothing ever asked again.
/// Every scene declared `.landscape` — the stream hero and the trust card — was therefore captured
/// in PORTRAIT at the portrait App Store size. Overriding `supportedInterfaceOrientations` as well
/// keeps the scene from rotating back if the simulator reports a device orientation change.
final class ShotOrientationController: UIViewController {
    var mask: UIInterfaceOrientationMask

    init(mask: UIInterfaceOrientationMask) {
        self.mask = mask
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not from a nib") }

    override var supportedInterfaceOrientations: UIInterfaceOrientationMask { mask }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        applyGeometry()
    }

    func applyGeometry() {
        // `view.window` once mounted; the connected-scene lookup covers the first update pass,
        // which still runs before this controller is in a window.
        let scene = view.window?.windowScene
            ?? UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
        guard let scene else { return }
        // Report a refusal instead of silently shipping the wrong orientation — that is exactly
        // how every landscape scene went out as a portrait PNG for as long as it did.
        scene.requestGeometryUpdate(.iOS(interfaceOrientations: mask)) { error in
            print("PF_SHOT_ORIENTATION_REFUSED \(error.localizedDescription)")
            fflush(stdout)
        }
        setNeedsUpdateOfSupportedInterfaceOrientations()
    }
}
#endif
#endif
