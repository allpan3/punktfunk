//! The encode thread's steady state ([`Drive`]): pool slot → `submit` → `poll` → heap + slot
//! table → `latest` + event, with back-pressure taken on pool slots and the wedge state word
//! kept for the host's classifier.

use std::collections::VecDeque;
use std::mem::offset_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pf_driver_proto::encode::FrameToken;
use pf_driver_proto::encode::au::{self, AuHeader, AuSlot, HeapRing};
use pf_encode_win::{AuChunk, Encoder};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{WaitForMultipleObjects, WaitForSingleObject};

use super::pool::{Offer, Pool};
use super::section::{Ctl, EncodeSession};
use super::thread::{hdr_meta, qpc_frequency, qpc_now, qpc_to_ns};

/// Submits allowed ahead of the oldest AU — the host's pipeline depth.
const MAX_INFLIGHT: usize = 2;
/// Polls that return nothing while an AU is owed, before the state word says WEDGED.
const WEDGE_AFTER: Duration = Duration::from_secs(2);
/// How long a produced chunk waits for heap space or a free slot before the AU is dropped and
/// a keyframe requested; longer would stall the encoder behind a host that stopped reading.
const SLOT_WAIT: Duration = Duration::from_millis(250);
/// Consecutive failed submits before the thread gives up on its backend.
const MAX_SUBMIT_FAILURES: u32 = 8;

/// G3 fault injection: encode this many frames, then never return from the encode work
/// (`PFVD_ENCODE_BLOCK_AFTER`, a frame count; unset or unparsable disables it). Read once per
/// encoder open, so a `reset` reopens blocked again until the knob is cleared.
#[cfg(feature = "encode-probe")]
fn block_after() -> Option<u64> {
    crate::log::knob("PFVD_ENCODE_BLOCK_AFTER")?
        .trim()
        .parse()
        .ok()
}

/// Park forever, holding the pool slot and the session, once `encoded` passes the knob. Nothing
/// unparks this thread: the drain worker has to keep composing against a thread that is gone.
#[cfg(feature = "encode-probe")]
fn block_if_armed(limit: Option<u64>, encoded: u64) {
    if limit.is_some_and(|n| encoded > n) {
        dbglog!("[pf-vd] encode: PFVD_ENCODE_BLOCK_AFTER — wedging at frame {encoded}");
        loop {
            std::thread::park();
        }
    }
}

fn stop_signalled(stop: HANDLE) -> bool {
    // SAFETY: `stop` is the worker's stop event, alive until the worker joins or leaks.
    unsafe { WaitForSingleObject(stop, 0) == WAIT_OBJECT_0 }
}

impl<'a> Drive<'a> {
    pub fn new(
        enc: Box<dyn Encoder>,
        pool: &'a Pool,
        session: &'a EncodeSession,
        stop: HANDLE,
        live: &'a AtomicBool,
        fps: u32,
    ) -> Self {
        let (heap_offset, heap_bytes) = session.section.heap();
        Self {
            enc,
            pool,
            session,
            ring: HeapRing::new(heap_offset, heap_bytes),
            wire_seq: session.wire_seq_base.load(Ordering::Acquire),
            publish_seq: 0,
            inflight: VecDeque::new(),
            mid_au: false,
            dropping_au: false,
            au_published: false,
            submit_failures: 0,
            want_republish: false,
            qpc_hz: qpc_frequency(),
            frame_interval: Duration::from_micros(1_000_000 / u64::from(fps.max(1))),
            last_cursor: None,
            state: au::ENCODER_OPEN,
            stop,
            live,
        }
    }

    /// A cursor-only frame when the pointer moved but nothing composed, capped to the refresh so
    /// a 1 kHz mouse cannot outrun the encoder — between caps the mark coalesces to the latest
    /// position. `None` when no move is pending, the cap has not elapsed, or the re-encode was
    /// pre-empted by a queued composed frame.
    fn cursor_frame(&mut self) -> Option<(usize, u64, u64)> {
        if !self.pool.cursor_pending() {
            return None;
        }
        if self
            .last_cursor
            .is_some_and(|t| t.elapsed() < self.frame_interval)
        {
            return None;
        }
        let framed = self.pool.cursor_republish()?;
        self.last_cursor = Some(Instant::now());
        Some(framed)
    }
}

/// The steady state: pool slot → `submit` → `poll` → heap + slot table → `latest` + event.
///
/// Idle desktop: nothing is re-encoded at cadence — a pool with no new frame means no AU, and
/// the host, which reads `source_seq` standing still with `drain_heartbeat_qpc` moving, kicks
/// a compose when it wants one. The stash rule (§2.2) covers only the first frame.
pub struct Drive<'a> {
    enc: Box<dyn Encoder>,
    pool: &'a Pool,
    session: &'a EncodeSession,
    ring: HeapRing,
    /// Next wire index to hand `submit_indexed`; every submit takes exactly one, so the index
    /// the encoder names in an RFI is the index the AU is published under.
    wire_seq: u32,
    /// Publish-token sequence, per session thread.
    publish_seq: u32,
    /// `(slot, qpc, source_seq, wire index)` of every frame whose AU is owed, in submit order.
    inflight: VecDeque<(usize, u64, u64, u32)>,
    /// A partially drained AU must finish through `poll_chunk`.
    mid_au: bool,
    /// The AU in progress could not be placed; its remaining chunks are dropped too.
    dropping_au: bool,
    /// A chunk of the AU in progress reached the section, so its wire index is spent even if
    /// the tail is dropped — the reader closes it as a gap, never splices the next AU on.
    au_published: bool,
    /// Consecutive `submit` failures; see [`MAX_SUBMIT_FAILURES`].
    submit_failures: u32,
    /// A keyframe was asked for; if nothing composes, re-encode the stash instead of waiting.
    want_republish: bool,
    qpc_hz: u64,
    /// The display's frame period — the floor between cursor-only re-encodes.
    frame_interval: Duration,
    /// When the last cursor-only frame went out; `None` before the first.
    last_cursor: Option<Instant>,
    state: u32,
    stop: HANDLE,
    live: &'a AtomicBool,
}

impl Drive<'_> {
    fn stopped(&self) -> bool {
        !self.live.load(Ordering::Acquire) || stop_signalled(self.stop)
    }

    fn set_state(&mut self, state: u32) {
        if self.state != state && self.live.load(Ordering::Acquire) {
            self.state = state;
            self.session
                .section
                .store_u32(offset_of!(AuHeader, encoder_state), state);
        }
    }

    pub fn run(&mut self) {
        self.pool.set_live(true);
        #[cfg(feature = "encode-probe")]
        let (block, mut encoded) = (block_after(), 0u64);
        while !self.stopped() {
            self.drain_ctl();
            // Nothing composed and a client is waiting on a keyframe: re-encode the stash, or
            // the request sits on a frame that never arrives (an idle desktop under a client
            // that draws its own pointer dirties nothing at all).
            let next = self
                .pool
                .take_full()
                .or_else(|| self.want_republish.then(|| self.pool.republish()).flatten())
                .or_else(|| self.cursor_frame());
            self.want_republish = false;
            let Some((slot, qpc, seq)) = next else {
                // Nothing to submit: drain what is owed now, or the last AU of a burst waits
                // for the next compose, which an idle desktop never makes.
                if !self.inflight.is_empty() {
                    self.collect(1);
                }
                self.wait();
                continue;
            };
            // Back-pressure lands here, where dropping is free: no free AU slot, no submit.
            if !self.section_has_free() {
                self.pool.release(slot);
                self.count_drop();
                continue;
            }
            let pts = qpc_to_ns(if qpc == 0 { qpc_now() } else { qpc }, self.qpc_hz);
            let frame = match self.pool.frame(slot, pts) {
                Ok(f) => f,
                Err(_) => {
                    self.pool.release(slot);
                    self.count_drop();
                    continue;
                }
            };
            #[cfg(feature = "encode-probe")]
            {
                encoded += 1;
                block_if_armed(block, encoded);
            }
            let index = self.wire_seq;
            if let Err(e) = self.enc.submit_indexed(&frame, index) {
                dbglog!("[pf-vd] encode: submit failed: {e:#}");
                self.pool.release(slot);
                self.set_state(au::ENCODER_WEDGED);
                // A lazy backend re-runs its whole bring-up per submit; stop retrying at the
                // compose rate and leave the session threadless for the host's reset rung.
                self.submit_failures += 1;
                if self.submit_failures >= MAX_SUBMIT_FAILURES {
                    dbglog!(
                        "[pf-vd] encode: {MAX_SUBMIT_FAILURES} submits failed in a row — leaving"
                    );
                    break;
                }
                continue;
            }
            self.submit_failures = 0;
            self.wire_seq = self.wire_seq.wrapping_add(1);
            self.inflight.push_back((slot, qpc, seq, index));
            self.collect(MAX_INFLIGHT);
        }
        // No flush on the way out: a stopped session has nowhere to send the last AUs. A
        // detached thread owns nothing in the pool any more — its successor reclaimed it.
        if self.live.load(Ordering::Acquire) {
            for (slot, ..) in self.inflight.drain(..) {
                self.pool.release(slot);
            }
            self.pool.set_live(false);
        }
    }

    /// The rate the backend took, into the header word the host's `reconfigure_bitrate` waits
    /// on; `0` = declined, which sends the host to its rebuild path. The sequence only rises,
    /// so a `RESET`'s fresh thread cannot hand back a number the host already saw.
    fn answer_bitrate(&self, kbps: u32) {
        let section = &self.session.section;
        let at = offset_of!(AuHeader, applied_bitrate);
        let seq = au::applied_bitrate_seq(section.load_u64(at));
        section.store_u64(at, au::applied_bitrate(seq.wrapping_add(1), kbps));
    }

    /// The `ENCODE_CTL` ops queued since the last frame, in order. An RFI the backend cannot
    /// honour becomes a keyframe request, the host's own fallback.
    fn drain_ctl(&mut self) {
        for op in self.session.take_ctl() {
            match op {
                Ctl::RequestKeyframe => {
                    self.enc.request_keyframe();
                    self.want_republish = true;
                }
                Ctl::InvalidateRefFrames(first, last) => {
                    if !self
                        .enc
                        .invalidate_ref_frames(i64::from(first), i64::from(last))
                    {
                        self.enc.request_keyframe();
                        self.want_republish = true;
                    }
                }
                Ctl::DistrustReferences => self.enc.distrust_references(),
                Ctl::ReconfigureBitrate(kbps) => {
                    let applied = if self.enc.reconfigure_bitrate(u64::from(kbps) * 1000) {
                        self.enc
                            .applied_bitrate_bps()
                            .map_or(kbps, |bps| (bps / 1000) as u32)
                    } else {
                        dbglog!("[pf-vd] encode: backend declined bitrate {kbps} kbps in place");
                        0
                    };
                    self.answer_bitrate(applied);
                }
                Ctl::SetHdrMeta(bytes) => self.enc.set_hdr_meta(Some(hdr_meta(&bytes))),
                Ctl::Flush => {
                    if let Err(e) = self.enc.flush() {
                        dbglog!("[pf-vd] encode: flush failed: {e:#}");
                    }
                    self.collect(1);
                }
            }
        }
    }

    /// One bounded wait on `{stop, pool event}`; a signal raised while nobody waited latches.
    /// A cursor move held off by the refresh cap gets a short bound, so the pointer's resting
    /// spot lands within one frame period instead of after the idle 1 s.
    fn wait(&self) {
        let ms = if self.pool.cursor_pending() {
            let due = self
                .last_cursor
                .map(|t| self.frame_interval.saturating_sub(t.elapsed()))
                .unwrap_or_default();
            (due.as_millis() as u32).clamp(1, 1000)
        } else {
            1000
        };
        let handles = [self.stop, self.pool.event()];
        // SAFETY: `stop` is the worker's stop event, alive until the worker joins or leaks; the
        // pool event lives as long as the pool, which the session's thread borrows.
        let _ = unsafe { WaitForMultipleObjects(&handles, false, ms) };
    }

    fn states(&self) -> [u32; au::AU_SLOTS as usize] {
        core::array::from_fn(|i| self.session.section.slot_state(i).load(Ordering::Acquire))
    }

    fn section_has_free(&self) -> bool {
        self.states().contains(&au::FREE)
    }

    fn count_drop(&self) {
        if !self.live.load(Ordering::Acquire) {
            return;
        }
        if let Offer::Dropped(n) = self.pool.drop_one() {
            self.session
                .section
                .store_u64(offset_of!(AuHeader, dropped_total), n);
        }
    }

    /// Poll until fewer than `keep` frames are in flight, publishing every chunk. A backend
    /// that owes an AU and produces none for [`WEDGE_AFTER`] is reported wedged; the host's
    /// reset is what ends that, not this loop.
    fn collect(&mut self, keep: usize) {
        let mut idle_since: Option<Instant> = None;
        while self.inflight.len() >= keep && !self.stopped() {
            let chunked = self.mid_au || self.enc.supports_chunked_poll();
            let next = if chunked {
                self.enc.poll_chunk()
            } else {
                self.enc.poll().map(|au| au.map(AuChunk::whole))
            };
            match next {
                Ok(Some(chunk)) => {
                    idle_since = None;
                    self.set_state(au::ENCODER_ENCODING);
                    self.on_chunk(chunk);
                }
                Ok(None) => {
                    let since = *idle_since.get_or_insert_with(Instant::now);
                    if since.elapsed() > WEDGE_AFTER {
                        self.set_state(au::ENCODER_WEDGED);
                    }
                    std::thread::sleep(Duration::from_micros(200));
                }
                Err(e) => {
                    dbglog!("[pf-vd] encode: poll failed: {e:#}");
                    self.set_state(au::ENCODER_WEDGED);
                    if let Some((slot, ..)) = self.inflight.pop_front() {
                        self.pool.release(slot);
                    }
                    self.mid_au = false;
                    return;
                }
            }
        }
    }

    /// One chunk of the oldest in-flight AU: published, or dropped with the rest of its AU. A
    /// detached thread returning from its wedge touches neither the pool nor the section.
    fn on_chunk(&mut self, chunk: AuChunk) {
        let Some(&(slot, qpc, seq, index)) = self.inflight.front() else {
            return;
        };
        if !self.live.load(Ordering::Acquire) {
            return;
        }
        self.mid_au = !chunk.last;
        if chunk.first {
            self.dropping_au = false;
            self.au_published = false;
        }
        if !self.dropping_au {
            let flags = [
                (chunk.first, au::AU_FIRST),
                (chunk.last, au::AU_LAST),
                (chunk.keyframe, au::AU_KEYFRAME),
                (chunk.recovery_anchor, au::AU_RECOVERY_ANCHOR),
                (chunk.chunk_aligned, au::AU_CHUNK_ALIGNED),
            ]
            .into_iter()
            .fold(0, |acc, (on, bit)| if on { acc | bit } else { acc });
            if self.publish(&chunk.data, flags, qpc, seq, index) {
                self.au_published = true;
            } else {
                self.dropping_au = true;
                self.count_drop();
                self.enc.request_keyframe();
            }
        }
        if chunk.last {
            if self.au_published {
                if self.dropping_au {
                    // The host opened this wire frame from the FIRST that did land. Without a
                    // LAST it waits out its chunk timeout and resets the encoder, undoing the
                    // keyframe already asked for above.
                    self.publish(&[], au::AU_LAST | au::AU_ABORTED, qpc, seq, index);
                }
                let n = self
                    .session
                    .section
                    .add_u64(offset_of!(AuHeader, published_total), 1);
                if n == 1 {
                    dbglog!(
                        "[pf-vd] encode: first AU published ({} B)",
                        chunk.data.len()
                    );
                }
            }
            self.inflight.pop_front();
            self.pool.release(slot);
            self.dropping_au = false;
        }
    }

    /// Heap bytes, slot record, `latest`, event — in that order, under `index`: the wire index
    /// the encoder was submitted with, so an RFI naming it names the picture it encoded.
    /// `false` when no placement came free within [`SLOT_WAIT`].
    fn publish(&mut self, data: &[u8], flags: u32, qpc: u64, seq: u64, index: u32) -> bool {
        let section = &self.session.section;
        let len = data.len() as u32;
        let deadline = Instant::now() + SLOT_WAIT;
        let (slot, offset) = loop {
            let states = self.states();
            if let Some(placed) = self.ring.take(len, &states) {
                break placed;
            }
            if Instant::now() > deadline || self.stopped() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        if !self.live.load(Ordering::Acquire) || !section.write_heap(offset, data) {
            return false;
        }
        section.publish_slot(
            slot,
            &AuSlot {
                offset,
                len,
                wire_seq: index,
                source_seq: seq as u32,
                qpc_pts: qpc,
                flags,
                state: au::PUBLISHED,
            },
        );
        self.publish_seq = self.publish_seq.wrapping_add(1);
        section.store_u64(offset_of!(AuHeader, last_au_qpc), qpc_now());
        section.publish_latest(FrameToken {
            generation: self.session.generation,
            seq: self.publish_seq,
            slot: slot as u8,
        });
        true
    }
}
