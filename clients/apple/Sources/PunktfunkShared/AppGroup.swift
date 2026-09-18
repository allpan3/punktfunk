// The App-Group foundation shared by the app and its extensions (Widgets / Live Activity).
//
// PunktfunkShared is deliberately dependency-free: it never links PunktfunkKit, which would drag
// in the Rust staticlib and the presentation layer. A widget process gets ~30 MB, so everything an
// extension needs — the stored-host model and its JSON codec, the settings-key names, the deep-link
// grammar, the Live Activity attributes — lives here and here only. (A few files import SwiftUI for
// a Color or an AppIntent; none import PunktfunkKit.)

import Foundation

/// The one App-Group identifier, matched by `Config/*.entitlements`
/// (`com.apple.security.application-groups`). Registered on the developer portal for both the app
/// id (`io.unom.punktfunk`) and the widget extension id (`io.unom.punktfunk.widgets`).
public enum AppGroup {
    public static let suiteName = "group.io.unom.punktfunk"

    /// Ad-hoc macOS packages opt into app-local storage because they have no provisioned group
    /// Creating a named suite does not establish permission to read or write it
    public static var defaults: UserDefaults {
        #if os(macOS)
        if Bundle.main.object(forInfoDictionaryKey: "PunktfunkUseAppLocalDefaults") as? Bool == true {
            return .standard
        }
        #endif
        return UserDefaults(suiteName: suiteName) ?? .standard
    }
}

/// Widget kind strings. The extension declares them and the app reloads by them, so a rename that
/// touches only one side leaves a widget nothing ever refreshes again (both timelines are
/// `.never`).
public enum WidgetKind {
    public static let hosts = "PunktfunkHosts"
    public static let library = "PunktfunkLibrary"
}
