package io.unom.punktfunk

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** One overlay line from the shared formatter: [role] 0 primary, 1 detail, 2 muted, 3 warning. */
internal data class HudLine(val role: Int, val text: String)

/** `<role>\t<text>\n` per line, as `nativeVideoStatsLines` returns it. Malformed lines drop. */
internal fun decodeHudLines(encoded: String?): List<HudLine> =
    encoded.orEmpty().lineSequence().mapNotNull { line ->
        val tab = line.indexOf('\t')
        if (tab <= 0) null else HudLine(line.substring(0, tab).toIntOrNull() ?: 0, line.substring(tab + 1))
    }.toList()

/**
 * The live stats overlay: the lines `punktfunk_core::hud` built for this window, painted by role.
 * The tier, the vocabulary and every label are decided natively, the same as on every other
 * client; this only draws them.
 */
@Composable
internal fun StatsOverlay(lines: List<HudLine>, modifier: Modifier = Modifier) {
    if (lines.isEmpty()) return
    Column(
        modifier = modifier
            .background(Color.Black.copy(alpha = 0.45f), RoundedCornerShape(6.dp))
            .padding(horizontal = 8.dp, vertical = 4.dp),
    ) {
        lines.forEach { statLine(it.text, roleColor(it.role)) }
    }
}

internal fun roleColor(role: Int): Color = when (role) {
    1 -> Color(0xFFB0D0FF)
    2 -> Color(0xFF9AA6B8)
    3 -> Color(0xFFFFD9A0)
    else -> Color.White
}

/**
 * One monospace HUD line — the shared type ramp so every line lines up. Line height and tracking
 * are pinned: the theme's `bodyLarge` would set 12 sp text on a 24 sp line.
 */
@Composable
private fun statLine(text: String, color: Color) {
    Text(
        text, color = color, fontFamily = FontFamily.Monospace, fontSize = 12.sp,
        lineHeight = 16.sp, letterSpacing = 0.sp,
    )
}
