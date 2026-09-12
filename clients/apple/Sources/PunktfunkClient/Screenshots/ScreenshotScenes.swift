// App Store screenshot scenes — the actual screens we render, each wired with mock data so it
// looks populated without a live host. Every scene is built from the REAL app views (HomeView,
// SettingsView, PairSheet, TrustCardView) so the screenshots track the shipping UI; only the
// live stream is faked (StreamView needs a real punktfunk/1 connection — see ShotStreamHero).

#if DEBUG
import PunktfunkKit
import SwiftUI

/// One screen to capture: a name (→ file suffix), the canvas orientation, a color scheme, and a
/// factory that builds the populated view on the main actor.
struct ShotScene {
    let name: String
    let orientation: ShotOrientation
    let colorScheme: ColorScheme
    /// macOS: the canvas without window chrome, as the app runs a session full screen. Every
    /// other scene is the app's own titled window.
    var macFullScreen = false
    let make: @MainActor () -> AnyView
}

@MainActor
enum ShotScenes {
    static var all: [ShotScene] {
        var scenes: [ShotScene] = [
            ShotScene(name: "01-stream", orientation: .landscape, colorScheme: .dark,
                      macFullScreen: true) {
                AnyView(ShotStreamHero())
            },
            ShotScene(name: "02-hosts", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotHome())
            },
            ShotScene(name: "03-pair", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotPair())
            },
            ShotScene(name: "04-trust", orientation: .landscape, colorScheme: .dark) {
                AnyView(ShotTrust())
            },
            ShotScene(name: "05-settings", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotSettings())
            },
            // 06–10 are the iOS/macOS console-shell block below; the library is cross-platform
            // (tvOS renders the same coverflow), hence the number above that range.
            ShotScene(name: "11-library", orientation: .landscape, colorScheme: .dark) {
                AnyView(ShotLibrary())
            },
            // The grid arrangement, and the view/sort bar FOCUSED — the desktop shipped a
            // mis-sized bar wash precisely because no shot ever showed the bar with focus.
            ShotScene(name: "11b-library-grid", orientation: .landscape, colorScheme: .dark) {
                AnyView(ShotLibrary(arrangement: .grid))
            },
            ShotScene(name: "11c-library-bar", orientation: .landscape, colorScheme: .dark) {
                AnyView(ShotLibrary(arrangement: .shelf, barFocused: true))
            },
            // The Collections tiles (group by platform), as "start in collections" opens them.
            ShotScene(name: "11d-collections", orientation: .landscape, colorScheme: .dark) {
                AnyView(ShotLibrary(collections: true))
            },
            // A title's Options menu (X) over the shelf.
            ShotScene(name: "11e-library-options", orientation: .landscape, colorScheme: .dark) {
                AnyView(ShotLibrary(options: true))
            },
            // The launch hold, settled: what the player looks at from the tap until the game is
            // actually running.
            ShotScene(name: "13-launch-hold", orientation: .landscape, colorScheme: .dark) {
                AnyView(ShotLaunchHold())
            },
            // The same screen ARRIVING, on a loop: the cover leaves its shelf tile every few
            // seconds so the flight can be watched rather than guessed at from a still.
            ShotScene(name: "13b-launch-hold-flight", orientation: .landscape, colorScheme: .dark) {
                AnyView(ShotLaunchHold(flight: true))
            },
        ]
        #if os(iOS) || os(macOS)
        // The gamepad-mode console screens (no tvOS — native focus engine there). Dev-only shots
        // for eyeballing the Liquid Glass host tiles + settings rows.
        scenes += [
            ShotScene(name: "06-gamepad-home", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotGamepadHome())
            },
            ShotScene(name: "07-gamepad-settings", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotGamepadSettings())
            },
            ShotScene(name: "08-gamepad-addhost", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotGamepadAddHost())
            },
            // The keyboard tray up, with the edited row seated above the keys (set
            // PUNKTFUNK_SHOT_EDITING=address to type into a field other than the name).
            ShotScene(name: "08b-gamepad-addhost-typing", orientation: .landscape, colorScheme: .dark) {
                AnyView(ShotGamepadAddHost())
            },
            ShotScene(name: "09-connecting", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotConnect(kind: .connecting))
            },
            ShotScene(name: "09b-waking", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotConnect(kind: .waking))
            },
            ShotScene(name: "09c-wake-timed-out", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotConnect(kind: .timedOut))
            },
            // The default-UI presentation (Liquid Glass modal over the touch grid) of the same phases.
            ShotScene(name: "09d-connecting-modal", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotConnect(kind: .connecting, gamepadUI: false))
            },
            ShotScene(name: "09e-waking-modal", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotConnect(kind: .waking, gamepadUI: false))
            },
            ShotScene(name: "09f-wake-timed-out-modal", orientation: .natural, colorScheme: .dark) {
                AnyView(ShotConnect(kind: .timedOut, gamepadUI: false))
            },
            // FEEL THE GAME — the controller test panel with injected pads. Gated with the
            // console block because ControllerTestView doesn't build on tvOS, not because it
            // is a console screen. Landscape like the rest of the store set: the app is built
            // for horizontal use, so the two pads sit as side-by-side columns (see the scene).
            ShotScene(name: "12-controllers", orientation: .landscape, colorScheme: .dark) {
                AnyView(ShotControllers())
            },
        ]
        #endif
        scenes.append(ShotScene(name: "10-edithost", orientation: .natural, colorScheme: .dark) {
            AnyView(ShotEditHost())
        })
        return scenes
    }
}

// MARK: - Mock data

@MainActor
enum ShotMock {
    // Stable ids so the store, the adverts and the profile bindings all point at the same things
    // across every scene and every run.
    static let battlestationID = UUID(uuidString: "5B0D1E00-0000-4000-8000-000000000001")!
    static let livingRoomID = UUID(uuidString: "5B0D1E00-0000-4000-8000-000000000002")!
    static let workshopID = UUID(uuidString: "5B0D1E00-0000-4000-8000-000000000003")!
    static let officeID = UUID(uuidString: "5B0D1E00-0000-4000-8000-000000000004")!
    static let editingID = UUID(uuidString: "5B0D1E00-0000-4000-8000-000000000005")!
    static let bedroomID = UUID(uuidString: "5B0D1E00-0000-4000-8000-000000000006")!

    static let hdrProfileID = "a71c4e0d9f22"
    static let couchProfileID = "3e88b107c4da"

    /// The catalog the host cards read their chips and pinned cards from. Seeded once, on the
    /// first store build — `ProfileStore` is a singleton, and in shot mode its write-back is
    /// suppressed, so this never reaches a real user's catalog.
    static func installProfiles() {
        guard !profilesInstalled else { return }
        profilesInstalled = true
        ProfileStore.shared.debugSet([
            StreamProfile(name: "4K HDR", id: hdrProfileID, accent: "#8B7BF7"),
            StreamProfile(name: "Couch 1080p", id: couchProfileID, accent: "#4FD1A5"),
        ])
    }

    private static var profilesInstalled = false

    /// A populated saved-host grid: the most-recent host bound to a profile (its chip), a second
    /// paired machine, and one asleep box we hold a MAC for (so its card offers Wake-on-LAN). OS
    /// chains give every tile its real vendor mark instead of a letter monogram.
    ///
    /// No PINNED host+profile card: it renders a second tile for the SAME host, which is the
    /// feature working as designed but reads as a duplicate to anyone meeting the app in a store
    /// listing. The binding chip carries the profile story on its own.
    static func hostStore() -> HostStore {
        installProfiles()
        let store = HostStore()
        store.hosts = [
            StoredHost(
                id: battlestationID, name: "Battlestation", address: "192.168.1.20", port: 9777,
                pinnedSHA256: fingerprint, lastConnected: Date().addingTimeInterval(-420),
                macAddresses: ["a4:b1:c2:d3:e4:f5"], profileID: hdrProfileID,
                osChain: "windows/11"),
            StoredHost(
                id: livingRoomID, name: "Living Room PC", address: "192.168.1.41", port: 9777,
                pinnedSHA256: hostFingerprint(1), lastConnected: Date().addingTimeInterval(-86_400),
                macAddresses: ["b8:27:eb:11:22:33"], osChain: "linux/fedora/bazzite"),
            StoredHost(
                id: officeID, name: "Office NUC", address: "192.168.1.33", port: 9777,
                pinnedSHA256: hostFingerprint(4), lastConnected: Date().addingTimeInterval(-259_200),
                profileID: couchProfileID, osChain: "linux/ubuntu"),
            StoredHost(
                id: workshopID, name: "Workshop", address: "10.0.0.7", port: 9777,
                pinnedSHA256: hostFingerprint(2), macAddresses: ["de:ad:be:ef:00:07"],
                osChain: "linux/arch"),
            StoredHost(
                id: editingID, name: "Editing Rig", address: "192.168.1.62", port: 9777,
                pinnedSHA256: hostFingerprint(5), lastConnected: Date().addingTimeInterval(-604_800),
                osChain: "linux/nobara"),
            StoredHost(
                id: bedroomID, name: "Bedroom Mini", address: "192.168.1.77", port: 9777,
                pinnedSHA256: hostFingerprint(6), macAddresses: ["00:1a:2b:3c:4d:5e"],
                osChain: "windows/11"),
        ]
        // Which cards read ONLINE. Seeded, because presence is a live probe and a capture has no
        // network — the same three hosts `discovery()` advertises, so both halves agree.
        store.debugSetProbedOnline([battlestationID, livingRoomID, officeID])
        return store
    }

    /// Discovery, seeded rather than live. Three saved hosts advertise (matching the seeded
    /// reachable set above), "Workshop" stays quiet so the grid shows an asleep machine, and one
    /// genuinely new host populates the "On this network" section.
    ///
    /// A live browse made the shot non-deterministic AND leaked whatever was on the capturing
    /// machine's LAN into the App Store listing.
    static func discovery() -> HostDiscovery {
        let discovery = HostDiscovery()
        discovery.debugSet([
            HostDiscovery.debugAdvert(
                id: "battlestation", name: "Battlestation", host: "192.168.1.20",
                fingerprintHex: fingerprint.hexLower, macAddresses: ["a4:b1:c2:d3:e4:f5"],
                osChain: "windows/11"),
            HostDiscovery.debugAdvert(
                id: "living-room", name: "Living Room PC", host: "192.168.1.41",
                fingerprintHex: hostFingerprint(1).hexLower, macAddresses: ["b8:27:eb:11:22:33"],
                osChain: "linux/fedora/bazzite"),
            HostDiscovery.debugAdvert(
                id: "office-nuc", name: "Office NUC", host: "192.168.1.33",
                fingerprintHex: hostFingerprint(4).hexLower, osChain: "linux/ubuntu"),
            HostDiscovery.debugAdvert(
                id: "studio", name: "Studio PC", host: "192.168.1.58",
                fingerprintHex: hostFingerprint(3).hexLower, requiresPairing: true, allowsTofu: false,
                osChain: "windows/11"),
        ])
        return discovery
    }

    static let host = StoredHost(
        id: battlestationID, name: "Battlestation", address: "192.168.1.20", port: 9777,
        pinnedSHA256: fingerprint, osChain: "windows/11")

    /// What the pairing sheet calls THIS device. Taken from the platform, not from
    /// `UIDevice.current.name` — on a capture simulator that is the harness's own throwaway name
    /// (`pf-shot-iphone-6.9` went out on the store listing that way).
    static var clientDeviceName: String {
        #if os(tvOS)
        "Apple TV"
        #elseif os(macOS)
        "MacBook Pro"
        #else
        UIDevice.current.userInterfaceIdiom == .pad ? "iPad Pro" : "iPhone"
        #endif
    }

    /// A believable shelf for the library coverflow. Decoded rather than constructed:
    /// `GameEntry`'s memberwise init is internal to PunktfunkKit, and Codable is its public
    /// construction surface. The `shot://art/…` posters are answered by [`ShotPosterArt.source`]
    /// (drawn at capture time), so the shot stays offline; the Steam launcher entry stays artless
    /// by design and renders its brand mark.
    static let games: [GameEntry] = {
        let json = """
        [
          {"id": "custom:aurora", "store": "custom", "title": "Aurora Drift",
           "platform": "PS3", "release_year": 2009, "developer": "Nine Lanterns",
           "genres": ["Racing"], "art": {"portrait": "shot://art/aurora"}},
          {"id": "steam:starfall", "store": "steam", "title": "Starfall Vale",
           "platform": "PC", "release_year": 2024, "developer": "Meridian Foundry",
           "genres": ["Action", "Adventure"],
           "art": {"portrait": "shot://art/starfall"}},
          {"id": "heroic:neon", "store": "heroic", "title": "Neon Circuit",
           "platform": "PC", "art": {"portrait": "shot://art/neon"}},
          {"id": "gog:ember", "store": "gog", "title": "Ember Peaks",
           "art": {"portrait": "shot://art/ember"}},
          {"id": "steam:launcher", "store": "steam", "title": "Steam", "art": {},
           "role": "launcher", "icon": "steam"}
        ]
        """
        return (try? JSONDecoder().decode([GameEntry].self, from: Data(json.utf8))) ?? []
    }()

    /// A plausible-looking 32-byte SHA-256 for the trust card / pin lock glyphs.
    static let fingerprint = hostFingerprint(0)

    /// Distinct per host — `StoredHost.matches` prefers a fingerprint comparison, so sharing one
    /// across the mock grid made a single advert light up every card.
    static func hostFingerprint(_ seed: Int) -> Data {
        Data((0..<32).map { UInt8((($0 &* 37) &+ 0x1d &+ (seed &* 91)) & 0xff) })
    }
}

// MARK: - Home

private struct ShotHome: View {
    @StateObject private var store = ShotMock.hostStore()
    @StateObject private var model = SessionModel()
    @StateObject private var discovery = ShotMock.discovery()

    var body: some View {
        #if os(macOS)
        HomeView(
            store: store, model: model, discovery: discovery,
            showAddHost: .constant(false), pairingTarget: .constant(nil),
            speedTestTarget: .constant(nil), libraryTarget: .constant(nil),
            connect: { _, _ in }, connectDiscovered: { _ in },
            onPaired: { _, _ in }, onLaunchTitle: { _, _ in }, onConnectShelf: { _ in },
            wake: { _ in })
        #else
        HomeView(
            store: store, model: model, discovery: discovery,
            showAddHost: .constant(false), pairingTarget: .constant(nil),
            speedTestTarget: .constant(nil), libraryTarget: .constant(nil),
            showSettings: .constant(false),
            connect: { _, _ in }, connectDiscovered: { _ in },
            onPaired: { _, _ in }, onLaunchTitle: { _, _ in }, onConnectShelf: { _ in },
            wake: { _ in })
        #endif
    }
}

// MARK: - Library

/// The library with the mock shelf — the store listing's PICK & PLAY frame. The real
/// `LibraryConsoleView` (coverflow or grid), no network: `ShotPosterArt` answers the mock entries' art immediately,
/// so the cards swing in already carrying posters (the entrance waits on art settling).
private struct ShotLibrary: View {
    var arrangement: LibraryArrangement?
    var barFocused = false
    var collections = false
    var options = false

    /// Dev knobs for driving the console library on a Mac from the shot harness: with
    /// `PUNKTFUNK_FAKE_LIBRARY` set the scene shows that catalog (a real multi-row grid, the
    /// shared collate vectors file works) instead of the five-title mock; with
    /// `PUNKTFUNK_SHOT_INTERACTIVE=1` the screen owns the controller/keyboard, so arrow keys walk
    /// the grid exactly as the pad would.
    private var games: [GameEntry] {
        let env = ProcessInfo.processInfo.environment
        guard let path = env["PUNKTFUNK_FAKE_LIBRARY"], !path.isEmpty,
              let data = FileManager.default.contents(atPath: path)
        else { return ShotMock.games }
        struct Wrapped: Decodable { let library: [GameEntry] }
        let decoder = JSONDecoder()
        if let list = try? decoder.decode([GameEntry].self, from: data) { return list.launchersFirst }
        if let wrapped = try? decoder.decode(Wrapped.self, from: data) { return wrapped.library.launchersFirst }
        return ShotMock.games
    }

    private var interactive: Bool {
        ProcessInfo.processInfo.environment["PUNKTFUNK_SHOT_INTERACTIVE"] == "1"
    }

    var body: some View {
        LibraryConsoleView(
            games: games, artLoader: ShotPosterArt.source,
            onLaunch: { _ in }, onDismiss: {},
            // The mock has a clipboard action, and a game up, so the Options menu shows both
            // of the rows a real shelf offers.
            onCopyLink: { _ in }, hostName: "Battlestation",
            nowPlaying: "Hollow Knight", onConnect: {},
            controllerActive: interactive,
            arrangementOverride: arrangement, barFocusedInitially: barFocused,
            startInCollectionsOverride: collections, optionsInitially: options)
    }
}

// MARK: - Launch hold

/// The launch hold over the real shelf.
///
/// The shelf underneath is the actual `LibraryConsoleView`, not a backdrop image, which is what
/// makes the flight real: its posters publish their rects to `TileFrames` exactly as they do in
/// the app, so the cover here leaves the tile it is drawn in rather than a rect this scene made
/// up. `flight` replays the arrival every few seconds — the still frame cannot show it.
private struct ShotLaunchHold: View {
    var flight = false
    /// Which mock title launches. Its tile has to be on screen for a rect to exist.
    private let launched = ShotMock.games.first { $0.id == "steam:starfall" } ?? ShotMock.games[0]

    @State private var showing = false
    /// Bumped per replay, for the same reason the app counts launches: without a fresh
    /// identity the second hold inherits the first one's state and never flies.
    @State private var cycle = 0

    var body: some View {
        ZStack {
            // The GRID for the flight: the coverflow's centred card already sits where the hold
            // puts it, so a cover leaving it barely travels. A grid tile is small and off to one
            // side, which is the move the animation is actually for.
            ShotLibrary(arrangement: flight ? .grid : nil)
            if showing {
                LaunchHoldView(
                    entry: launched, host: nil, connecting: true,
                    sourceRect: TileFrames.rect(launched.id),
                    artOverride: ShotPosterArt.source,
                    onShow: {})
                    .id(cycle)
                    .transition(.asymmetric(insertion: .identity, removal: .opacity))
            }
        }
        .animation(.easeInOut(duration: 0.3), value: showing)
        .task {
            guard flight else {
                // The still: the hold is the whole frame, so there is nothing to wait for.
                showing = true
                return
            }
            // Let the shelf's own entrance finish first, so the tile the cover flies out of is
            // where it will finally sit and the only thing moving after that is the cover.
            try? await Task.sleep(nanoseconds: 2_600_000_000)
            showing = true
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_600_000_000)
                showing = false
                try? await Task.sleep(nanoseconds: 900_000_000)
                cycle += 1
                showing = true
            }
        }
    }
}

// MARK: - Gamepad-mode console screens (dev-only glass preview)

#if os(iOS) || os(macOS)
private struct ShotGamepadHome: View {
    @StateObject private var store = ShotMock.hostStore()
    @StateObject private var model = SessionModel()
    @StateObject private var discovery = ShotMock.discovery()
    @StateObject private var waker = HostWaker()

    var body: some View {
        GamepadHomeView(
            store: store, model: model, discovery: discovery,
            libraryTarget: .constant(nil), pairingTarget: .constant(nil),
            onPaired: { _, _ in }, waker: waker,
            connect: { _, _ in }, connectDiscovered: { _ in }, launchTitle: { _, _ in },
            connectShelf: { _ in }, wakeOnly: { _ in })
    }
}

private struct ShotGamepadSettings: View {
    @StateObject private var store = ShotMock.hostStore()

    var body: some View { GamepadSettingsView(store: store) }
}

private struct ShotGamepadAddHost: View {
    var body: some View { GamepadAddHostView(onAdd: { _ in }) }
}

/// The unified connect overlay (the real `ConnectOverlay`) in each phase — instant "Connecting…"
/// feedback, the "Waking…" wait, and the wake-timed-out prompt. `gamepadUI` picks the presentation:
/// the console's full-screen aurora takeover over the gamepad home, or the default UI's Liquid Glass
/// modal over the touch host grid.
private struct ShotConnect: View {
    enum Kind { case connecting, waking, timedOut }
    let kind: Kind
    var gamepadUI = true

    @StateObject private var store = ShotMock.hostStore()
    @StateObject private var model = SessionModel()
    @StateObject private var discovery = ShotMock.discovery()
    @StateObject private var waker = HostWaker()

    var body: some View {
        backdrop
            .overlay {
                ConnectOverlay(
                    connectingHostName: kind == .connecting ? "Battlestation" : nil,
                    waker: waker,
                    gamepadUI: gamepadUI,
                    onCancelConnect: {})
            }
            .onAppear {
                switch kind {
                case .connecting:
                    break
                case .waking:
                    waker.debugSet(.init(
                        hostID: store.hosts.first?.id ?? UUID(),
                        hostName: "Battlestation", connectsAfter: true, seconds: 14))
                case .timedOut:
                    waker.debugSet(.init(
                        hostID: store.hosts.first?.id ?? UUID(),
                        hostName: "Battlestation", connectsAfter: true, seconds: 90, timedOut: true))
                }
            }
    }

    @ViewBuilder private var backdrop: some View {
        if gamepadUI {
            GamepadHomeView(
                store: store, model: model, discovery: discovery,
                libraryTarget: .constant(nil), pairingTarget: .constant(nil),
            onPaired: { _, _ in }, waker: waker,
                connect: { _, _ in }, connectDiscovered: { _ in }, launchTitle: { _, _ in },
            connectShelf: { _ in }, wakeOnly: { _ in })
        } else {
            ShotHome()
        }
    }
}

// MARK: - Controllers (the pads the store listing names)

/// The FEEL THE GAME frame: the controller test panel rendering the two pads the listing talks
/// about. A GCController cannot be constructed, so the panel draws injected `ShotPad`s — the
/// DualSense leads with the feedback surface (adaptive-trigger effects, rumble backend, lightbar
/// + player LEDs), the Xbox pad carries the input readout, frozen mid-game.
private struct ShotControllers: View {
    var body: some View {
        #if os(macOS)
        // The app presents the panel as a sheet on Settings → Controllers, at its minimum size.
        ShotMacSettingsWindow(
            tab: "Controllers",
            sheet: AnyView(ControllerTestView(shotPads: Self.pads).frame(width: 420, height: 540)))
        #else
        // Landscape canvas: one column per pad, so neither story is cut by the short height —
        // the DualSense feedback surface left, the Xbox live-input readout right.
        HStack(spacing: 0) {
            ControllerTestView(shotPads: [Self.pads[0]])
            ControllerTestView(shotPads: [Self.pads[1]])
        }
        #endif
    }

    /// Transport/battery/player ride in `detail` — the panel has no dedicated battery row.
    /// Each pad shows a different half of the panel: the DualSense skips the input card (the
    /// effect grid is the marketing point), the Xbox pad skips rumble and shows the readout.
    static let pads: [ControllerTestView.ShotPad] = [
        .init(
            name: "DualSense Wireless Controller",
            detail: "Bluetooth · 85% · Player 1",
            isDualSense: true, hasAdaptiveTriggers: true, hasLight: true,
            rumbleBackend: "DualSense HID · Bluetooth"),
        .init(
            name: "Xbox Wireless Controller",
            detail: "Bluetooth · 60% · Player 2",
            isDualSense: false, hasAdaptiveTriggers: false, hasLight: false,
            input: .init(
                leftStick: .init(x: -0.31, y: 0.54),
                rightStick: .init(x: 0.72, y: -0.16),
                leftTrigger: 0.08, rightTrigger: 0.62,
                buttons: [
                    ("A", true), ("B", false), ("X", false), ("Y", false),
                    ("LB", false), ("RB", true), ("L3", false), ("R3", false),
                    ("Menu", false), ("Opts", false),
                    ("↑", false), ("↓", false), ("←", false), ("→", false),
                ])),
    ]
}
#endif

// MARK: - Edit host (add/edit sheet with the Wake-on-LAN MAC field)

private struct ShotEditHost: View {
    var body: some View {
        ZStack {
            ShotHome().blur(radius: 24).overlay(Color.black.opacity(0.45))
            AddHostSheet(
                existing: StoredHost(
                    name: "Battlestation", address: "192.168.1.20", port: 9777,
                    pinnedSHA256: ShotMock.fingerprint, macAddresses: ["a4:b1:c2:d3:e4:f5"]),
                onSave: { _ in })
                #if os(macOS)
                .clipShape(RoundedRectangle(cornerRadius: 12))
                .shadow(radius: 40, y: 16)
                #endif
        }
    }
}

// MARK: - Settings

private struct ShotSettings: View {
    var body: some View {
        #if os(macOS)
        ShotMacSettingsWindow()
        #elseif os(iOS)
        // SettingsView owns its NavigationSplitView (sidebar + detail) and Done button, so it is
        // rendered directly — a wrapping NavigationStack would nest a split view in a stack. Open
        // on Display rather than the bare category list: resolution, frame rate, bitrate, HDR and
        // codec are what someone reads a streaming app's settings shot to find out.
        SettingsView(initialCategory: .display)
        #else
        NavigationStack { SettingsView() }
        #endif
    }
}

#if os(macOS)
/// The host grid with the app's real Settings window in front, as ⌘, opens it: the toolbar tabs
/// only exist in the Settings scene. `tab` picks a toolbar tab by its label; `sheet` rides on the
/// Settings window the way Test Controller does.
private struct ShotMacSettingsWindow: View {
    var tab: String?
    var sheet: AnyView?
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        ShotHome().task {
            // Once the capture window has settled on the canvas.
            try? await Task.sleep(nanoseconds: 2_500_000_000)
            openSettings()
            try? await Task.sleep(nanoseconds: 500_000_000)
            arrange()
        }
    }

    private func arrange() {
        let windows = NSApp.windows
        let canvas = ShotDevice.mac.points(.natural)
        guard let main = windows.first(where: { $0.frame.size == canvas }),
              let settings = windows.first(where: {
                  $0.identifier?.rawValue.contains("Settings") == true
              })
        else { return }
        settings.setFrameOrigin(NSPoint(
            x: main.frame.midX - settings.frame.width / 2,
            y: main.frame.midY - settings.frame.height / 2))
        if let tab, let item = settings.toolbar?.items.first(where: { $0.label == tab }),
           let action = item.action {
            NSApp.sendAction(action, to: item.target, from: item)
        }
        // An inactive app draws grey controls.
        NSApp.activate(ignoringOtherApps: true)
        settings.makeKeyAndOrderFront(nil)
        if let sheet {
            settings.beginSheet(NSWindow(contentViewController: NSHostingController(
                rootView: sheet.tint(.brand))))
        }
    }
}
#endif

// MARK: - Pair (PIN ceremony)

private struct ShotPair: View {
    /// The PIN as the host's web console shows it, and a device name that doesn't depend on what
    /// the capture simulator happens to be called.
    private var sheet: some View {
        PairSheet(
            host: ShotMock.host, shotPIN: "418 306",
            shotClientName: ShotMock.clientDeviceName, onPaired: { _ in })
    }

    var body: some View {
        #if os(iOS)
        // PRESENT it, don't rebuild it. `PairSheet` is a bottom sheet on iOS — it carries its own
        // `.presentationDetents([.medium, .large])` and the system's Liquid Glass background, both
        // of which only exist inside a real `.sheet`. Composed into a ZStack instead (what this
        // scene used to do), the detents were inert, the grouped Form stretched to the full height
        // of the screen, and the capture was a thin strip of content over a huge black void.
        ShotHome()
            .sheet(isPresented: .constant(true)) {
                // Pinned to one detent. The sheet ships `[.medium, .large]` so it can grow over
                // the keyboard, and the resting height leaves a wide empty band between the form
                // and the button row; a capture wants the snug version.
                sheet.presentationDetents([.fraction(0.52)])
            }
        #elseif os(tvOS)
        // tvOS pushes the ceremony as a full screen (HomeView's `navigationDestination`).
        NavigationStack { sheet }
        #else
        // macOS: the window-modal sheet the app presents. It is a window of its own, which the
        // driver's capture of the window's rect takes in.
        ShotHome().sheet(isPresented: .constant(true)) { sheet }
        #endif
    }
}

// MARK: - Trust (TOFU card over the blurred live stream)

private struct ShotTrust: View {
    var body: some View {
        ZStack {
            ShotDesktopFrame()
                .blur(radius: 32)
                .overlay(Color.black.opacity(0.45))
            TrustCardView(
                fingerprint: ShotMock.fingerprint, hostName: "Battlestation",
                onCancel: {}, onTrust: {}, onPairInstead: {})
        }
    }
}

// MARK: - Stream hero

/// The marketing hero: a stand-in streamed frame with the real glass HUD chip on top.
/// StreamView can't render here (it needs a live punktfunk/1 connection), so the frame is
/// synthetic — set `PUNKTFUNK_SHOT_HERO=/path/to/frame.png` to drop in a real captured frame.
/// The frame fills the display; the HUD stays inside the safe area. The status bar and home
/// indicator hide, as they do for a live session.
private struct ShotStreamHero: View {
    @Environment(\.displayScale) private var scale

    var body: some View {
        GeometryReader { geo in
            #if os(macOS)
            // The mac canvas: the stream window overhangs it by a point on each side.
            let px = ShotDevice.mac.pixels(.landscape)
            ShotHUD(width: px.w, height: px.h)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
            #else
            // The whole display in pixels: the safe-area frame plus its insets, at backing scale.
            let insets = geo.safeAreaInsets
            ShotHUD(
                width: Int(((geo.size.width + insets.leading + insets.trailing) * scale).rounded()),
                height: Int(((geo.size.height + insets.top + insets.bottom) * scale).rounded()))
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
            #endif
        }
        .background { ShotDesktopFrame() }
        .background(Color.black.ignoresSafeArea())
        #if os(iOS)
        .statusBarHidden(true)
        .persistentSystemOverlays(.hidden)
        #endif
    }
}

/// A faithful copy of StreamHUDView's overlay (which needs a live PunktfunkConnection for the
/// mode line), reusing the app's real `.glassBackground`. The mode line is the capturing
/// display's own pixel size and max refresh rate.
private struct ShotHUD: View {
    let width: Int
    let height: Int

    /// 812.4 Mb/s at 5120×1440@240: the bitrate scales with pixel rate at this density.
    private static let bitsPerPixelFrame = 0.459

    /// The display's max refresh rate. `PUNKTFUNK_SHOT_FPS` overrides it: a Simulator reports
    /// 60 even for a 120 Hz device.
    private var fps: Int {
        if let fps = ProcessInfo.processInfo.environment["PUNKTFUNK_SHOT_FPS"].flatMap(Int.init) {
            return fps
        }
        #if os(macOS)
        return NSScreen.main?.maximumFramesPerSecond ?? 60
        #else
        return UIApplication.shared.connectedScenes
            .compactMap { ($0 as? UIWindowScene)?.screen.maximumFramesPerSecond }.first ?? 60
        #endif
    }

    private var modeLine: String {
        let mbps = Double(width * height * fps) * Self.bitsPerPixelFrame / 1_000_000
        return "\(width)×\(height)@\(fps)  \(fps) fps  " + String(format: "%.1f Mb/s", mbps)
    }

    var body: some View {
        VStack(alignment: .trailing, spacing: 4) {
            HStack(spacing: 6) {
                Circle().fill(Color.accentColor).frame(width: 7, height: 7)
                Text(modeLine)
                    .font(.system(.caption, design: .monospaced))
            }
            Text("end-to-end 2.9 ms p50 · 3.8 p95 · capture→on-glass")
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(.secondary)
            Text("= host+network 1.3 + decode 0.7 + display 0.9")
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(.secondary)
            #if os(macOS)
            Text("⌘⎋ releases the mouse")
                .font(.geist(11, relativeTo: .caption2)).foregroundStyle(.secondary)
            #elseif os(tvOS)
            Text("Press Menu to disconnect")
                .font(.geist(12, relativeTo: .caption)).foregroundStyle(.secondary)
            #endif
        }
        .padding(10)
        .glassBackground(RoundedRectangle(cornerRadius: 10))
        .padding(10)
    }
}

/// A synthetic "streamed frame" — a synthwave scene that reads as game content without shipping
/// any real art. Replaced wholesale when `PUNKTFUNK_SHOT_HERO` points at a real PNG. Fills the
/// whole display, safe area included, like the real stream.
private struct ShotDesktopFrame: View {
    var body: some View {
        Group {
            if let image = Self.overrideImage {
                // Fill without growing the layout: the clear view sets the size, the image crops.
                Color.clear.overlay { image.resizable().scaledToFill() }.clipped()
            } else {
                synthetic
            }
        }
        .ignoresSafeArea()
    }

    private var synthetic: some View {
        ZStack {
            LinearGradient(
                colors: [
                    Color(red: 0.05, green: 0.02, blue: 0.16),
                    Color(red: 0.35, green: 0.05, blue: 0.42),
                    Color(red: 0.95, green: 0.30, blue: 0.35),
                    Color(red: 0.99, green: 0.62, blue: 0.32),
                ],
                startPoint: .top, endPoint: .bottom)
            Canvas { ctx, size in
                let horizon = size.height * 0.52
                // Sun.
                let sunR = size.height * 0.20
                let sun = CGRect(x: size.width / 2 - sunR, y: horizon - sunR * 1.6,
                                 width: sunR * 2, height: sunR * 2)
                ctx.fill(Path(ellipseIn: sun),
                         with: .linearGradient(
                            Gradient(colors: [Color(red: 1, green: 0.95, blue: 0.5),
                                              Color(red: 1, green: 0.35, blue: 0.45)]),
                            startPoint: CGPoint(x: sun.midX, y: sun.minY),
                            endPoint: CGPoint(x: sun.midX, y: sun.maxY)))
                // Sun scanlines — clip a copy so the base context stays unclipped (GraphicsContext
                // is a value type; there is no resetClip).
                var sunCtx = ctx
                sunCtx.clip(to: Path(ellipseIn: sun))
                for i in 0..<7 {
                    let y = sun.minY + sun.height * (0.55 + Double(i) * 0.07)
                    let bar = CGRect(x: sun.minX, y: y, width: sun.width,
                                     height: sun.height * (0.012 + Double(i) * 0.006))
                    sunCtx.fill(Path(bar), with: .color(.black.opacity(0.85)))
                }
                // Perspective grid below the horizon.
                ctx.opacity = 0.55
                let cx = size.width / 2
                for col in -10...10 {
                    var p = Path()
                    p.move(to: CGPoint(x: cx, y: horizon))
                    p.addLine(to: CGPoint(x: cx + Double(col) * size.width * 0.11,
                                          y: size.height))
                    ctx.stroke(p, with: .color(Color(red: 0.6, green: 0.95, blue: 1)),
                               lineWidth: 1.5)
                }
                var row = horizon
                var step = size.height * 0.012
                while row < size.height {
                    var p = Path()
                    p.move(to: CGPoint(x: 0, y: row))
                    p.addLine(to: CGPoint(x: size.width, y: row))
                    ctx.stroke(p, with: .color(Color(red: 0.6, green: 0.95, blue: 1)),
                               lineWidth: 1.5)
                    step *= 1.32
                    row += step
                }
            }
        }
        .overlay(alignment: .bottomLeading) {
            // A small "now playing" chip so the frame reads as live content, not a wallpaper.
            HStack(spacing: 8) {
                Image(systemName: "gamecontroller.fill")
                Text("Streaming from Battlestation")
                    .font(.geist(16, .semibold, relativeTo: .callout))
            }
            .padding(.horizontal, 14).padding(.vertical, 9)
            .glassBackground(Capsule())
            .padding(18)
        }
    }

    /// `PUNKTFUNK_SHOT_HERO=/abs/path.png` → use a real captured frame as the hero background.
    static var overrideImage: Image? {
        guard let path = ProcessInfo.processInfo.environment["PUNKTFUNK_SHOT_HERO"],
              !path.isEmpty, FileManager.default.fileExists(atPath: path) else { return nil }
        #if os(macOS)
        guard let ns = NSImage(contentsOfFile: path) else { return nil }
        return Image(nsImage: ns)
        #else
        guard let ui = UIImage(contentsOfFile: path) else { return nil }
        return Image(uiImage: ui)
        #endif
    }
}
#endif
