package io.unom.punktfunk

import org.junit.Assert.assertEquals
import org.junit.Test

/** [bitrateLabel] words any rate the way the console shell's `bitrate_label` does (#1122). */
class BitrateLabelTest {
    @Test
    fun anyRateReadsAsItself() {
        assertEquals("14 Mbps", bitrateLabel(14_000))
        assertEquals("14.4 Mbps", bitrateLabel(14_350)) // a speed-test recommendation
        assertEquals("1.5 Mbps", bitrateLabel(1_500))
        assertEquals("2 Gbps", bitrateLabel(2_000_000))
        assertEquals("1.5 Gbps", bitrateLabel(1_500_000))
    }
}
