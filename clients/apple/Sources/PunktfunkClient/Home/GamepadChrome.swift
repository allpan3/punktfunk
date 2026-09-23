// The console-style chrome the SwiftUI screens still share: the button glyph a legend wears, the
// form metrics, and the hint bar. The console itself is the Skia shell (`ConsoleView`); these
// serve the trust card's legend and the pad-navigable prompt (`GamepadPrompt`).

import PunktfunkKit
import SwiftUI
#if os(iOS) || os(macOS) || os(tvOS)
import GameController

/// The glyph a button wears in a legend: the ACTIVE controller's own (Xbox "A", DualSense ✕, …)
/// via `sfSymbolsName` while one is attached, else the glyph of the last pad this device ever saw
/// (`GamepadManager.lastKnownKind` → `GamepadGlyphs`), else the caller's generic fallback.
///
/// The middle rung is the whole point. `active` is nil whenever the pad sleeps, disconnects or
/// runs flat — and permanently under `gamepadUIMode == "always"`, which puts the console UI up
/// with no pad by design — and the fallbacks are letter glyphs, so a DualSense user's ✕/◯ legends
/// used to turn into A/B the moment the controller dozed off. The remembered kind keeps the
/// legends speaking the pad the user actually owns. The `fallback` still covers the genuinely
/// unknown case: a fresh install that has never seen a controller, and any button outside the six
/// `GamepadButtonRole` names.
///
/// On tvOS the middle rung is the Siri Remote instead: with no pad attached it is what drives the
/// screen. A role the remote has no button for yields an empty glyph, and the hint bar drops it.
///
/// @MainActor: GamepadManager is main-actor-bound (inside a View body this was implicit).
@MainActor
func buttonGlyph(
    _ button: KeyPath<GCExtendedGamepad, GCControllerButtonInput>, fallback: String
) -> String {
    let manager = GamepadManager.shared
    if let live = manager.active?.controller.extendedGamepad?[keyPath: button].sfSymbolsName {
        return live
    }
    guard let role = GamepadButtonRole(keyPath: button) else { return fallback }
    #if os(tvOS)
    return GamepadGlyphs.remoteSymbol(role) ?? ""
    #else
    return GamepadGlyphs.symbol(role, for: manager.lastKnownKind)
    #endif
}

/// Metrics for the console-style SwiftUI surfaces (the prompt, the hint bar) — one set of numbers
/// at the size the screen they are on calls for.
///
/// Three tiers, not two. The phone numbers used to serve every non-TV device, so an iPad Pro drew
/// a settings list at iPhone scale in the middle of a 13" display — the field verdict was that the
/// sizing "does not adapt to larger screens". `pad` sits between the in-hand and 10-foot sets.
///
/// Chosen from the SIZE CLASSES rather than the device idiom, so an iPad running a narrow Stage
/// Manager or Split View window correctly gets the in-hand numbers — the window is what the user
/// is reading, not the panel it sits on.
struct GamepadFormMetrics {
    /// Which set this is, for the few things that are a KIND of layout rather than a number.
    enum Tier { case phone, pad, tv }

    let tier: Tier
    let headerFont: CGFloat
    let labelFont: CGFloat
    let valueFont: CGFloat
    let iconFont: CGFloat
    let iconWidth: CGFloat
    let chevronFont: CGFloat
    let rowHPad: CGFloat
    let rowVPad: CGFloat
    let rowCorner: CGFloat
    let rowMaxWidth: CGFloat
    let detailFont: CGFloat
    /// The option band's (GamepadOptionBand) fixed stage inside a choice row.
    let bandWidth: CGFloat
    /// The settings screen's section-tab pills.
    let tabFont: CGFloat
    /// The pinned controls legend (GamepadHintBar).
    let hintGlyphFont: CGFloat
    let hintTextFont: CGFloat
    let hintPad: CGFloat

    /// In-hand: a phone, or any window narrow enough to read like one.
    static let phone = GamepadFormMetrics(
        tier: .phone,
        headerFont: 12, labelFont: 16, valueFont: 15, iconFont: 17, iconWidth: 28,
        chevronFont: 12, rowHPad: 16, rowVPad: 13, rowCorner: 14, rowMaxWidth: 620,
        detailFont: 13, bandWidth: 240,
        tabFont: 13, hintGlyphFont: 19, hintTextFont: 14, hintPad: 13)

    /// A tablet-sized window — an arm's length away rather than in the palm.
    static let pad = GamepadFormMetrics(
        tier: .pad,
        headerFont: 14, labelFont: 20, valueFont: 19, iconFont: 21, iconWidth: 34,
        chevronFont: 14, rowHPad: 20, rowVPad: 16, rowCorner: 16, rowMaxWidth: 820,
        detailFont: 16, bandWidth: 320,
        tabFont: 16, hintGlyphFont: 23, hintTextFont: 17, hintPad: 15)

    /// 10-foot.
    static let tv = GamepadFormMetrics(
        tier: .tv,
        headerFont: 17, labelFont: 23, valueFont: 21, iconFont: 24, iconWidth: 40,
        chevronFont: 16, rowHPad: 24, rowVPad: 19, rowCorner: 18, rowMaxWidth: 920,
        detailFont: 19, bandWidth: 380,
        tabFont: 17, hintGlyphFont: 27, hintTextFont: 20, hintPad: 18)

    /// What a screen gets before anything publishes a tier — and the only tier tvOS and macOS ever
    /// use (an Apple TV is always 10-foot; a Mac window is read at desk distance).
    static var platformDefault: GamepadFormMetrics {
        #if os(tvOS)
        tv
        #else
        phone
        #endif
    }

    #if os(iOS)
    /// The tier a window's size classes call for. REGULAR on both axes is the tablet case.
    static func forWindow(
        h: UserInterfaceSizeClass?, v: UserInterfaceSizeClass?
    ) -> GamepadFormMetrics {
        h == .regular && v == .regular ? .pad : .phone
    }
    #endif
}

private struct GamepadMetricsKey: EnvironmentKey {
    static let defaultValue = GamepadFormMetrics.platformDefault
}

extension EnvironmentValues {
    /// The form metrics for the screen currently drawing. Published from ContentView — the app
    /// ROOT — rather than only from `gamepadPaletteInk`, because a screen that applies that
    /// modifier itself sits ABOVE its own copy: its `@Environment` resolves against its parent, so
    /// it would read the bare default instead of its own window's tier.
    var gamepadMetrics: GamepadFormMetrics {
        get { self[GamepadMetricsKey.self] }
        set { self[GamepadMetricsKey.self] = newValue }
    }
}

/// One glyph + label cell in a hint bar.
struct GamepadHint: Identifiable {
    let glyph: String
    let text: String
    /// What tapping/clicking this cell does — the same thing its button does. Optional because a
    /// few legend cells NAME an input rather than an action ("↔ Adjust" is the stick itself;
    /// there is no single thing a tap on it could mean), and those stay inert labels.
    var action: (() -> Void)? = nil
    var id: String { glyph + text }
}

/// The pinned controls legend every gamepad screen shows bottom-leading (via `.safeAreaInset`).
/// Same font/spacing everywhere so the legend reads as system chrome, not per-screen decoration —
/// worn as a self-contained Liquid Glass pill (like the top-bar controller chip) so it floats over
/// the backdrop instead of dissolving into it.
struct GamepadHintBar: View {
    @Environment(\.gamepadInk) private var ink
    /// Sized with the screen it pins to — a legend at phone scale on a 13" iPad is the same
    /// mismatch the form rows had (see GamepadFormMetrics).
    @Environment(\.gamepadMetrics) private var metrics
    let hints: [GamepadHint]

    var body: some View {
        HStack(spacing: 18) {
            // An empty glyph names a button the input in hand doesn't have (see `buttonGlyph`).
            ForEach(hints.filter { !$0.glyph.isEmpty }) { hint in
                cell(hint)
            }
        }
        .font(.geist(metrics.hintTextFont, .semibold, relativeTo: .subheadline))
        .foregroundStyle(ink.fg(0.85))
        .padding(metrics.hintPad)
        .consoleGlass(Capsule())
        // The hairline is DECORATION and sits on top of the cells, so it must never take a touch.
        // Spelled out rather than left to defaults, because a swallowed touch in this bar is
        // invisible — the legend simply stops doing anything.
        .overlay(Capsule().strokeBorder(ink.fg(0.12), lineWidth: 1).allowsHitTesting(false))
    }

    /// A cell is a button where it has somewhere to go, and a plain label otherwise (see the type
    /// comment for why tvOS is always the latter).
    @ViewBuilder private func cell(_ hint: GamepadHint) -> some View {
        #if os(tvOS)
        label(hint)
        #else
        if let action = hint.action {
            Button(action: action) { label(hint) }
                .buttonStyle(HintCellStyle())
                .accessibilityLabel(hint.text)
        } else {
            label(hint)
        }
        #endif
    }

    private func label(_ hint: GamepadHint) -> some View {
        HStack(spacing: 7) {
            Image(systemName: hint.glyph)
                .font(.system(size: metrics.hintGlyphFont))
                .foregroundStyle(ink.fg)
            Text(hint.text)
        }
        .fixedSize() // keep glyph + label together; never truncate a hint mid-word
        // The tappable area covers the gap between glyph and label, not just their painted
        // pixels — a legend cell is small enough already.
        .contentShape(Rectangle())
    }
}

#if !os(tvOS)
/// Press feedback for a legend cell. Deliberately quiet — the bar is chrome, and a cell that lit
/// up like a primary button would pull the eye off the content it describes.
///
/// `contentShape` sits BELOW the scale so the hit region stays the unscaled layout bounds: a press
/// animation that shrinks the artwork must never move the target out from under a resting finger,
/// or the touch-up lands outside and SwiftUI discards the tap.
private struct HintCellStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .opacity(configuration.isPressed ? 0.55 : 1)
            .scaleEffect(configuration.isPressed ? 0.94 : 1)
            .animation(.smooth(duration: 0.14), value: configuration.isPressed)
            .contentShape(Rectangle())
    }
}
#endif
#endif
