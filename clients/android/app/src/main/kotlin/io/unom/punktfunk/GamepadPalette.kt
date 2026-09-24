package io.unom.punktfunk

import androidx.compose.ui.graphics.Color

// The console (gamepad) UI's background colour families, and the ink each one calls for.
//
// A palette is a short ordered ramp of DISTINCT hues, not one hue at several brightnesses. The
// field samples that ramp so several tones show at once and pool into each other, the way a real
// gradient poster does. An earlier version rotated ONE field's hue per palette, which is why every
// non-default palette read flat and monotone.
//
// A palette also owns the UI sitting on it: [accent] is the focus wash / selected pill / switch
// colour, and [light] flips the ink so a pale field gets dark text instead of white.
//
// The table, [ramp] and [CELL_RAMP] are mirrored in `pf-console-ui`'s `library.rs` (Rust) and the
// Apple client's `GamepadPalette.swift` under the same ids, so one `ui_palette` value is one look
// on every client. Keep the three copies in step: a palette added here without the others is a
// value the other clients silently render as Violet.

/**
 * One wandering interior control point of the mesh: [x]/[y] its resting place in unit UV, [amp] how
 * far it strays, [sx]/[sy] its per-axis rates in rad·s⁻¹ and [phase] its offset. Its live
 * displacement `(amp·sin(t·sx+ph), amp·cos(t·sy+ph·1.3))` drives a bounded domain warp, so the
 * bright colour pools drift with it.
 */
class MeshWarpPoint(
    val x: Double,
    val y: Double,
    val amp: Double,
    val sx: Double,
    val sy: Double,
    val phase: Double,
)

/** One background colour family. */
class GamepadPalette(
    /** The stored `ui_palette` value ([Settings.uiPalette]). */
    val id: String,
    /** What the settings row shows. */
    val name: String,
    /**
     * The colour ramp, dark end first. Empty = the brand default's explicit field, kept
     * bit-identical to what every install already sees.
     */
    val stops: List<Triple<Double, Double, Double>>,
    /** The field's ground — what it settles onto and what the calm mix lifts toward. */
    val ground: Triple<Double, Double, Double>,
    /** The UI accent: focus wash, selected tab pill, switch track. */
    val accent: Triple<Double, Double, Double>,
    /** A pale field: the UI flips to dark ink and the legibility scrims go white. */
    val light: Boolean,
) {
    /** Four drifting blob colours, spread across the ramp so the field shows several hues. */
    val blobColors: List<Color> by lazy {
        val s = stops.ifEmpty { VIOLET_BLOBS }
        (0..3).map { color(ramp(s, 0.15 + 0.25 * it)) }
    }

    /** The field's ground as a Compose colour. */
    val groundColor: Color by lazy { color(ground) }

    /** The accent as a Compose colour. */
    val accentColor: Color by lazy { color(accent) }

    /**
     * The 16 mesh colours this palette's field is woven from: the ramp sampled per cell (see
     * [CELL_RAMP]), or [MESH_COLORS] verbatim for the brand default — the exact rule
     * `pf-console-ui`'s `Palette::mesh_colors` follows, so one `ui_palette` value is one field on
     * every client. Consumed by the AGSL backdrop on API 33+; the blob field
     * ([blobColors]) approximates the same table below that.
     */
    val meshColors: List<Triple<Double, Double, Double>> by lazy {
        if (stops.isEmpty()) {
            MESH_COLORS
        } else {
            (0..15).map { i ->
                ramp(stops, 0.5 * ((i % 4) / 3.0 + (i / 4) / 3.0) + CELL_RAMP[i])
            }
        }
    }

    companion object {
        /**
         * Where each of the 16 mesh cells samples the ramp. The base is the diagonal
         * `0.5·(x + y)` — top-left the ramp's dark end, bottom-right its bright one — and the
         * per-cell nudges break the banding a pure diagonal would show. Mirrored from
         * `pf-console-ui`'s `CELL_RAMP`.
         */
        val CELL_RAMP = listOf(
            0.10, -0.06, 0.04, -0.12,
            -0.08, 0.14, -0.10, 0.06,
            0.06, -0.12, 0.16, -0.04,
            -0.10, 0.08, -0.06, 0.12,
        )

        /**
         * The brand default's 16 mesh colours, used verbatim (rather than sampled from a ramp) so
         * `violet` stays bit-identical to what every install already sees. Mirrors
         * `pf-console-ui`'s `MESH_COLORS`.
         */
        val MESH_COLORS = listOf(
            Triple(0.075, 0.060, 0.160), Triple(0.34, 0.27, 0.72),
            Triple(0.30, 0.26, 0.74), Triple(0.075, 0.060, 0.160),
            Triple(0.42, 0.20, 0.54), Triple(0.49, 0.39, 0.95),
            Triple(0.28, 0.31, 0.84), Triple(0.16, 0.26, 0.64),
            Triple(0.45, 0.23, 0.60), Triple(0.53, 0.31, 0.75),
            Triple(0.35, 0.35, 0.91), Triple(0.19, 0.28, 0.70),
            Triple(0.075, 0.060, 0.160), Triple(0.22, 0.18, 0.54),
            Triple(0.24, 0.20, 0.58), Triple(0.075, 0.060, 0.160),
        )

        /**
         * The four interior points that wander; the 12 boundary points stay pinned to the frame (a
         * drifting edge point would shrink the field and expose the ground behind it). Periods
         * ~90–130 s, out of phase, so the field never visibly loops. Mirrors `MESH_INTERIOR`.
         */
        val MESH_INTERIOR = listOf(
            MeshWarpPoint(0.333, 0.333, 0.11, 0.049, 0.063, 0.4),
            MeshWarpPoint(0.667, 0.333, 0.10, 0.055, 0.052, 2.1),
            MeshWarpPoint(0.333, 0.667, 0.10, 0.058, 0.049, 3.6),
            MeshWarpPoint(0.667, 0.667, 0.12, 0.047, 0.061, 5.0),
        )

        /** The brand default's blob ramp — the colours the pre-palette field used. */
        private val VIOLET_BLOBS = listOf(
            Triple(0.53, 0.47, 0.96), Triple(0.24, 0.20, 0.72), Triple(0.62, 0.30, 0.80),
            Triple(0.22, 0.38, 0.86), Triple(0.53, 0.47, 0.96),
        )

        /**
         * The 36 shipped palettes: the brand default, 19 more dark fields, then 16 pale
         * ones. Cycling order runs dark → light, so a walk of the table goes one way.
         */
        val ALL = listOf(
            // --- dark fields (white ink) ---
            GamepadPalette(
                // The brand default: a bright periwinkle field with lavender pools, still white ink.
                "violet", "Violet", emptyList(),
                ground = Triple(0.510, 0.470, 0.960),
                accent = Triple(0.525, 0.471, 0.961), light = false,
            ),
            GamepadPalette(
                // First two stops are (0,0,0): OLED pixels off, not dark grey. Ground is black so
                // the calm mix lifts to nothing. The id stays "oled": it is the stored `ui_palette`
                // value and the cross-client key, so renaming it would orphan saved choices.
                "oled", "Eclipse",
                listOf(
                    Triple(0.00, 0.00, 0.00), Triple(0.00, 0.00, 0.00), Triple(0.01, 0.02, 0.10),
                    Triple(0.045, 0.016, 0.115), Triple(0.12, 0.024, 0.13),
                ),
                ground = Triple(0.000, 0.000, 0.000),
                accent = Triple(0.525, 0.471, 0.961), light = false,
            ),
            GamepadPalette(
                // Nothing at all: every pixel off. The accent is the only colour on it.
                "void", "Void",
                listOf(
                    Triple(0.00, 0.00, 0.00), Triple(0.00, 0.00, 0.00), Triple(0.00, 0.00, 0.00),
                    Triple(0.00, 0.00, 0.00), Triple(0.00, 0.00, 0.00),
                ),
                ground = Triple(0.000, 0.000, 0.000),
                accent = Triple(0.525, 0.471, 0.961), light = false,
            ),
            GamepadPalette(
                "graphite", "Graphite",
                listOf(
                    Triple(0.06, 0.07, 0.11), Triple(0.15, 0.18, 0.25), Triple(0.30, 0.31, 0.35),
                    Triple(0.45, 0.42, 0.38), Triple(0.60, 0.56, 0.49),
                ),
                ground = Triple(0.055, 0.055, 0.070),
                accent = Triple(0.78, 0.80, 0.86), light = false,
            ),
            GamepadPalette(
                // Cool neutral: blue-grey warming to stone at the top.
                "slate", "Slate",
                listOf(
                    Triple(0.06, 0.08, 0.11), Triple(0.14, 0.18, 0.24), Triple(0.24, 0.30, 0.38),
                    Triple(0.40, 0.44, 0.48), Triple(0.60, 0.58, 0.52),
                ),
                ground = Triple(0.060, 0.080, 0.110),
                accent = Triple(0.60, 0.80, 1.00), light = false,
            ),
            GamepadPalette(
                // Indigo shadow, cobalt body, a teal break.
                "midnight", "Midnight",
                listOf(
                    Triple(0.05, 0.02, 0.16), Triple(0.05, 0.11, 0.36), Triple(0.08, 0.24, 0.60),
                    Triple(0.14, 0.42, 0.80), Triple(0.36, 0.76, 0.86),
                ),
                ground = Triple(0.030, 0.050, 0.160),
                accent = Triple(0.40, 0.72, 1.00), light = false,
            ),
            GamepadPalette(
                // Ultraviolet into an electric blue and cyan.
                "electric", "Electric",
                listOf(
                    Triple(0.02, 0.00, 0.10), Triple(0.14, 0.02, 0.50), Triple(0.30, 0.10, 0.95),
                    Triple(0.10, 0.45, 1.00), Triple(0.20, 0.90, 1.00),
                ),
                ground = Triple(0.020, 0.000, 0.100),
                accent = Triple(0.45, 0.85, 1.00), light = false,
            ),
            GamepadPalette(
                // Deep water rising through teal to foam.
                "ocean", "Ocean",
                listOf(
                    Triple(0.01, 0.05, 0.14), Triple(0.02, 0.18, 0.40), Triple(0.02, 0.40, 0.62),
                    Triple(0.05, 0.66, 0.72), Triple(0.55, 0.92, 0.80),
                ),
                ground = Triple(0.010, 0.050, 0.140),
                accent = Triple(0.45, 0.95, 0.90), light = false,
            ),
            GamepadPalette(
                // Deep teal into green, then a violet curtain.
                "aurora", "Aurora",
                listOf(
                    Triple(0.02, 0.07, 0.11), Triple(0.03, 0.24, 0.28), Triple(0.05, 0.46, 0.40),
                    Triple(0.14, 0.60, 0.72), Triple(0.44, 0.42, 0.86),
                ),
                ground = Triple(0.020, 0.070, 0.110),
                accent = Triple(0.36, 0.90, 0.78), light = false,
            ),
            GamepadPalette(
                // Deep water into jade and a leaf-green lift.
                "jade", "Jade",
                listOf(
                    Triple(0.02, 0.07, 0.10), Triple(0.03, 0.22, 0.19), Triple(0.05, 0.40, 0.34),
                    Triple(0.16, 0.58, 0.46), Triple(0.60, 0.84, 0.52),
                ),
                ground = Triple(0.020, 0.070, 0.100),
                accent = Triple(0.52, 0.90, 0.62), light = false,
            ),
            GamepadPalette(
                // Saturated green, forest floor to lime.
                "emerald", "Emerald",
                listOf(
                    Triple(0.01, 0.07, 0.04), Triple(0.02, 0.26, 0.12), Triple(0.04, 0.50, 0.22),
                    Triple(0.16, 0.74, 0.34), Triple(0.60, 0.92, 0.40),
                ),
                ground = Triple(0.010, 0.070, 0.040),
                accent = Triple(0.55, 1.00, 0.55), light = false,
            ),
            GamepadPalette(
                // Plum shadow, crimson body, a coral edge.
                "crimson", "Crimson",
                listOf(
                    Triple(0.10, 0.02, 0.10), Triple(0.34, 0.03, 0.12), Triple(0.62, 0.06, 0.20),
                    Triple(0.86, 0.20, 0.28), Triple(0.98, 0.52, 0.32),
                ),
                ground = Triple(0.080, 0.020, 0.060),
                accent = Triple(1.00, 0.42, 0.42), light = false,
            ),
            GamepadPalette(
                // Violet shadow under a hot pink-red.
                "ruby", "Ruby",
                listOf(
                    Triple(0.06, 0.00, 0.20), Triple(0.40, 0.02, 0.16), Triple(0.80, 0.06, 0.30),
                    Triple(1.00, 0.28, 0.48), Triple(1.00, 0.62, 0.56),
                ),
                ground = Triple(0.080, 0.000, 0.060),
                accent = Triple(1.00, 0.50, 0.62), light = false,
            ),
            GamepadPalette(
                // Black rock, red heat, a yellow glow.
                "lava", "Lava",
                listOf(
                    Triple(0.06, 0.01, 0.02), Triple(0.42, 0.02, 0.04), Triple(0.86, 0.12, 0.02),
                    Triple(1.00, 0.45, 0.02), Triple(1.00, 0.85, 0.20),
                ),
                ground = Triple(0.060, 0.010, 0.020),
                accent = Triple(1.00, 0.72, 0.20), light = false,
            ),
            GamepadPalette(
                // Bronze shadow under copper, a verdigris lift.
                "copper", "Copper",
                listOf(
                    Triple(0.08, 0.05, 0.04), Triple(0.36, 0.15, 0.08), Triple(0.66, 0.32, 0.14),
                    Triple(0.85, 0.56, 0.26), Triple(0.50, 0.78, 0.62),
                ),
                ground = Triple(0.070, 0.050, 0.040),
                accent = Triple(1.00, 0.70, 0.36), light = false,
            ),
            GamepadPalette(
                // Wine shadow into amber and gold.
                "amber", "Amber",
                listOf(
                    Triple(0.10, 0.02, 0.10), Triple(0.36, 0.16, 0.02), Triple(0.66, 0.36, 0.04),
                    Triple(0.88, 0.58, 0.08), Triple(0.92, 0.86, 0.36),
                ),
                ground = Triple(0.100, 0.040, 0.020),
                accent = Triple(1.00, 0.80, 0.30), light = false,
            ),
            GamepadPalette(
                // Indigo through mauve to a peach horizon.
                "dusk", "Dusk",
                listOf(
                    Triple(0.08, 0.04, 0.14), Triple(0.26, 0.10, 0.34), Triple(0.50, 0.20, 0.48),
                    Triple(0.78, 0.38, 0.50), Triple(0.96, 0.62, 0.48),
                ),
                ground = Triple(0.070, 0.040, 0.120),
                accent = Triple(1.00, 0.62, 0.56), light = false,
            ),
            GamepadPalette(
                // Purple climbing to orchid and pink.
                "grape", "Grape",
                listOf(
                    Triple(0.08, 0.02, 0.16), Triple(0.28, 0.06, 0.48), Triple(0.52, 0.14, 0.78),
                    Triple(0.78, 0.30, 0.92), Triple(1.00, 0.55, 0.80),
                ),
                ground = Triple(0.080, 0.020, 0.160),
                accent = Triple(0.85, 0.55, 1.00), light = false,
            ),
            GamepadPalette(
                // Magenta, electric blue and a lime flash on black.
                "neon", "Neon",
                listOf(
                    Triple(0.05, 0.00, 0.12), Triple(0.40, 0.00, 0.60), Triple(0.90, 0.05, 0.55),
                    Triple(0.15, 0.35, 0.95), Triple(0.30, 0.95, 0.55),
                ),
                ground = Triple(0.050, 0.000, 0.120),
                accent = Triple(0.40, 1.00, 0.70), light = false,
            ),
            GamepadPalette(
                // Teal shade, orange sun, a pink bloom.
                "tropic", "Tropic",
                listOf(
                    Triple(0.02, 0.10, 0.12), Triple(0.02, 0.42, 0.42), Triple(0.95, 0.45, 0.10),
                    Triple(0.98, 0.20, 0.45), Triple(0.40, 0.10, 0.55),
                ),
                ground = Triple(0.020, 0.080, 0.100),
                accent = Triple(1.00, 0.60, 0.30), light = false,
            ),
            // --- pale fields (dark ink) ---
            GamepadPalette(
                // Near-white: warm cream, cool blue and a rose tint in turn.
                "paper", "Paper",
                listOf(
                    Triple(0.99, 0.95, 0.88), Triple(0.91, 0.94, 0.98), Triple(0.98, 0.91, 0.94),
                    Triple(0.99, 0.97, 0.89), Triple(0.90, 0.94, 0.99),
                ),
                ground = Triple(0.970, 0.960, 0.940),
                accent = Triple(0.42, 0.30, 0.28), light = true,
            ),
            GamepadPalette(
                // Pale blue through periwinkle to a mint edge.
                "sky", "Sky",
                listOf(
                    Triple(0.76, 0.87, 1.00), Triple(0.62, 0.78, 0.99), Triple(0.72, 0.76, 0.99),
                    Triple(0.84, 0.82, 1.00), Triple(0.86, 0.98, 0.96),
                ),
                ground = Triple(0.920, 0.950, 1.000),
                accent = Triple(0.12, 0.30, 0.62), light = true,
            ),
            GamepadPalette(
                // Saturated sky blue cooling into violet.
                "glacier", "Glacier",
                listOf(
                    Triple(0.45, 0.70, 1.00), Triple(0.60, 0.80, 1.00), Triple(0.75, 0.85, 1.00),
                    Triple(0.85, 0.80, 1.00), Triple(0.95, 0.85, 1.00),
                ),
                ground = Triple(0.780, 0.880, 1.000),
                accent = Triple(0.10, 0.20, 0.55), light = true,
            ),
            GamepadPalette(
                // Lavender and periwinkle, warming to a pink bloom.
                "lilac", "Lilac",
                listOf(
                    Triple(0.84, 0.76, 0.99), Triple(0.74, 0.70, 0.99), Triple(0.88, 0.74, 0.98),
                    Triple(0.98, 0.82, 0.94), Triple(0.96, 0.94, 1.00),
                ),
                ground = Triple(0.950, 0.920, 0.990),
                accent = Triple(0.44, 0.24, 0.66), light = true,
            ),
            GamepadPalette(
                // Vivid purple and periwinkle, a pink edge.
                "iris", "Iris",
                listOf(
                    Triple(0.60, 0.35, 0.95), Triple(0.55, 0.50, 1.00), Triple(0.65, 0.65, 1.00),
                    Triple(0.85, 0.60, 0.98), Triple(1.00, 0.70, 0.90),
                ),
                ground = Triple(0.800, 0.720, 1.000),
                accent = Triple(0.25, 0.05, 0.55), light = true,
            ),
            GamepadPalette(
                // Hot pink cooling into a baby blue.
                "bubblegum", "Bubblegum",
                listOf(
                    Triple(1.00, 0.50, 0.80), Triple(1.00, 0.62, 0.85), Triple(0.92, 0.70, 0.95),
                    Triple(0.70, 0.75, 1.00), Triple(0.60, 0.85, 1.00),
                ),
                ground = Triple(1.000, 0.780, 0.900),
                accent = Triple(0.55, 0.05, 0.35), light = true,
            ),
            GamepadPalette(
                // Pink into coral, fading to a warm cream.
                "coral", "Coral",
                listOf(
                    Triple(1.00, 0.58, 0.62), Triple(1.00, 0.68, 0.56), Triple(1.00, 0.80, 0.64),
                    Triple(0.99, 0.88, 0.72), Triple(1.00, 0.96, 0.80),
                ),
                ground = Triple(1.000, 0.900, 0.800),
                accent = Triple(0.68, 0.14, 0.22), light = true,
            ),
            GamepadPalette(
                // Hot pink into orange, fully saturated.
                "flamingo", "Flamingo",
                listOf(
                    Triple(1.00, 0.30, 0.60), Triple(1.00, 0.42, 0.55), Triple(1.00, 0.55, 0.45),
                    Triple(1.00, 0.70, 0.45), Triple(1.00, 0.85, 0.60),
                ),
                ground = Triple(1.000, 0.720, 0.660),
                accent = Triple(0.50, 0.00, 0.20), light = true,
            ),
            GamepadPalette(
                // Pink into peach and apricot.
                "peach", "Peach",
                listOf(
                    Triple(1.00, 0.45, 0.60), Triple(1.00, 0.60, 0.44), Triple(1.00, 0.72, 0.52),
                    Triple(1.00, 0.82, 0.58), Triple(0.98, 0.92, 0.72),
                ),
                ground = Triple(1.000, 0.820, 0.660),
                accent = Triple(0.60, 0.16, 0.10), light = true,
            ),
            GamepadPalette(
                // Pink, peach, lime and sky in one bag.
                "candy", "Candy",
                listOf(
                    Triple(1.00, 0.40, 0.70), Triple(1.00, 0.55, 0.60), Triple(1.00, 0.75, 0.40),
                    Triple(0.80, 0.90, 0.50), Triple(0.55, 0.85, 0.95),
                ),
                ground = Triple(1.000, 0.800, 0.750),
                accent = Triple(0.55, 0.05, 0.30), light = true,
            ),
            GamepadPalette(
                // Lemon through lime to a pale green.
                "lemon", "Lemon",
                listOf(
                    Triple(1.00, 0.86, 0.44), Triple(0.98, 0.96, 0.56), Triple(0.70, 0.94, 0.64),
                    Triple(0.84, 0.97, 0.78), Triple(0.98, 0.99, 0.90),
                ),
                ground = Triple(1.000, 0.980, 0.860),
                accent = Triple(0.30, 0.36, 0.08), light = true,
            ),
            GamepadPalette(
                // Orange into a full yellow and a green edge.
                "sunflower", "Sunflower",
                listOf(
                    Triple(1.00, 0.60, 0.10), Triple(1.00, 0.75, 0.10), Triple(1.00, 0.88, 0.20),
                    Triple(0.95, 0.95, 0.40), Triple(0.75, 0.92, 0.60),
                ),
                ground = Triple(1.000, 0.880, 0.400),
                accent = Triple(0.40, 0.22, 0.00), light = true,
            ),
            GamepadPalette(
                // Orange, lemon and lime scoops.
                "sherbet", "Sherbet",
                listOf(
                    Triple(1.00, 0.55, 0.25), Triple(1.00, 0.72, 0.30), Triple(1.00, 0.90, 0.40),
                    Triple(0.85, 0.95, 0.50), Triple(0.60, 0.92, 0.70),
                ),
                ground = Triple(1.000, 0.850, 0.600),
                accent = Triple(0.45, 0.18, 0.02), light = true,
            ),
            GamepadPalette(
                // Sage into pale olive and cream.
                "sage", "Sage",
                listOf(
                    Triple(0.72, 0.84, 0.70), Triple(0.80, 0.90, 0.76), Triple(0.90, 0.94, 0.80),
                    Triple(0.96, 0.96, 0.84), Triple(0.99, 0.97, 0.90),
                ),
                ground = Triple(0.940, 0.960, 0.900),
                accent = Triple(0.18, 0.36, 0.24), light = true,
            ),
            GamepadPalette(
                // Grass green into a warm yellow.
                "meadow", "Meadow",
                listOf(
                    Triple(0.30, 0.80, 0.40), Triple(0.55, 0.90, 0.40), Triple(0.80, 0.95, 0.45),
                    Triple(0.95, 0.95, 0.55), Triple(1.00, 0.90, 0.65),
                ),
                ground = Triple(0.850, 0.950, 0.600),
                accent = Triple(0.10, 0.35, 0.12), light = true,
            ),
            GamepadPalette(
                // Turquoise water into a pale green shore.
                "lagoon", "Lagoon",
                listOf(
                    Triple(0.20, 0.75, 0.80), Triple(0.35, 0.85, 0.85), Triple(0.55, 0.92, 0.80),
                    Triple(0.70, 0.95, 0.70), Triple(0.92, 0.98, 0.75),
                ),
                ground = Triple(0.750, 0.950, 0.900),
                accent = Triple(0.02, 0.30, 0.35), light = true,
            ),
        )

        /**
         * The palette stored under [id], falling back to the brand default — an unknown name is a
         * palette a newer client shipped, not a reason to draw nothing.
         */
        fun named(id: String): GamepadPalette = ALL.firstOrNull { it.id == id } ?: ALL[0]

        /** Sample an ordered colour ramp at [t] ∈ [0, 1] (linear between neighbouring stops). */
        fun ramp(
            stops: List<Triple<Double, Double, Double>>,
            t: Double,
        ): Triple<Double, Double, Double> {
            if (stops.isEmpty()) return Triple(0.0, 0.0, 0.0)
            if (stops.size == 1) return stops[0]
            val x = t.coerceIn(0.0, 1.0) * (stops.size - 1)
            val i = x.toInt().coerceAtMost(stops.size - 2)
            val f = x - i
            val (ar, ag, ab) = stops[i]
            val (br, bg, bb) = stops[i + 1]
            return Triple(ar + (br - ar) * f, ag + (bg - ag) * f, ab + (bb - ab) * f)
        }

        fun color(c: Triple<Double, Double, Double>): Color =
            Color(c.first.toFloat(), c.second.toFloat(), c.third.toFloat())
    }
}
