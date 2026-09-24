// The gamepad UI's background colour families, and the ink each one calls for.
//
// A palette is a short ordered ramp of DISTINCT hues, not one hue at several brightnesses. The
// 4×4 mesh samples that ramp diagonally with a per-cell offset (`cellRamp`), so neighbouring
// cells land on different parts of it and the colours pool and swirl the way a real gradient
// poster does; the mesh's existing control-point drift then moves those pools around. An earlier
// version rotated ONE field's hue per palette, which is why every non-default palette read flat.
//
// A palette also owns the UI sitting on it: `accent` is the focus wash / selected pill / switch
// colour, and `light` flips the ink so a pale field gets dark text instead of white.
//
// The table, `ramp` and `cellRamp` are mirrored in `pf-console-ui`'s `library.rs` (Rust) and the
// Android client's `GamepadPalette.kt` (Kotlin) under the same ids, so one `ui_palette` value is
// one look on every client. Keep the three copies in step: a palette added here without the
// others is a value the other clients silently render as Violet.
//
// It lives in PunktfunkShared rather than next to the views because that is the target the tests
// can reach — the arithmetic below is the part that has to agree across three languages.

import Foundation
import simd

public struct GamepadPalette: Identifiable, Equatable, Sendable {
    /// The stored `ui_palette` value (`DefaultsKey.uiPalette`).
    public let id: String
    /// What the settings row shows.
    public let name: String
    /// The colour ramp, dark end first. Empty = use `violetMesh` verbatim (the brand default,
    /// kept bit-identical to what every install already sees).
    public let stops: [SIMD3<Double>]
    /// The field's ground — what the corners settle onto and what the calm mix lifts toward.
    public let ground: SIMD3<Double>
    /// The UI accent: focus wash, selected tab pill, switch track, caret.
    public let accent: SIMD3<Double>
    /// A pale field: the UI flips to dark ink and the legibility scrims go white.
    public let light: Bool

    /// Where each of the 16 mesh cells samples the ramp. The base is the diagonal
    /// `0.5·(x + y)` — top-left is the ramp's dark end, bottom-right its bright one — and the
    /// per-cell nudges break the banding a pure diagonal would give, so hues pool instead of
    /// striping.
    static let cellRamp: [Double] = [
         0.10, -0.06,  0.04, -0.12,
        -0.08,  0.14, -0.10,  0.06,
         0.06, -0.12,  0.16, -0.04,
        -0.10,  0.08, -0.06,  0.12,
    ]

    /// The brand default's 16 mesh colours, row-major 4×4: dark-violet corners sink the frame,
    /// the edges carry mid-tone violets, and the interior holds the bright brand family.
    public static let violetMesh: [SIMD3<Double>] = {
        let corner = SIMD3(0.075, 0.060, 0.160)
        return [
            corner, SIMD3(0.34, 0.27, 0.72), SIMD3(0.30, 0.26, 0.74), corner,
            SIMD3(0.42, 0.20, 0.54), SIMD3(0.49, 0.39, 0.95), SIMD3(0.28, 0.31, 0.84), SIMD3(0.16, 0.26, 0.64),
            SIMD3(0.45, 0.23, 0.60), SIMD3(0.53, 0.31, 0.75), SIMD3(0.35, 0.35, 0.91), SIMD3(0.19, 0.28, 0.70),
            corner, SIMD3(0.22, 0.18, 0.54), SIMD3(0.24, 0.20, 0.58), corner,
        ]
    }()

    /// The brand default's blob ramp — the four colours the pre-18/15 legacy field used, kept so
    /// `violet` is unchanged on older OSes too.
    static let violetBlobs: [SIMD3<Double>] = [
        SIMD3(0.53, 0.47, 0.96), SIMD3(0.24, 0.20, 0.72), SIMD3(0.62, 0.30, 0.80),
        SIMD3(0.22, 0.38, 0.86), SIMD3(0.53, 0.47, 0.96),
    ]

    /// The 36 shipped palettes: the brand default, 19 more dark fields, then 16 pale
    /// ones. Cycling order runs dark → light, so a walk of the table goes one way.
    public static let all: [GamepadPalette] = [
        // --- dark fields (white ink) ---
        GamepadPalette(
            // The brand default: a bright periwinkle field with lavender pools, still white ink.
            id: "violet", name: "Violet", stops: [],
            ground: SIMD3(0.510, 0.470, 0.960), accent: SIMD3(0.525, 0.471, 0.961), light: false),
        GamepadPalette(
            // First two stops are (0,0,0): OLED pixels off, not dark grey. Ground is black so the
            // calm mix lifts to nothing. The id stays "oled": it is the stored `ui_palette` value
            // and the cross-client key, so renaming it would orphan saved choices.
            id: "oled", name: "Eclipse",
            stops: [SIMD3(0.00, 0.00, 0.00), SIMD3(0.00, 0.00, 0.00), SIMD3(0.01, 0.02, 0.10),
                    SIMD3(0.045, 0.016, 0.115), SIMD3(0.12, 0.024, 0.13)],
            ground: SIMD3(0.000, 0.000, 0.000), accent: SIMD3(0.525, 0.471, 0.961), light: false),
        GamepadPalette(
            // Nothing at all: every pixel off. The accent is the only colour on it.
            id: "void", name: "Void",
            stops: [SIMD3(0.00, 0.00, 0.00), SIMD3(0.00, 0.00, 0.00), SIMD3(0.00, 0.00, 0.00),
                    SIMD3(0.00, 0.00, 0.00), SIMD3(0.00, 0.00, 0.00)],
            ground: SIMD3(0.000, 0.000, 0.000), accent: SIMD3(0.525, 0.471, 0.961), light: false),
        GamepadPalette(
            id: "graphite", name: "Graphite",
            stops: [SIMD3(0.06, 0.07, 0.11), SIMD3(0.15, 0.18, 0.25), SIMD3(0.30, 0.31, 0.35),
                    SIMD3(0.45, 0.42, 0.38), SIMD3(0.60, 0.56, 0.49)],
            ground: SIMD3(0.055, 0.055, 0.070), accent: SIMD3(0.78, 0.80, 0.86), light: false),
        GamepadPalette(
            // Cool neutral: blue-grey warming to stone at the top.
            id: "slate", name: "Slate",
            stops: [SIMD3(0.06, 0.08, 0.11), SIMD3(0.14, 0.18, 0.24), SIMD3(0.24, 0.30, 0.38),
                    SIMD3(0.40, 0.44, 0.48), SIMD3(0.60, 0.58, 0.52)],
            ground: SIMD3(0.060, 0.080, 0.110), accent: SIMD3(0.60, 0.80, 1.00), light: false),
        GamepadPalette(
            // Indigo shadow, cobalt body, a teal break.
            id: "midnight", name: "Midnight",
            stops: [SIMD3(0.05, 0.02, 0.16), SIMD3(0.05, 0.11, 0.36), SIMD3(0.08, 0.24, 0.60),
                    SIMD3(0.14, 0.42, 0.80), SIMD3(0.36, 0.76, 0.86)],
            ground: SIMD3(0.030, 0.050, 0.160), accent: SIMD3(0.40, 0.72, 1.00), light: false),
        GamepadPalette(
            // Ultraviolet into an electric blue and cyan.
            id: "electric", name: "Electric",
            stops: [SIMD3(0.02, 0.00, 0.10), SIMD3(0.14, 0.02, 0.50), SIMD3(0.30, 0.10, 0.95),
                    SIMD3(0.10, 0.45, 1.00), SIMD3(0.20, 0.90, 1.00)],
            ground: SIMD3(0.020, 0.000, 0.100), accent: SIMD3(0.45, 0.85, 1.00), light: false),
        GamepadPalette(
            // Deep water rising through teal to foam.
            id: "ocean", name: "Ocean",
            stops: [SIMD3(0.01, 0.05, 0.14), SIMD3(0.02, 0.18, 0.40), SIMD3(0.02, 0.40, 0.62),
                    SIMD3(0.05, 0.66, 0.72), SIMD3(0.55, 0.92, 0.80)],
            ground: SIMD3(0.010, 0.050, 0.140), accent: SIMD3(0.45, 0.95, 0.90), light: false),
        GamepadPalette(
            // Deep teal into green, then a violet curtain.
            id: "aurora", name: "Aurora",
            stops: [SIMD3(0.02, 0.07, 0.11), SIMD3(0.03, 0.24, 0.28), SIMD3(0.05, 0.46, 0.40),
                    SIMD3(0.14, 0.60, 0.72), SIMD3(0.44, 0.42, 0.86)],
            ground: SIMD3(0.020, 0.070, 0.110), accent: SIMD3(0.36, 0.90, 0.78), light: false),
        GamepadPalette(
            // Deep water into jade and a leaf-green lift.
            id: "jade", name: "Jade",
            stops: [SIMD3(0.02, 0.07, 0.10), SIMD3(0.03, 0.22, 0.19), SIMD3(0.05, 0.40, 0.34),
                    SIMD3(0.16, 0.58, 0.46), SIMD3(0.60, 0.84, 0.52)],
            ground: SIMD3(0.020, 0.070, 0.100), accent: SIMD3(0.52, 0.90, 0.62), light: false),
        GamepadPalette(
            // Saturated green, forest floor to lime.
            id: "emerald", name: "Emerald",
            stops: [SIMD3(0.01, 0.07, 0.04), SIMD3(0.02, 0.26, 0.12), SIMD3(0.04, 0.50, 0.22),
                    SIMD3(0.16, 0.74, 0.34), SIMD3(0.60, 0.92, 0.40)],
            ground: SIMD3(0.010, 0.070, 0.040), accent: SIMD3(0.55, 1.00, 0.55), light: false),
        GamepadPalette(
            // Plum shadow, crimson body, a coral edge.
            id: "crimson", name: "Crimson",
            stops: [SIMD3(0.10, 0.02, 0.10), SIMD3(0.34, 0.03, 0.12), SIMD3(0.62, 0.06, 0.20),
                    SIMD3(0.86, 0.20, 0.28), SIMD3(0.98, 0.52, 0.32)],
            ground: SIMD3(0.080, 0.020, 0.060), accent: SIMD3(1.00, 0.42, 0.42), light: false),
        GamepadPalette(
            // Violet shadow under a hot pink-red.
            id: "ruby", name: "Ruby",
            stops: [SIMD3(0.06, 0.00, 0.20), SIMD3(0.40, 0.02, 0.16), SIMD3(0.80, 0.06, 0.30),
                    SIMD3(1.00, 0.28, 0.48), SIMD3(1.00, 0.62, 0.56)],
            ground: SIMD3(0.080, 0.000, 0.060), accent: SIMD3(1.00, 0.50, 0.62), light: false),
        GamepadPalette(
            // Black rock, red heat, a yellow glow.
            id: "lava", name: "Lava",
            stops: [SIMD3(0.06, 0.01, 0.02), SIMD3(0.42, 0.02, 0.04), SIMD3(0.86, 0.12, 0.02),
                    SIMD3(1.00, 0.45, 0.02), SIMD3(1.00, 0.85, 0.20)],
            ground: SIMD3(0.060, 0.010, 0.020), accent: SIMD3(1.00, 0.72, 0.20), light: false),
        GamepadPalette(
            // Bronze shadow under copper, a verdigris lift.
            id: "copper", name: "Copper",
            stops: [SIMD3(0.08, 0.05, 0.04), SIMD3(0.36, 0.15, 0.08), SIMD3(0.66, 0.32, 0.14),
                    SIMD3(0.85, 0.56, 0.26), SIMD3(0.50, 0.78, 0.62)],
            ground: SIMD3(0.070, 0.050, 0.040), accent: SIMD3(1.00, 0.70, 0.36), light: false),
        GamepadPalette(
            // Wine shadow into amber and gold.
            id: "amber", name: "Amber",
            stops: [SIMD3(0.10, 0.02, 0.10), SIMD3(0.36, 0.16, 0.02), SIMD3(0.66, 0.36, 0.04),
                    SIMD3(0.88, 0.58, 0.08), SIMD3(0.92, 0.86, 0.36)],
            ground: SIMD3(0.100, 0.040, 0.020), accent: SIMD3(1.00, 0.80, 0.30), light: false),
        GamepadPalette(
            // Indigo through mauve to a peach horizon.
            id: "dusk", name: "Dusk",
            stops: [SIMD3(0.08, 0.04, 0.14), SIMD3(0.26, 0.10, 0.34), SIMD3(0.50, 0.20, 0.48),
                    SIMD3(0.78, 0.38, 0.50), SIMD3(0.96, 0.62, 0.48)],
            ground: SIMD3(0.070, 0.040, 0.120), accent: SIMD3(1.00, 0.62, 0.56), light: false),
        GamepadPalette(
            // Purple climbing to orchid and pink.
            id: "grape", name: "Grape",
            stops: [SIMD3(0.08, 0.02, 0.16), SIMD3(0.28, 0.06, 0.48), SIMD3(0.52, 0.14, 0.78),
                    SIMD3(0.78, 0.30, 0.92), SIMD3(1.00, 0.55, 0.80)],
            ground: SIMD3(0.080, 0.020, 0.160), accent: SIMD3(0.85, 0.55, 1.00), light: false),
        GamepadPalette(
            // Magenta, electric blue and a lime flash on black.
            id: "neon", name: "Neon",
            stops: [SIMD3(0.05, 0.00, 0.12), SIMD3(0.40, 0.00, 0.60), SIMD3(0.90, 0.05, 0.55),
                    SIMD3(0.15, 0.35, 0.95), SIMD3(0.30, 0.95, 0.55)],
            ground: SIMD3(0.050, 0.000, 0.120), accent: SIMD3(0.40, 1.00, 0.70), light: false),
        GamepadPalette(
            // Teal shade, orange sun, a pink bloom.
            id: "tropic", name: "Tropic",
            stops: [SIMD3(0.02, 0.10, 0.12), SIMD3(0.02, 0.42, 0.42), SIMD3(0.95, 0.45, 0.10),
                    SIMD3(0.98, 0.20, 0.45), SIMD3(0.40, 0.10, 0.55)],
            ground: SIMD3(0.020, 0.080, 0.100), accent: SIMD3(1.00, 0.60, 0.30), light: false),
        // --- pale fields (dark ink) ---
        GamepadPalette(
            // Near-white: warm cream, cool blue and a rose tint in turn.
            id: "paper", name: "Paper",
            stops: [SIMD3(0.99, 0.95, 0.88), SIMD3(0.91, 0.94, 0.98), SIMD3(0.98, 0.91, 0.94),
                    SIMD3(0.99, 0.97, 0.89), SIMD3(0.90, 0.94, 0.99)],
            ground: SIMD3(0.970, 0.960, 0.940), accent: SIMD3(0.42, 0.30, 0.28), light: true),
        GamepadPalette(
            // Pale blue through periwinkle to a mint edge.
            id: "sky", name: "Sky",
            stops: [SIMD3(0.76, 0.87, 1.00), SIMD3(0.62, 0.78, 0.99), SIMD3(0.72, 0.76, 0.99),
                    SIMD3(0.84, 0.82, 1.00), SIMD3(0.86, 0.98, 0.96)],
            ground: SIMD3(0.920, 0.950, 1.000), accent: SIMD3(0.12, 0.30, 0.62), light: true),
        GamepadPalette(
            // Saturated sky blue cooling into violet.
            id: "glacier", name: "Glacier",
            stops: [SIMD3(0.45, 0.70, 1.00), SIMD3(0.60, 0.80, 1.00), SIMD3(0.75, 0.85, 1.00),
                    SIMD3(0.85, 0.80, 1.00), SIMD3(0.95, 0.85, 1.00)],
            ground: SIMD3(0.780, 0.880, 1.000), accent: SIMD3(0.10, 0.20, 0.55), light: true),
        GamepadPalette(
            // Lavender and periwinkle, warming to a pink bloom.
            id: "lilac", name: "Lilac",
            stops: [SIMD3(0.84, 0.76, 0.99), SIMD3(0.74, 0.70, 0.99), SIMD3(0.88, 0.74, 0.98),
                    SIMD3(0.98, 0.82, 0.94), SIMD3(0.96, 0.94, 1.00)],
            ground: SIMD3(0.950, 0.920, 0.990), accent: SIMD3(0.44, 0.24, 0.66), light: true),
        GamepadPalette(
            // Vivid purple and periwinkle, a pink edge.
            id: "iris", name: "Iris",
            stops: [SIMD3(0.60, 0.35, 0.95), SIMD3(0.55, 0.50, 1.00), SIMD3(0.65, 0.65, 1.00),
                    SIMD3(0.85, 0.60, 0.98), SIMD3(1.00, 0.70, 0.90)],
            ground: SIMD3(0.800, 0.720, 1.000), accent: SIMD3(0.25, 0.05, 0.55), light: true),
        GamepadPalette(
            // Hot pink cooling into a baby blue.
            id: "bubblegum", name: "Bubblegum",
            stops: [SIMD3(1.00, 0.50, 0.80), SIMD3(1.00, 0.62, 0.85), SIMD3(0.92, 0.70, 0.95),
                    SIMD3(0.70, 0.75, 1.00), SIMD3(0.60, 0.85, 1.00)],
            ground: SIMD3(1.000, 0.780, 0.900), accent: SIMD3(0.55, 0.05, 0.35), light: true),
        GamepadPalette(
            // Pink into coral, fading to a warm cream.
            id: "coral", name: "Coral",
            stops: [SIMD3(1.00, 0.58, 0.62), SIMD3(1.00, 0.68, 0.56), SIMD3(1.00, 0.80, 0.64),
                    SIMD3(0.99, 0.88, 0.72), SIMD3(1.00, 0.96, 0.80)],
            ground: SIMD3(1.000, 0.900, 0.800), accent: SIMD3(0.68, 0.14, 0.22), light: true),
        GamepadPalette(
            // Hot pink into orange, fully saturated.
            id: "flamingo", name: "Flamingo",
            stops: [SIMD3(1.00, 0.30, 0.60), SIMD3(1.00, 0.42, 0.55), SIMD3(1.00, 0.55, 0.45),
                    SIMD3(1.00, 0.70, 0.45), SIMD3(1.00, 0.85, 0.60)],
            ground: SIMD3(1.000, 0.720, 0.660), accent: SIMD3(0.50, 0.00, 0.20), light: true),
        GamepadPalette(
            // Pink into peach and apricot.
            id: "peach", name: "Peach",
            stops: [SIMD3(1.00, 0.45, 0.60), SIMD3(1.00, 0.60, 0.44), SIMD3(1.00, 0.72, 0.52),
                    SIMD3(1.00, 0.82, 0.58), SIMD3(0.98, 0.92, 0.72)],
            ground: SIMD3(1.000, 0.820, 0.660), accent: SIMD3(0.60, 0.16, 0.10), light: true),
        GamepadPalette(
            // Pink, peach, lime and sky in one bag.
            id: "candy", name: "Candy",
            stops: [SIMD3(1.00, 0.40, 0.70), SIMD3(1.00, 0.55, 0.60), SIMD3(1.00, 0.75, 0.40),
                    SIMD3(0.80, 0.90, 0.50), SIMD3(0.55, 0.85, 0.95)],
            ground: SIMD3(1.000, 0.800, 0.750), accent: SIMD3(0.55, 0.05, 0.30), light: true),
        GamepadPalette(
            // Lemon through lime to a pale green.
            id: "lemon", name: "Lemon",
            stops: [SIMD3(1.00, 0.86, 0.44), SIMD3(0.98, 0.96, 0.56), SIMD3(0.70, 0.94, 0.64),
                    SIMD3(0.84, 0.97, 0.78), SIMD3(0.98, 0.99, 0.90)],
            ground: SIMD3(1.000, 0.980, 0.860), accent: SIMD3(0.30, 0.36, 0.08), light: true),
        GamepadPalette(
            // Orange into a full yellow and a green edge.
            id: "sunflower", name: "Sunflower",
            stops: [SIMD3(1.00, 0.60, 0.10), SIMD3(1.00, 0.75, 0.10), SIMD3(1.00, 0.88, 0.20),
                    SIMD3(0.95, 0.95, 0.40), SIMD3(0.75, 0.92, 0.60)],
            ground: SIMD3(1.000, 0.880, 0.400), accent: SIMD3(0.40, 0.22, 0.00), light: true),
        GamepadPalette(
            // Orange, lemon and lime scoops.
            id: "sherbet", name: "Sherbet",
            stops: [SIMD3(1.00, 0.55, 0.25), SIMD3(1.00, 0.72, 0.30), SIMD3(1.00, 0.90, 0.40),
                    SIMD3(0.85, 0.95, 0.50), SIMD3(0.60, 0.92, 0.70)],
            ground: SIMD3(1.000, 0.850, 0.600), accent: SIMD3(0.45, 0.18, 0.02), light: true),
        GamepadPalette(
            // Sage into pale olive and cream.
            id: "sage", name: "Sage",
            stops: [SIMD3(0.72, 0.84, 0.70), SIMD3(0.80, 0.90, 0.76), SIMD3(0.90, 0.94, 0.80),
                    SIMD3(0.96, 0.96, 0.84), SIMD3(0.99, 0.97, 0.90)],
            ground: SIMD3(0.940, 0.960, 0.900), accent: SIMD3(0.18, 0.36, 0.24), light: true),
        GamepadPalette(
            // Grass green into a warm yellow.
            id: "meadow", name: "Meadow",
            stops: [SIMD3(0.30, 0.80, 0.40), SIMD3(0.55, 0.90, 0.40), SIMD3(0.80, 0.95, 0.45),
                    SIMD3(0.95, 0.95, 0.55), SIMD3(1.00, 0.90, 0.65)],
            ground: SIMD3(0.850, 0.950, 0.600), accent: SIMD3(0.10, 0.35, 0.12), light: true),
        GamepadPalette(
            // Turquoise water into a pale green shore.
            id: "lagoon", name: "Lagoon",
            stops: [SIMD3(0.20, 0.75, 0.80), SIMD3(0.35, 0.85, 0.85), SIMD3(0.55, 0.92, 0.80),
                    SIMD3(0.70, 0.95, 0.70), SIMD3(0.92, 0.98, 0.75)],
            ground: SIMD3(0.750, 0.950, 0.900), accent: SIMD3(0.02, 0.30, 0.35), light: true),
    ]

    /// The palette stored under `id`, falling back to the brand default — an unknown name is a
    /// palette a newer client shipped, not a reason to draw nothing.
    public static func named(_ id: String) -> GamepadPalette {
        all.first { $0.id == id } ?? all[0]
    }

    /// Sample an ordered colour ramp at `t` ∈ [0, 1] (linear between neighbouring stops).
    public static func ramp(_ stops: [SIMD3<Double>], _ t: Double) -> SIMD3<Double> {
        guard let first = stops.first else { return SIMD3(0, 0, 0) }
        guard stops.count > 1 else { return first }
        let x = min(max(t, 0), 1) * Double(stops.count - 1)
        let i = min(Int(x.rounded(.down)), stops.count - 2)
        let f = x - Double(i)
        return stops[i] + (stops[i + 1] - stops[i]) * f
    }

    /// The 16 mesh colours for this palette: the ramp sampled per cell, or `violetMesh` verbatim
    /// for the brand default.
    public var meshColors: [SIMD3<Double>] {
        guard !stops.isEmpty else { return Self.violetMesh }
        return (0..<16).map { i in
            let (x, y) = (Double(i % 4) / 3.0, Double(i / 4) / 3.0)
            return Self.ramp(stops, 0.5 * (x + y) + Self.cellRamp[i])
        }
    }

    /// Four drifting blob colours for the pre-18/15 legacy field. Spread across the ramp so it
    /// still shows several hues at once.
    public var blobColors: [SIMD3<Double>] {
        let s = stops.isEmpty ? Self.violetBlobs : stops
        return (0..<4).map { Self.ramp(s, 0.15 + 0.25 * Double($0)) }
    }
}
