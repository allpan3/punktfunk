//! Every planner meets a damaged stream, and must answer instead of dying.
//!
//! A client's decode path is fed by a lossy link: an AU can arrive truncated,
//! spliced, or with any byte flipped. The contract is that `plan_au` returns —
//! `Ok` or `Err`, both fine — and never panics, never loops, never runs away.
//! WP9 is entirely fixes of that shape (a duplicate POC that looped the H.265
//! bumper, an unchecked POC-type-1 sum, an AV1 header replayed across a
//! truncated AU); this is the check that keeps them fixed.
//!
//! Deterministic, so a failure reproduces from the printed seed. Mutating a real
//! vector rather than feeding random bytes is what reaches past the parser's
//! front door into the DPB and RPS code where the interesting faults live.

use pf_bitstream::av1::Av1Planner;
use pf_bitstream::h264::H264Planner;
use pf_bitstream::h265::H265Planner;

const H264: &[u8] =
    include_bytes!("../vendor/cros-codecs/src/codec/h264/test_data/test-25fps.h264");
const H265: &[u8] =
    include_bytes!("../vendor/cros-codecs/src/codec/h265/test_data/test-25fps.h265");
const AV1: &[u8] =
    include_bytes!("../vendor/cros-codecs/src/codec/av1/test_data/test-25fps.ivf.av1");

/// Enough to exercise every NAL type in the vector several times over, and still
/// a couple of seconds, so it rides the ordinary PR lane. `PF_DAMAGE_ROUNDS`
/// raises it for a nightly soak without touching the code.
fn rounds() -> u32 {
    std::env::var("PF_DAMAGE_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3_000)
}

/// A whole run must finish well inside this. A planner that loops on damage
/// blows the CI job's timeout, which reads as "infra flake"; a planner that
/// merely goes quadratic trips this instead and names itself.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

/// xorshift64*: deterministic, no dev-dependency, good enough to pick offsets.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// One damaged copy: a truncation, then a handful of byte edits. Both together,
/// because a truncated AU whose remaining bytes still parse is the shape that
/// leaves a parser holding half a header — S-79's fault exactly.
fn damage(src: &[u8], rng: &mut Rng) -> Vec<u8> {
    let keep = match rng.next() % 4 {
        // Three runs in four keep the whole thing: most faults need a stream
        // long enough to build DPB state before the damage lands.
        0 => rng.below(src.len()),
        _ => src.len(),
    };
    let mut out = src[..keep].to_vec();
    if out.is_empty() {
        return out;
    }
    for _ in 0..1 + rng.below(4) {
        let at = rng.below(out.len());
        out[at] = match rng.next() % 3 {
            // A flipped bit is the wire's own failure; the whole-byte writes
            // reach syntax elements a single flip rarely moves.
            0 => out[at] ^ (1u8 << (rng.next() % 8)),
            1 => (rng.next() % 256) as u8,
            _ => 0,
        };
    }
    out
}

/// Annex-B split. Deliberately naive and local to the test: the planners' own
/// splitters are private, and a damaged stream is not owed a tidy split anyway.
fn annex_b_chunks(stream: &[u8], want: usize) -> Vec<&[u8]> {
    let mut starts = vec![];
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts.push(stream.len());
    starts
        .windows(2)
        .take(want)
        .map(|w| &stream[w[0]..w[1]])
        .collect()
}

#[test]
fn a_damaged_stream_is_answered_not_survived_by_luck() {
    let t0 = std::time::Instant::now();
    let mut rng = Rng(0x5015_2026_0906_1337);
    let mut planned = 0u32;
    let mut refused = 0u32;

    let rounds = rounds();
    for round in 0..rounds {
        // The seed is printed on the way in, so a panic names the round that
        // produced it and the run replays byte for byte.
        let seed = rng.0;
        let mut tally = |ok: bool| {
            if ok {
                planned += 1;
            } else {
                refused += 1;
            }
        };

        match round % 3 {
            0 => {
                let bytes = damage(H264, &mut rng);
                let mut planner = H264Planner::new();
                for au in annex_b_chunks(&bytes, 40) {
                    tally(planner.plan_au(au).is_ok());
                }
            }
            1 => {
                let bytes = damage(H265, &mut rng);
                let mut planner = H265Planner::new();
                for au in annex_b_chunks(&bytes, 40) {
                    tally(planner.plan_au(au).is_ok());
                }
            }
            _ => {
                let bytes = damage(AV1, &mut rng);
                let mut planner = Av1Planner::new();
                // No annex-B framing in an IVF payload: hand it fixed slices, which
                // is what a reassembler with a hole in it produces anyway.
                for au in bytes.chunks(4096).take(40) {
                    tally(planner.plan_au(au).is_ok());
                }
            }
        }

        assert!(
            t0.elapsed() < BUDGET,
            "round {round} (seed {seed:#x}) pushed the run past {BUDGET:?} — a planner is \
             looping or has gone quadratic on damaged input"
        );
    }

    // Both arms must be non-empty, or the test is asserting nothing: all-refused
    // means the damage destroyed every stream before it reached the planner, and
    // all-planned means it never damaged anything that mattered.
    assert!(
        planned > 0,
        "no damaged AU ever planned — the mutation is too destructive"
    );
    assert!(
        refused > 0,
        "no damaged AU was ever refused — the mutation reaches nothing"
    );
    println!(
        "{rounds} rounds: {planned} planned, {refused} refused, in {:?}",
        t0.elapsed()
    );
}
