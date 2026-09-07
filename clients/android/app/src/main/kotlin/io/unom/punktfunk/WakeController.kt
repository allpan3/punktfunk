package io.unom.punktfunk

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.unom.punktfunk.kit.NativeBridge
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * Wake a sleeping host and WAIT for it to come back before proceeding — the Android mirror of the
 * Apple client's `HostWaker`.
 *
 * A magic packet is fire-and-forget, and a cold box can take 20–60 s to POST, boot, and start
 * answering again — far longer than a connect attempt will sit. So instead of firing one packet and
 * immediately dialing (which just fails on a genuinely-asleep host), this drives a visible "Waking…"
 * state: it polls [isOnline] about once a second, (re-)sends the packet while that stays false, and
 * on success runs [onOnline] (the real connect for a Wake-&-Connect, or nothing for a wake-only);
 * on timeout it parks in a retry/cancel state. One wake at a time.
 *
 * [isOnline] suspends because the only trustworthy answer costs a round trip: mDNS presence is a
 * cache a sleeping host keeps warm for up to 75 minutes, so waiting on it both returned true for a
 * host that never woke and never returned true for a routed host that does not advertise at all.
 *
 * [scope] is the composition's coroutine scope (main-dispatched), so [waking] mutations and the
 * [onOnline] callback run on the main thread; the probe and the blocking send are off-loaded.
 */
class WakeController(private val scope: CoroutineScope) {
    /** null = idle; non-null drives the "Waking…" phase of [ConnectOverlay]. */
    data class Waking(
        val hostName: String,
        /** Whether coming online chains into a connect (Wake & Connect) vs. just stopping. */
        val connectsAfter: Boolean,
        val seconds: Int = 0,
        val timedOut: Boolean = false,
    )

    var waking by mutableStateOf<Waking?>(null)
        private set

    private var loop: Job? = null

    /** Captured so "Try Again" replays the exact same wait. */
    private var replay: (() -> Unit)? = null

    /**
     * Wake the host and wait for [isOnline] to go true, then run [onOnline]. [macs]/[lastIp] target
     * the magic packet. No-ops straight to [onOnline] when there's nothing to wake with; a host
     * that turns out to be up already (a race with the caller's check) falls out of the loop's
     * first pass, before any packet is sent.
     */
    fun start(
        hostName: String,
        connectsAfter: Boolean,
        macs: List<String>,
        lastIp: String,
        isOnline: suspend () -> Boolean,
        onOnline: () -> Unit,
    ) {
        if (macs.isEmpty()) {
            cancel()
            onOnline()
            return
        }
        replay = { run(hostName, connectsAfter, macs, lastIp, isOnline, onOnline) }
        replay?.invoke()
    }

    /** Stop waiting and dismiss the overlay (B / Cancel). */
    fun cancel() {
        loop?.cancel()
        loop = null
        replay = null
        waking = null
    }

    /** Restart the wait after a timeout (A / Try Again). */
    fun retry() {
        replay?.invoke()
    }

    private fun run(
        hostName: String,
        connectsAfter: Boolean,
        macs: List<String>,
        lastIp: String,
        isOnline: suspend () -> Boolean,
        onOnline: () -> Unit,
    ) {
        loop?.cancel()
        waking = Waking(hostName = hostName, connectsAfter = connectsAfter)
        loop = scope.launch {
            // Wall-clock, not a lap count: one [isOnline] costs a probe round trip, so laps are
            // longer than the delay and a counted one would stretch both the timeout and the
            // seconds this shows.
            val started = android.os.SystemClock.elapsedRealtime()
            fun elapsed() = ((android.os.SystemClock.elapsedRealtime() - started) / 1000).toInt()
            var sentAt: Int? = null
            while (isActive) {
                if (isOnline()) {
                    waking = null
                    loop = null
                    onOnline()
                    return@launch
                }
                if (elapsed() >= TIMEOUT_S) {
                    waking = waking?.copy(timedOut = true)
                    loop = null
                    return@launch
                }
                // Checked before sent, so a host that is already up never gets a packet. Re-sent on
                // a cadence because a single one can be missed, and some NICs only wake on a fresh
                // packet after dropping into a deeper sleep state.
                if (sentAt == null || elapsed() - sentAt >= RESEND_EVERY_S) {
                    sentAt = elapsed()
                    val csv = macs.joinToString(",")
                    launch(Dispatchers.IO) { NativeBridge.nativeWakeOnLan(csv, lastIp) }
                }
                delay(1000)
                waking = waking?.copy(seconds = elapsed())
            }
        }
    }

    companion object {
        /** How long to wait for the host to reappear before giving up (a cold boot can be a minute+). */
        const val TIMEOUT_S = 90

        /** Re-send the magic packet this often. */
        const val RESEND_EVERY_S = 6
    }
}
