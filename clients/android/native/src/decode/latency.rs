//! Decode-latency bookkeeping: realtime clock + decoded-pts / user-flags stat recording.

use punktfunk_core::client::NativeClient;
use punktfunk_core::session::Frame;
use std::collections::VecDeque;
use std::time::Duration;

use super::PENDING_SPLIT_CAP;

/// Wall-clock now in nanoseconds (CLOCK_REALTIME basis), to compare against the host-stamped
/// capture `pts_ns` after the skew offset is applied.
pub(crate) fn now_realtime_ns() -> i128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

/// HUD `decoded` point for one dequeued output frame, keyed by the echoed `presentationTimeUs`:
/// build the end-to-end (capture→decoded, skew-corrected, clamped to (0, 10 s)) and `decode`
/// (received→decoded, single-clock local, ≥ 0) samples and hand them to
/// [`crate::stats::VideoStats::note_decoded`]. The pts keys the receipt stamp in `in_flight`;
/// entries older than it are evicted (decode order == input order here — low-latency, no
/// B-frames — so anything before it was dropped inside the codec or stamped before a flush).
/// `decoded_ns` is the availability instant: the dequeue (sync loop) or the output callback's
/// stamp (async loop). Returns the receipt stamp it paired (if any) so the caller can split the
/// `decode` stage further (feed wait vs codec-pure) without re-walking the map.
pub(super) fn note_decoded_pts(
    client: &NativeClient,
    measure_decode: bool,
    stats: &crate::stats::VideoStats,
    in_flight: &mut VecDeque<(u64, i128)>,
    clock_offset: i64,
    pts_us: u64,
    decoded_ns: i128,
) -> Option<i128> {
    // Pair the echoed pts back to its receipt stamp, evicting stale (older) entries as we go.
    let mut received_ns = None;
    while let Some(&(p, r)) = in_flight.front() {
        if p > pts_us {
            break; // future frame — leave it for its own output buffer
        }
        in_flight.pop_front();
        if p == pts_us {
            received_ns = Some(r);
            break;
        }
    }
    let decode_us = received_ns.map(|r| ((decoded_ns - r).max(0) / 1000) as u64);
    // Adaptive bitrate: the `decode` stage (received→decoded, single-clock local) IS the decoder-
    // backlog signal — the only bottleneck the host-side network signals can't see (a fast LAN
    // feeding a slower mobile decoder). Report it whenever the controller is armed, regardless of
    // the HUD; `report_decode_us` is a cheap accumulate the pump windows.
    if measure_decode {
        if let Some(us) = decode_us {
            client.report_decode_us(us.min(u32::MAX as u64) as u32);
        }
    }
    // HUD histogram: only while the overlay is visible (a measure-only caller enters here for the
    // ABR report alone). `end-to-end` = capture→decoded (skew-corrected) tiles the `decode` stage.
    // pts_us is the truncated frame.pts_ns/1000 we queued, so ×1000 re-approximates capture time to
    // < 1 µs — negligible against the ms-scale figures shown.
    if stats.enabled() {
        let e2e_ns = decoded_ns + clock_offset as i128 - pts_us as i128 * 1000;
        let e2e_us = (e2e_ns > 0 && e2e_ns < 10_000_000_000).then_some((e2e_ns / 1000) as u64);
        stats.note_decoded(e2e_us, decode_us);
    }
    received_ns
}

/// The queued-instant stamp for a decoded output, keyed by the echoed `presentationTimeUs` — the
/// same monotonic evict-as-you-go pairing as [`take_flags`], over an `(pts_us, realtime_ns)` map
/// (the feed side stamps each AU as its last piece enters the codec). A miss returns `None` —
/// the split is simply not recorded for that frame.
pub(super) fn take_stamp(map: &mut VecDeque<(u64, i128)>, pts_us: u64) -> Option<i128> {
    while let Some(&(p, t)) = map.front() {
        if p > pts_us {
            break; // future frame — leave it for its own output buffer
        }
        map.pop_front();
        if p == pts_us {
            return Some(t);
        }
    }
    None
}

/// The AU `user_flags` for a decoded output, keyed by the echoed `presentationTimeUs`. Recovery
/// signalling (FLAG_SOF IDR marker / RECOVERY_ANCHOR / RECOVERY_POINT) rides the AU's flags, which are
/// only in scope at feed time — so the feed side parks `(pts_us, flags)` here and the present side
/// looks them up to fold [`ReanchorGate::on_decoded`]. Decode order == input order (low-latency, no
/// B-frames), so this evicts entries older than `pts_us` as it goes; a miss (probe filler, or an entry
/// aged past the cap) reads `0` — no recovery flags, decoded normally.
pub(super) fn take_flags(map: &mut VecDeque<(u64, u32)>, pts_us: u64) -> u32 {
    while let Some(&(p, f)) = map.front() {
        if p > pts_us {
            break; // future frame — leave it for its own output buffer
        }
        map.pop_front();
        if p == pts_us {
            return f;
        }
    }
    0
}

/// p50/max of an unsorted µs sample vec, in ms — the HUD's per-stage summary, shared by both
/// presenters. `(0, 0)` when empty.
pub(super) fn p50_max_ms(mut v: Vec<u64>) -> (f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    v.sort_unstable();
    (
        v[v.len() / 2] as f64 / 1000.0,
        v[v.len() - 1] as f64 / 1000.0,
    )
}

/// The `received` point for one arriving AU, for both decode loops: the core's reassembly stamp,
/// the HUD's capture→received sample, and the 0xCF host/network split parked against it. Returns
/// the stamp, which is all each loop needs to key its own in-flight map — the async one holds that
/// map behind a mutex, the sync one owns it outright, and that is the only part not shared.
///
/// Shared because it drifted. Both loops carried these ~35 lines by hand, and the parts-stream byte
/// count that restores a split AU's real size reached only the async one, so the low-latency escape
/// hatch under-reported bitrate for a release; the phase-lock readout had drifted the same way.
/// Sampling stays gated on the HUD — a hidden overlay pays one wall-clock read and nothing else.
pub(super) fn note_received_frame(
    client: &NativeClient,
    stats: &crate::stats::VideoStats,
    frame: &Frame,
    clock_offset: i64,
    pending_split: &mut VecDeque<(u64, u64)>,
    last_phase_ack: &mut Option<i32>,
) -> i128 {
    // Core reassembly-completion stamp (ABI v9), NOT the pull instant: stamping at the pull would
    // fold the hand-off queue wait into the network figure. 0 = older core.
    let received_ns = if frame.received_ns > 0 {
        frame.received_ns as i128
    } else {
        now_realtime_ns()
    };
    if !stats.enabled() {
        return received_ns;
    }
    // `host+network` = client_now + (host−client) − capture_pts.
    let lat_ns = received_ns + clock_offset as i128 - frame.pts_ns as i128;
    let lat_us = (lat_ns > 0 && lat_ns < 10_000_000_000).then_some((lat_ns / 1000) as u64);
    // On a parts stream the completing delivery carries only the AU's suffix — its offset
    // restores the full AU byte count for bitrate.
    let au_len = frame.part.map_or(0, |p| p.offset as usize) + frame.data.len();
    stats.note_received(au_len, lat_us, clock_offset != 0);
    // Phase-2 split: park this AU's capture→received sample, then match any 0xCF host timings that
    // arrived — host = the host's capture→sent, network = ours minus it (saturating, for clock jitter).
    if let Some(hostnet_us) = lat_us {
        pending_split.push_back((frame.pts_ns, hostnet_us));
        if pending_split.len() > PENDING_SPLIT_CAP {
            pending_split.pop_front(); // 0xCF lost / old host — evict
        }
    }
    while let Ok(t) = client.next_host_timing(Duration::ZERO) {
        // Phase-lock closed-loop readout: the host's applied hold rides the 0xCF tail. Logged on
        // change so `adb logcat -s pf.phase` shows the loop working; None = a pre-phase-lock host.
        if t.applied_phase_ns != *last_phase_ack {
            log::info!(
                target: "pf.phase",
                "host applied_phase={:?}us",
                t.applied_phase_ns.map(|n| n / 1000)
            );
            *last_phase_ack = t.applied_phase_ns;
        }
        if let Some(i) = pending_split.iter().position(|&(p, _)| p == t.pts_ns) {
            let (_, hostnet_us) = pending_split.remove(i).unwrap();
            stats.note_host_split(
                t.host_us as u64,
                hostnet_us.saturating_sub(t.host_us as u64),
            );
        }
    }
    received_ns
}
