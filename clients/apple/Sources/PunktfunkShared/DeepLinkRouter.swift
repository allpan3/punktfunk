// What a `punktfunk://` link should DO, decided once for both routes.
//
// Connect and browse each ran the same three checks side by side — resolve a `profile=` reference,
// refuse a fingerprint that disagrees with the saved record, refuse to preempt a live session —
// and had already drifted: the same unsaved host produced two different sentences depending on
// which route the user came in on.
//
// A value, so the rules are testable without a window. The caller supplies what it knows and
// performs the outcome; nothing here touches the store, the session or the view.

import Foundation

public enum DeepLinkRouter {
    /// What the caller should do about a link.
    public enum Outcome: Equatable, Sendable {
        /// Refuse, and say this.
        case notice(String)
        /// A host named by something GUESSABLE (its label or address): the same action, one tap
        /// later, so a link cannot dial on its own say-so.
        case confirm(StoredHost, ProfileSelection)
        /// Named by its unguessable record id, or already confirmed: go.
        case proceed(StoredHost, ProfileSelection)
        /// The link points at the session already running. The open foregrounded the app, which
        /// is all "focus it" can mean mid-stream.
        case alreadyHere
    }

    /// The session state the guards need, so the router stays free of the model.
    public struct SessionState: Equatable, Sendable {
        public var isIdle: Bool
        public var activeHostID: UUID?
        public var activeHostName: String?

        public init(isIdle: Bool, activeHostID: UUID? = nil, activeHostName: String? = nil) {
            self.isIdle = isIdle
            self.activeHostID = activeHostID
            self.activeHostName = activeHostName
        }
    }

    /// `browse` picks the wording for an unsaved host — a library needs a paired identity, so
    /// there is nothing to show before the host is saved, where a connect can at least name the
    /// address the link pointed at.
    public static func resolve(
        link: DeepLink,
        hosts: [StoredHost],
        catalog: ProfileCatalog,
        session: SessionState,
        browse: Bool
    ) -> Outcome {
        // The profile FIRST: an unknown or ambiguous reference must refuse, never quietly degrade
        // to the host's own binding, which is a different profile wearing the same host's name.
        var selection = ProfileSelection.inherit
        if let reference = link.profile {
            let (profile, resolution) = catalog.resolve(reference)
            switch resolution {
            case .found:
                selection = .profile(profile?.id ?? "")
            case .notFound:
                return .notice("No settings profile called “\(reference)” on this device.")
            case .ambiguous:
                return .notice(
                    "More than one settings profile is called “\(reference)”. "
                        + "Rename one, or link to it by its id.")
            }
        }

        let resolution = link.resolveHost(in: hosts)
        switch resolution {
        case .known(let host), .confirm(let host):
            guard !link.pinConflict(with: host) else {
                return .notice(
                    "That link's fingerprint doesn't match the identity saved for "
                        + "\(host.displayName). It's out of date, or it isn't pointing where it says.")
            }
            guard session.isIdle else {
                guard session.activeHostID == host.id else {
                    let current = session.activeHostName ?? "a host"
                    return .notice("Already streaming \(current). End that session first.")
                }
                return .alreadyHere
            }
            if case .confirm = resolution {
                return .confirm(host, selection)
            }
            return .proceed(host, selection)

        case .unknown(let address, let port, let name, let fingerprint):
            // Never a silent connect: an unsaved host is a trust decision, and a link is not
            // where it gets made. This only NAMES what the link pointed at.
            guard session.isIdle || browse else {
                return .notice("Already streaming. End that session first.")
            }
            if browse {
                return .notice(
                    "\(name ?? address) isn't saved on this device yet. Add it with the + button "
                        + "first — a library can only be browsed on a saved host.")
            }
            return .notice(
                "\(name ?? address) isn't saved on this device yet. "
                    + "Add it with the + button — the link points at \(address):\(String(port))"
                    + (fingerprint == nil ? "." : ", and carries a fingerprint to verify it against."))

        case .ambiguous:
            return .notice(
                "More than one saved host is called “\(link.hostRef)”. "
                    + "Rename one, or link to it by its address.")

        case .unresolvable:
            return .notice("That host isn't saved on this device.")
        }
    }
}
