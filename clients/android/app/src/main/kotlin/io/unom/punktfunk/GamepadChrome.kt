package io.unom.punktfunk

import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.view.InputDevice
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Easing
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBars
import androidx.compose.foundation.layout.union
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.SportsEsports
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInRoot
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.hideFromAccessibility
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.toggleableState
import androidx.compose.ui.state.ToggleableState
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.chrisbanes.haze.HazeState
import dev.chrisbanes.haze.hazeEffect
import io.unom.punktfunk.kit.Gamepad
import io.unom.punktfunk.kit.deviceBodyVibrator
import androidx.compose.ui.zIndex
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt
import kotlin.math.sin
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch

// The console-style chrome the Compose screens still share. The console itself is the Skia shell
// (SkiaConsoleShell); what is left here serves the licences screen it opens (heading, legend, button
// glyphs), the menu haptics the stream and the ring overlay play, and the safe area and pill shape
// the touch UI borrows. The backdrop lives in GamepadAurora.kt.

// --- The vocabulary -------------------------------------------------------------------------

/** The curve and durations the Compose screens still in console style animate on. */
object ConsoleMotion {
    /**
     * The desktop's `ease_out_cubic` — `1 − (1−t)³`, ANALYTICALLY, not as a Bézier approximation
     * of it (`crates/pf-console-ui/src/anim.rs`).
     *
     * ⚠ Two different curves are published under the name "easeOutCubic" and neither is the real
     * one: the Penner/Ceaser CSS table's `cubic-bezier(0.215, 0.61, 0.355, 1)` and easings.net's
     * `cubic-bezier(0.33, 1, 0.68, 1)`. At the midpoint the true curve is 0.875, the second bezier
     * ≈0.87, and the first ≈0.80 — visibly slacker. Compose's `Easing` is a plain function, so
     * there is no reason to approximate at all; the Apple client uses the second bezier only
     * because SwiftUI's `timingCurve` cannot take a closure.
     */
    val EaseOutCubic = Easing { t ->
        val u = 1f - t
        1f - u * u * u
    }

    /** Screen push/pop, ms — the desktop's `TRANSITION_S` (0.26 s). */
    const val TRANSITION_MS = 260

    /** Focus arriving on a row/field/pill: background, border, chevrons, label colour. */
    const val FOCUS_MS = 160

    /** Everything eased on the console's own curve. */
    fun <T> ease(durationMillis: Int = TRANSITION_MS, delayMillis: Int = 0) =
        tween<T>(durationMillis, delayMillis, EaseOutCubic)
}

/** The console's corner radii. */
object ConsoleShape {
    /** A pill: the legend, a badge. */
    val Pill = RoundedCornerShape(50)
}

/**
 * `false` when the user has turned animations off system-wide (Developer options' animator duration
 * scale, or the accessibility "Remove animations" switch, which sets the same global). Read once
 * per composition — it needs a settings trip to the system, and it changes about never.
 */
@Composable
internal fun animationsEnabled(): Boolean {
    val context = LocalContext.current
    return remember {
        runCatching {
            android.provider.Settings.Global.getFloat(
                context.contentResolver,
                android.provider.Settings.Global.ANIMATOR_DURATION_SCALE,
                1f,
            ) != 0f
        }.getOrDefault(true)
    }
}

// --- Safe area ------------------------------------------------------------------------------

/**
 * The safe area a console screen's CONTENT keeps clear: the system bars UNION the display cutout.
 *
 * `systemBarsPadding()`, which every console screen used to pad with, EXCLUDES
 * `WindowInsets.displayCutout`. In portrait that mostly hides — a top-centre hole punch sits under
 * the status-bar inset anyway — but in landscape a cutout is a LEFT or RIGHT edge inset with no bar
 * behind it (`cutout=[0,162,0,0]` on the Nothing Phone 3, see the dump quoted in MainActivity), so
 * settings rows and add-host fields ran straight under the camera. Material3's own components lay
 * out against `systemBars.union(displayCutout)`; this is that rule, applied to the console screens,
 * which draw their own chrome instead of sitting in a Scaffold.
 *
 * The BACKDROP deliberately does not take it: the aurora is ambience, and running full-bleed under
 * the camera is exactly what ambience should do.
 */
@Composable
fun Modifier.consoleSafeArea(): Modifier =
    windowInsetsPadding(WindowInsets.systemBars.union(WindowInsets.displayCutout))

/**
 * The insets a FLOATING legend takes. In landscape it deliberately ignores the system bars so it
 * hugs the corner rather than the nav-bar inset — but it must still clear the CUTOUT: the console
 * screens are `SENSOR_LANDSCAPE`, so reverse-landscape parks the punch on exactly the corner the
 * legend lives in.
 */
@Composable
fun Modifier.consoleLegendInsets(landscape: Boolean): Modifier =
    if (landscape) windowInsetsPadding(WindowInsets.displayCutout) else consoleSafeArea()

/**
 * The exact inset every console screen places its floating legend at (bottom-start), so the legend
 * sits in the SAME spot across Home / Settings / Add-Host / Library and appears pinned while the
 * content behind it transitions.
 */
val ConsoleLegendInset = PaddingValues(start = 24.dp, end = 24.dp, bottom = 24.dp)

/** The shared horizontal inset for a console screen's heading (matches the legend's left edge). */
val ConsoleEdgeInset = 24.dp

/**
 * How much room a scrolling console list leaves at its bottom for the floating legend to sit over.
 * One constant, so the list's `contentPadding` and the keep-focus-visible scroll margin cannot
 * drift apart.
 */
val ConsoleLegendClearance = 152.dp

// --- Headings ------------------------------------------------------------------------------

/**
 * The heading every console screen uses — one style, one inset, so titles line up across Home /
 * Settings / Add-Host / Library. Callers place it at the top of their content (or float it, on Home).
 */
@Composable
fun ConsoleHeader(title: String, modifier: Modifier = Modifier, horizontalInset: Boolean = true) {
    val ink = LocalGamepadInk.current
    // `horizontalInset = false` when the caller's container already pads to ConsoleEdgeInset (e.g. a
    // LazyColumn contentPadding) — so the heading lands at the SAME 24dp on every screen either way.
    val h = if (horizontalInset) ConsoleEdgeInset else 0.dp
    Text(
        title,
        style = MaterialTheme.typography.headlineMedium,
        fontWeight = FontWeight.Bold,
        color = ink.fg,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier.padding(start = h, end = h, top = 18.dp, bottom = 10.dp),
    )
}

// --- Menu haptics -----------------------------------------------------------------------------

/**
 * The console's menu feel: a tick as the cursor moves, a heavier thud when a press is refused at a
 * boundary, a pulse on confirm. The Apple client's `MenuHaptics` semantics and the desktop's
 * `menu_rumble(pulse)`, on whatever actuator this device actually has.
 *
 * Constructed by [rememberConsoleHaptics], which resolves the actuator in the order that matches
 * where the player's hands are: the DRIVING controller's own vibrator first, then the phone body
 * (which is what a clip-on pad without motors leaves you holding), and nothing at all on a TV — a
 * remote has no motor and a TV box has no body, so both resolve null and every call is a no-op.
 */
class ConsoleHaptics internal constructor(private val vibrator: Vibrator?) {
    /** The cursor moved one step. */
    fun tick() = play(7, 40)

    /** The press was refused — the list ended, or the value is already at its limit. */
    fun boundary() = play(18, 95)

    /** Something was confirmed, cycled, or flipped. */
    fun confirm() = play(12, 70)

    private fun play(durationMs: Long, amplitude: Int) {
        val v = vibrator ?: return
        runCatching {
            v.vibrate(
                if (v.hasAmplitudeControl()) {
                    VibrationEffect.createOneShot(durationMs, amplitude)
                } else {
                    VibrationEffect.createOneShot(durationMs, VibrationEffect.DEFAULT_AMPLITUDE)
                },
            )
        }
    }
}

/** A haptics object that renders nothing — previews, tests, and every device with no actuator. */
private val SilentHaptics = ConsoleHaptics(null)

/**
 * Resolve the console's haptics for whatever is driving the UI right now. Re-resolves when the
 * driving pad changes (a fresh controller may have motors where the last one had none), and honours
 * the system's own "touch feedback" switch — a user who turned haptics off meant it here too.
 */
@Composable
fun rememberConsoleHaptics(): ConsoleHaptics {
    val context = LocalContext.current
    val activity = context as? MainActivity ?: return SilentHaptics
    val padId = activity.lastPadDeviceId
    @Suppress("DEPRECATION") // still the canonical read for the user's touch-feedback switch
    val enabled = remember {
        runCatching {
            android.provider.Settings.System.getInt(
                context.contentResolver,
                android.provider.Settings.System.HAPTIC_FEEDBACK_ENABLED,
                1,
            ) != 0
        }.getOrDefault(true)
    }
    return remember(padId, enabled) {
        if (!enabled) SilentHaptics
        else ConsoleHaptics(padVibrator(padId) ?: deviceBodyVibrator(context))
    }
}

/** The vibrator of the controller with this device id, or null (no such device / no motors). */
private fun padVibrator(deviceId: Int): Vibrator? = runCatching {
    val dev = InputDevice.getDevice(deviceId) ?: return null
    val v = if (Build.VERSION.SDK_INT >= 31) {
        dev.vibratorManager.defaultVibrator
    } else {
        @Suppress("DEPRECATION")
        dev.vibrator
    }
    v?.takeIf { it.hasVibrator() }
}.getOrNull()

// --- Button glyphs and the legend --------------------------------------------------------------

/**
 * One glyph + label cell of a hint bar. [glyph] is the SEMANTIC face letter (the Android
 * `KEYCODE_BUTTON_*` name — 'A' = confirm/south); [color] its Xbox-convention hue. How the pair is
 * actually DRAWN is the hint bar's decision, per the driving controller's [Gamepad.PadStyle] — a
 * DualSense renders 'A' as the ✕ shape, a Switch pad as a monochrome letter. [onClick], when set,
 * makes the cell tappable — a TOUCH escape hatch so a user without a working controller can still
 * drive the console UI (and reach Settings to switch it off).
 */
class GamepadHint(
    val glyph: Char,
    val color: Color,
    val text: String,
    val onClick: (() -> Unit)? = null,
    // Render as the D-pad-centre "select" button (a ring) instead of a lettered face-button disc —
    // for a TV remote, which has no A/B/X/Y.
    val select: Boolean = false,
    // Render as the pad's physical Select/View/Create/− button (per PadStyle) — the button that
    // delivers KEYCODE_BUTTON_SELECT.
    val viewButton: Boolean = false,
)

/**
 * Xbox-convention face-button colours, so the glyphs read at a glance across the room. These are
 * the DEFAULT (Xbox/generic) rendering; the hint bar swaps in PlayStation shapes or Nintendo
 * monochrome per the driving pad's [Gamepad.PadStyle] at draw time.
 */
object PadGlyph {
    val A = Color(0xFF6BBE45)
    val B = Color(0xFFD14B4B)
    val X = Color(0xFF4B7BD1)
    val Y = Color(0xFFE0B23C)

    /** The tint the DIRECTIONAL hints (↔ ⇄ ↑ ↓) wear — not a face button, so not a face colour. */
    val Arrow = Color(0xFF9A93C7)

    fun hint(glyph: Char, text: String, onClick: (() -> Unit)? = null) = GamepadHint(
        glyph, when (glyph) { 'A' -> A; 'B' -> B; 'X' -> X; 'Y' -> Y; else -> Arrow }, text, onClick,
    )
}

/** The dark button-face fill shared by the PlayStation / Nintendo / select-button badges. */
internal val PadButtonFace = Color(0xFF2A2740)

/** A round face-button badge: a coloured disc with the button letter, like a controller's face. */
@Composable
fun GamepadButtonGlyph(glyph: Char, color: Color, size: Dp = 26.dp) {
    val ink = LocalGamepadInk.current
    Box(
        modifier = Modifier
            .size(size)
            .clip(CircleShape)
            .background(color),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            glyph.toString(),
            color = ink.fg,
            fontWeight = FontWeight.Bold,
            fontSize = (size.value * 0.52f).sp,
            textAlign = TextAlign.Center,
        )
    }
}

/** The D-pad-centre "select" button — a green (confirm) disc with a ring; the TV-remote glyph for A. */
@Composable
private fun SelectGlyph(size: Dp = 26.dp) {
    val ink = LocalGamepadInk.current
    Box(
        modifier = Modifier.size(size).clip(CircleShape).background(PadGlyph.A),
        contentAlignment = Alignment.Center,
    ) {
        Box(Modifier.size(size * 0.46f).clip(CircleShape).border(2.dp, ink.fg, CircleShape))
    }
}

/** The remote's "Back" button — a back-arrow disc; the TV-remote glyph for B (back / cancel / done). */
@Composable
private fun BackGlyph(size: Dp = 26.dp) {
    GamepadButtonGlyph('↩', PadGlyph.B, size)
}

/**
 * A PlayStation face button: the dark button face with the coloured shape outline Sony prints on it.
 * Keyed by the SEMANTIC letter (Android keycode name): A = ✕ cross, B = ○ circle, X = □ square,
 * Y = △ triangle — exactly how a Sony pad's buttons map to `KEYCODE_BUTTON_*`, in the classic
 * DualShock colours.
 */
@Composable
internal fun PsFaceGlyph(glyph: Char, size: Dp = 26.dp) {
    val color = when (glyph) {
        'A' -> Color(0xFF7C9CE8) // cross — light blue
        'B' -> Color(0xFFE0736F) // circle — red
        'X' -> Color(0xFFD48FC7) // square — pink
        else -> Color(0xFF5FBFA5) // triangle — green
    }
    Box(
        Modifier.size(size).clip(CircleShape).background(PadButtonFace),
        contentAlignment = Alignment.Center,
    ) {
        Canvas(Modifier.size(size * 0.46f)) {
            val w = this.size.minDimension
            val stroke = Stroke(width = w * 0.17f, cap = StrokeCap.Round, join = StrokeJoin.Round)
            when (glyph) {
                'A' -> { // ✕ — the two diagonals
                    drawLine(color, Offset(0f, 0f), Offset(w, w), stroke.width, StrokeCap.Round)
                    drawLine(color, Offset(w, 0f), Offset(0f, w), stroke.width, StrokeCap.Round)
                }
                'B' -> drawCircle(color, radius = (w - stroke.width) / 2f, style = stroke)
                'X' -> drawRect(
                    color,
                    topLeft = Offset(stroke.width / 2f, stroke.width / 2f),
                    size = Size(w - stroke.width, w - stroke.width),
                    style = stroke,
                )
                else -> { // △
                    val p = Path().apply {
                        moveTo(w / 2f, stroke.width / 2f)
                        lineTo(w - stroke.width / 2f, w - stroke.width / 2f)
                        lineTo(stroke.width / 2f, w - stroke.width / 2f)
                        close()
                    }
                    drawPath(p, color, style = stroke)
                }
            }
        }
    }
}

/**
 * The pad's physical Select-family button — the one that delivers `KEYCODE_BUTTON_SELECT` and opens
 * Options — drawn per [Gamepad.PadStyle] as a badge with the button's real face: Xbox View (two
 * overlapping windows), PlayStation Create/Share (a slim capsule), Nintendo − (minus). The generic
 * fallback wears the capsule too (the near-universal select shape).
 */
@Composable
internal fun SelectButtonGlyph(style: Gamepad.PadStyle, size: Dp = 26.dp) {
    val ink = LocalGamepadInk.current
    Box(
        Modifier.size(size).clip(CircleShape).background(PadButtonFace),
        contentAlignment = Alignment.Center,
    ) {
        when (style) {
            Gamepad.PadStyle.XBOX -> Box(Modifier.size(size * 0.50f)) {
                // The View icon: two overlapping outlined windows; the front one is filled with the
                // button face so it visibly occludes the back one.
                val corner = RoundedCornerShape(2.dp)
                Box(
                    Modifier.size(size * 0.32f).align(Alignment.TopEnd)
                        .border(1.4.dp, ink.fg(0.9f), corner),
                )
                Box(
                    Modifier.size(size * 0.32f).align(Alignment.BottomStart)
                        .clip(corner).background(PadButtonFace)
                        .border(1.4.dp, ink.fg(0.9f), corner),
                )
            }
            Gamepad.PadStyle.NINTENDO -> Text(
                "−",
                color = ink.fg,
                fontWeight = FontWeight.Bold,
                fontSize = (size.value * 0.62f).sp,
                textAlign = TextAlign.Center,
            )
            else -> Box(
                Modifier
                    .size(width = size * 0.58f, height = size * 0.30f)
                    .clip(ConsoleShape.Pill)
                    .border(1.6.dp, ink.fg(0.9f), ConsoleShape.Pill),
            )
        }
    }
}

/**
 * The pinned controls legend every gamepad screen shows along the bottom — worn as a self-contained
 * translucent pill so it floats over the aurora rather than dissolving into it.
 */
@Composable
fun GamepadHintBar(hints: List<GamepadHint>, modifier: Modifier = Modifier, hazeState: HazeState? = null) {
    val ink = LocalGamepadInk.current
    // On a TV D-pad remote (no A/B/X/Y), auto-swap the two universal pad glyphs every screen uses:
    // A (confirm) → the select ring, B (back/cancel) → a back glyph. Screen-specific glyphs like the
    // home's Up/Down handle themselves. A real pad instead picks its glyph FAMILY (Xbox letters /
    // PlayStation shapes / Nintendo monochrome) from the controller that last drove the UI.
    // Defaults to the generic gamepad look off an Activity (preview/tests).
    val activity = LocalContext.current as? MainActivity
    val padIsGamepad = activity?.lastPadIsGamepad ?: true
    val padStyle = activity?.lastPadStyle ?: Gamepad.PadStyle.GENERIC
    val shape = ConsoleShape.Pill
    // With a haze source, blur the content behind the pill (real backdrop blur, API 31+; a translucent
    // scrim below) + a light tint; otherwise fall back to a solid frosted fill.
    val frosted = if (hazeState != null) {
        modifier.clip(shape).hazeEffect(hazeState).background(ink.shade(0.25f))
    } else {
        modifier.clip(shape).background(ink.shade(0.55f))
    }
    Row(
        modifier = frosted
            .border(
                width = 1.dp,
                // The same top-edge highlight the glass rows carry, so the legend belongs to the
                // same material rather than reading as a flat sticker over it.
                brush = Brush.verticalGradient(
                    0f to ink.highlight.copy(alpha = ink.highlight.alpha * 0.7f),
                    0.5f to ink.fg(0.14f),
                    1f to ink.fg(0.14f),
                ),
                shape = shape,
            )
            .padding(horizontal = 16.dp, vertical = 10.dp)
            // The pill still hugs its content when it fits; when it doesn't (a narrow phone, or a
            // screen whose legend grew a cell) it scrolls rather than running off the edge and
            // silently eating the last hint — which is exactly what the settings screen's new
            // Section cell did on a 360 dp phone.
            .horizontalScroll(rememberScrollState()),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(11.dp),
    ) {
        for (h in hints) {
            val cb = h.onClick
            val cell = if (cb != null) {
                Modifier.clip(ConsoleShape.Pill).clickable(onClick = cb).padding(horizontal = 4.dp, vertical = 5.dp)
            } else {
                Modifier
            }
            Row(modifier = cell, verticalAlignment = Alignment.CenterVertically) {
                when {
                    h.viewButton -> SelectButtonGlyph(padStyle)
                    h.select || (!padIsGamepad && h.glyph == 'A') -> SelectGlyph()
                    !padIsGamepad && h.glyph == 'B' -> BackGlyph()
                    padStyle == Gamepad.PadStyle.PLAYSTATION && h.glyph in "ABXY" ->
                        PsFaceGlyph(h.glyph)
                    padStyle == Gamepad.PadStyle.NINTENDO && h.glyph in "ABXY" ->
                        GamepadButtonGlyph(h.glyph, PadButtonFace)
                    else -> GamepadButtonGlyph(h.glyph, h.color)
                }
                Spacer(Modifier.width(6.dp))
                Text(
                    h.text,
                    style = MaterialTheme.typography.labelLarge,
                    color = ink.fg(0.9f),
                    maxLines = 1,
                    softWrap = false, // never char-wrap a label when several hints crowd a narrow pill
                )
            }
        }
    }
}
