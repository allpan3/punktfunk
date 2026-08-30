package io.unom.punktfunk.kit

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Pure JVM tests of [Sc2Device] — the SC2 protocol surface this client shares with the Apple
 * client (`Sc2Device.swift`) and the host (`triton_proto.rs` / `pf_driver_proto::triton`).
 * Run: `./gradlew :kit:testDebugUnitTest`.
 *
 * **Why this file exists: the drift tripwire only pointed one way.** The Apple client's
 * `Sc2DeviceTests.testWireMapMatchesAndroidPairForPair` transcribes THIS file's [Sc2Device]
 * table as a literal, so it catches a Swift-side edit and is blind to a Kotlin-side one — the
 * same asymmetry `pf_driver_proto::triton::out_report_len` warns about in its own doc comment.
 * A `WIRE_MAP` edit here therefore used to leave every existing test green while the two clients
 * silently disagreed about which SC2 bit is `MISC1`, and the host would mirror one client's idea
 * of the pad. The failure is invisible on-glass until someone's paddle presses the wrong thing.
 *
 * So the expectations below are transcribed from the SWIFT side, deliberately, the way the Swift
 * test transcribes this one. Two hand-mirrored tables that must agree; either edit alone goes red.
 */
class Sc2DeviceTest {

    /**
     * The full SC2-bit → `Gamepad.BTN_*` table, pair for pair with `Sc2Device.swift`'s `wireMap`
     * (which is itself pinned against this file). Paddles R4/L4/R5/L5 = PADDLE1..4, QAM = MISC1,
     * right-pad click = the touchpad wire bit — the inverse of the host's typed-fallback mapping
     * in `triton_proto::from_gamepad`.
     */
    private val expected = listOf(
        Sc2Device.A to Gamepad.BTN_A,
        Sc2Device.B to Gamepad.BTN_B,
        Sc2Device.X to Gamepad.BTN_X,
        Sc2Device.Y to Gamepad.BTN_Y,
        Sc2Device.LB to Gamepad.BTN_LB,
        Sc2Device.RB to Gamepad.BTN_RB,
        Sc2Device.VIEW to Gamepad.BTN_BACK,
        Sc2Device.MENU to Gamepad.BTN_START,
        Sc2Device.STEAM to Gamepad.BTN_GUIDE,
        Sc2Device.L3 to Gamepad.BTN_LS_CLICK,
        Sc2Device.R3 to Gamepad.BTN_RS_CLICK,
        Sc2Device.DPAD_UP to Gamepad.BTN_DPAD_UP,
        Sc2Device.DPAD_DOWN to Gamepad.BTN_DPAD_DOWN,
        Sc2Device.DPAD_LEFT to Gamepad.BTN_DPAD_LEFT,
        Sc2Device.DPAD_RIGHT to Gamepad.BTN_DPAD_RIGHT,
        Sc2Device.QAM to Gamepad.BTN_MISC1,
        Sc2Device.R4 to Gamepad.BTN_PADDLE1,
        Sc2Device.L4 to Gamepad.BTN_PADDLE2,
        Sc2Device.R5 to Gamepad.BTN_PADDLE3,
        Sc2Device.L5 to Gamepad.BTN_PADDLE4,
        Sc2Device.RPAD_CLICK to Gamepad.BTN_TOUCHPAD,
    )

    @Test
    fun `wire map matches the Apple client pair for pair`() {
        for ((sc2, wire) in expected) {
            assertEquals("sc2 bit 0x${Integer.toHexString(sc2)}", wire, Sc2Device.wireButtons(sc2))
        }
    }

    @Test
    fun `every mapped bit at once, and nothing else`() {
        val allSc2 = expected.fold(0) { acc, (sc2, _) -> acc or sc2 }
        val allWire = expected.fold(0) { acc, (_, wire) -> acc or wire }
        assertEquals(allWire, Sc2Device.wireButtons(allSc2))
        // Unmapped SC2 bits (trackpad touch, trigger clicks, the left-pad bits) translate to
        // nothing — a new mapping must be added on BOTH clients, so it must fail here first.
        assertEquals(0, Sc2Device.wireButtons(allSc2.inv()))
        assertEquals(0, Sc2Device.wireButtons(0))
    }

    /**
     * A state report of [id]: buttons LE u32 @2, triggers i16 @6/@8, sticks i16 @10..16 — the
     * 46-byte BLE shape (long enough for every offset the parser reads).
     */
    private fun stateReport(
        id: Int = Sc2Device.ID_STATE_BLE,
        buttons: Int = 0,
        lt: Int = 0,
        rt: Int = 0,
        lsX: Int = 0,
        lsY: Int = 0,
        rsX: Int = 0,
        rsY: Int = 0,
    ): ByteArray = ByteArray(46).also {
        it[0] = id.toByte()
        it[2] = buttons.toByte()
        it[3] = (buttons ushr 8).toByte()
        it[4] = (buttons ushr 16).toByte()
        it[5] = (buttons ushr 24).toByte()
        fun i16(o: Int, v: Int) {
            it[o] = v.toByte()
            it[o + 1] = (v shr 8).toByte()
        }
        i16(6, lt); i16(8, rt)
        i16(10, lsX); i16(12, lsY); i16(14, rsX); i16(16, rsY)
    }

    /** The parse contract, case for case with Swift's `testParseStateTruthTable`. */
    @Test
    fun `parse state truth table`() {
        val out = Sc2Device.State()
        val report = stateReport(
            buttons = Sc2Device.A or Sc2Device.STEAM or Sc2Device.RPAD_CLICK,
            lt = 32767, rt = -100, lsX = -32768, lsY = 32767, rsX = 1234, rsY = -1234,
        )
        assertTrue(Sc2Device.parseState(report, report.size, out))
        assertEquals(Sc2Device.A or Sc2Device.STEAM or Sc2Device.RPAD_CLICK, out.buttons)
        assertEquals(255, out.lt) // 32767 >> 7
        assertEquals(0, out.rt) // negative clamps to 0, it does not wrap
        assertEquals(-32768, out.lsX)
        assertEquals(32767, out.lsY)
        assertEquals(1234, out.rsX)
        assertEquals(-1234, out.rsY)

        // All three state shapes parse — identical offsets for everything read here.
        for (id in listOf(Sc2Device.ID_STATE, Sc2Device.ID_STATE_BLE, Sc2Device.ID_STATE_TIMESTAMP)) {
            val r = stateReport(id = id)
            assertTrue("id 0x${Integer.toHexString(id)}", Sc2Device.parseState(r, r.size, out))
        }
        // Non-state ids and short reports answer false (battery/status still ride the RAW plane;
        // they simply have no typed mirror).
        val battery = stateReport(id = Sc2Device.ID_BATTERY)
        assertFalse(Sc2Device.parseState(battery, battery.size, out))
        val short = stateReport()
        assertFalse(Sc2Device.parseState(short, 17, out))
    }

    /**
     * The two initialization feature reports, byte for byte — the Apple client sends the
     * identical 64-byte zero-padded frames (`Sc2Device.disableLizard` / its USB
     * `normalizeJoysticks`), and the firmware accepts the padded form.
     */
    @Test
    fun `feature command bytes verbatim`() {
        assertEquals(64, Sc2Device.DISABLE_LIZARD.size)
        // [id 1][ID_SET_SETTINGS_VALUES 0x87][len 3][SETTING_LIZARD_MODE 9][LIZARD_MODE_OFF u16]
        assertArrayEquals(
            byteArrayOf(0x01, 0x87.toByte(), 0x03, 0x09, 0x00, 0x00),
            Sc2Device.DISABLE_LIZARD.copyOf(6),
        )
        assertEquals(64, Sc2Device.NORMALIZE_JOYSTICKS.size)
        // …[SETTING_ENABLE_RAW_JOYSTICK 0x2E][0 u16] — without it a controller previously opened
        // in raw mode reports ADC coordinates (~0..3200), a few percent of full travel.
        assertArrayEquals(
            byteArrayOf(0x01, 0x87.toByte(), 0x03, 0x2E, 0x00, 0x00),
            Sc2Device.NORMALIZE_JOYSTICKS.copyOf(6),
        )
        // Both are pure padding past the command — a stray byte would be sent to the firmware.
        assertTrue(Sc2Device.DISABLE_LIZARD.drop(6).all { it == 0.toByte() })
        assertTrue(Sc2Device.NORMALIZE_JOYSTICKS.drop(6).all { it == 0.toByte() })
    }
}
