package io.unom.punktfunk

import android.os.SystemClock
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.platform.LocalContext
import io.unom.punktfunk.kit.Gamepad
import kotlin.math.abs
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive

// Controller navigation for the Compose screens still in console style (the licences screen). It
// taps the MainActivity input probes (padMotionProbe / padKeyProbe) so it sees the raw analog stick
// before MainActivity's stick→D-pad focus synthesis, which is edge-only. The left stick drives
// discrete moves with hysteresis (fire once past HIGH; re-arm only under LOW, so a flick is one
// move) and auto-repeat while held.

private const val STICK_HIGH = 0.6f   // cross this to commit a move
private const val STICK_LOW = 0.3f    // fall back under this to re-arm (hysteresis)
private const val INITIAL_DELAY_MS = 420L // hold this long before the first auto-repeat
private const val REPEAT_MS = 150L        // then repeat this often while held

private class NavInputState {
    @Volatile var stickX = 0f
    @Volatile var stickY = 0f
    @Volatile var hatX = 0f
    @Volatile var hatY = 0f
    @Volatile var dpadX = 0
    @Volatile var dpadY = 0
    fun reset() { stickX = 0f; stickY = 0f; hatX = 0f; hatY = 0f; dpadX = 0; dpadY = 0 }
}

/** A committed navigation direction from the stick / D-pad / HAT. */
enum class NavDir { UP, DOWN, LEFT, RIGHT }

/** What the input state resolves to on one tick of the repeat loop. */
private sealed interface Resolved<out D> {
    /** Past the commit threshold in direction [dir]. */
    data class Dir<D>(val dir: D) : Resolved<D>

    /** Back near centre: re-arm. */
    data object Centre : Resolved<Nothing>

    /** Inside the hysteresis band: hold whatever is committed, fire nothing. */
    data object Band : Resolved<Nothing>
}

/**
 * 2-D controller navigation with hysteresis and hold-to-repeat: the dominant stick axis (or the
 * pressed D-pad/HAT) commits a [NavDir], and it re-arms only after the stick returns near centre, so
 * a flick is one step. [onActivate] is A / center, [onTertiary] is X, [onSecondary] is Y, and
 * [onShoulder] is L1 (-1) / R1 (+1). B is left to MainActivity's BACK remap → the screen's
 * BackHandler.
 */
@Composable
fun GamepadNavEffect2D(
    active: Boolean,
    onDirection: (NavDir) -> Unit,
    onActivate: () -> Unit,
    onTertiary: () -> Unit = {},
    onSecondary: () -> Unit = {},
    onShoulder: (Int) -> Unit = {},
) {
    val currentOnShoulder by rememberUpdatedState(onShoulder)
    val haptics by rememberUpdatedState(rememberConsoleHaptics())
    PadNavCore(
        active = active,
        onStep = onDirection,
        onActivate = onActivate,
        onSecondary = onSecondary,
        onTertiary = onTertiary,
        extraKeys = { code, _, edge ->
            when (code) {
                // Edge-only, no auto-repeat: a held shoulder shouldn't spin through the tabs.
                KeyEvent.KEYCODE_BUTTON_L1 -> { if (edge) { haptics.tick(); currentOnShoulder(-1) }; true }
                KeyEvent.KEYCODE_BUTTON_R1 -> { if (edge) { haptics.tick(); currentOnShoulder(1) }; true }
                else -> false
            }
        },
        vertical = true,
        resolve = { s, _ ->
            resolveDir(s)?.let { Resolved.Dir(it) }
                ?: if (
                    s.dpadX == 0 && s.dpadY == 0 &&
                    abs(s.hatX) < 0.5f && abs(s.hatY) < 0.5f &&
                    abs(s.stickX) < STICK_LOW && abs(s.stickY) < STICK_LOW
                ) Resolved.Centre else Resolved.Band
        },
    )
}

/**
 * The shared half of both effects: one entry on the MainActivity probe stack while [active]
 * (removed by identity on dispose — a cross-fading-out screen must take only its OWN claim, never
 * the incoming screen's, and never the console shell's underneath), the face buttons everyone
 * shares, and the hysteresis + hold-to-repeat loop over [resolve]. [extraKeys] claims the keys a
 * variant owns before the shared set sees them; [vertical] routes D-pad Up/Down into the state
 * instead of leaving them to [extraKeys].
 */
@Composable
private fun <D : Any> PadNavCore(
    active: Boolean,
    onStep: (D) -> Unit,
    onActivate: () -> Unit,
    onSecondary: () -> Unit,
    onTertiary: () -> Unit,
    extraKeys: (code: Int, down: Boolean, edge: Boolean) -> Boolean,
    resolve: (NavInputState, committed: D?) -> Resolved<D>,
    vertical: Boolean = false,
) {
    val activity = LocalContext.current as? MainActivity ?: return
    val state = remember { NavInputState() }
    // Menu feel, inherited by every console screen that navigates through here rather than wired
    // per screen: a tick as the cursor steps, a pulse on confirm. Renders on the driving pad's own
    // motors, the phone body if it has none, and nothing at all on a TV.
    val haptics by rememberUpdatedState(rememberConsoleHaptics())
    // The effects below are keyed on `active` only (they must NOT restart on every recomposition), so
    // they'd otherwise capture the FIRST callbacks — closing over a stale `tiles` (fewer hosts than are
    // discovered later, which clamped navigation to that old count). rememberUpdatedState keeps the
    // long-lived coroutine/probes pointed at the CURRENT callbacks.
    val currentOnStep by rememberUpdatedState(onStep)
    val currentOnActivate by rememberUpdatedState(onActivate)
    val currentOnSecondary by rememberUpdatedState(onSecondary)
    val currentOnTertiary by rememberUpdatedState(onTertiary)
    val currentExtraKeys by rememberUpdatedState(extraKeys)
    val currentResolve by rememberUpdatedState(resolve)

    DisposableEffect(active) {
        val motionProbe: (MotionEvent) -> Boolean = probe@{ ev ->
            if (ev.isFromSource(InputDevice.SOURCE_JOYSTICK) && ev.actionMasked == MotionEvent.ACTION_MOVE) {
                state.stickX = ev.getAxisValue(MotionEvent.AXIS_X)
                state.stickY = ev.getAxisValue(MotionEvent.AXIS_Y)
                state.hatX = ev.getAxisValue(MotionEvent.AXIS_HAT_X)
                state.hatY = ev.getAxisValue(MotionEvent.AXIS_HAT_Y)
                return@probe true // consume → MainActivity's stick→D-pad synthesis stays out of it
            }
            false
        }
        val keyProbe: (KeyEvent) -> Boolean = probe@{ ev ->
            val down = ev.action == KeyEvent.ACTION_DOWN
            val edge = down && ev.repeatCount == 0
            val code = Gamepad.padKeyCode(ev)
            if (currentExtraKeys(code, down, edge)) return@probe true
            when (code) {
                KeyEvent.KEYCODE_DPAD_LEFT -> { state.dpadX = if (down) -1 else 0; true }
                KeyEvent.KEYCODE_DPAD_RIGHT -> { state.dpadX = if (down) 1 else 0; true }
                KeyEvent.KEYCODE_DPAD_UP -> if (vertical) { state.dpadY = if (down) -1 else 0; true } else false
                KeyEvent.KEYCODE_DPAD_DOWN -> if (vertical) { state.dpadY = if (down) 1 else 0; true } else false
                KeyEvent.KEYCODE_BUTTON_A, KeyEvent.KEYCODE_DPAD_CENTER,
                KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> {
                    if (edge) { haptics.confirm(); currentOnActivate() }
                    true
                }
                KeyEvent.KEYCODE_BUTTON_X -> { if (edge) currentOnTertiary(); true }
                KeyEvent.KEYCODE_BUTTON_Y -> { if (edge) currentOnSecondary(); true }
                else -> false // B / shoulders / etc. → MainActivity handles (B remaps to BACK)
            }
        }
        val probes = if (active) MainActivity.PadProbes(keyProbe, motionProbe) else null
        probes?.let { activity.pushPadProbes(it) }
        onDispose {
            probes?.let { activity.removePadProbes(it) }
            state.reset()
        }
    }

    LaunchedEffect(active) {
        if (!active) return@LaunchedEffect
        var committed: D? = null // the direction currently held (hysteresis + repeat authority)
        var fireAt = 0L          // uptime at/after which the next auto-repeat may fire
        while (isActive) {
            val now = SystemClock.uptimeMillis()
            when (val r = currentResolve(state, committed)) {
                Resolved.Centre -> committed = null
                Resolved.Band -> {}
                is Resolved.Dir -> when {
                    r.dir != committed -> {
                        haptics.tick(); currentOnStep(r.dir); committed = r.dir; fireAt = now + INITIAL_DELAY_MS
                    }
                    now >= fireAt -> { haptics.tick(); currentOnStep(r.dir); fireAt = now + REPEAT_MS }
                }
            }
            delay(16)
        }
    }
}

/** The direction currently past the commit threshold (D-pad/HAT first, then the dominant stick axis). */
private fun resolveDir(s: NavInputState): NavDir? {
    if (s.dpadY < 0) return NavDir.UP
    if (s.dpadY > 0) return NavDir.DOWN
    if (s.dpadX < 0) return NavDir.LEFT
    if (s.dpadX > 0) return NavDir.RIGHT
    if (s.hatY <= -0.5f) return NavDir.UP
    if (s.hatY >= 0.5f) return NavDir.DOWN
    if (s.hatX <= -0.5f) return NavDir.LEFT
    if (s.hatX >= 0.5f) return NavDir.RIGHT
    // Horizontal wins an exact |x| == |y| diagonal tie (Y must be strictly greater to take the
    // vertical branch), matching the SDL core and Apple nav so a perfect 45° push resolves the
    // same on every client.
    return if (abs(s.stickY) > abs(s.stickX)) {
        when {
            s.stickY <= -STICK_HIGH -> NavDir.UP
            s.stickY >= STICK_HIGH -> NavDir.DOWN
            else -> null
        }
    } else {
        when {
            s.stickX <= -STICK_HIGH -> NavDir.LEFT
            s.stickX >= STICK_HIGH -> NavDir.RIGHT
            else -> null
        }
    }
}
