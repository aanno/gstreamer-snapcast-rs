//! Keeps `snapsrc`'s output on the Snapcast timeline while the audio sink's clock
//! (the sound card) runs at its own rate.
//!
//! The pipeline clock is the sink's (see "Clock" in `imp.rs`). Each frame's ideal PTS
//! (its play-out time, mapped onto that clock) therefore drifts against the sample
//! count: a card that is 300 ppm slow needs 300 ppm fewer samples per second. Audio
//! sinks ignore small timestamp jumps ("alignment") and would play at the card's rate,
//! so instead the output stays **contiguous** and the remaining error is corrected
//! in the samples. Like the C++ snapclient, we insert or drop single frames, evenly
//! spread, never more than `MAX_CORRECTION` of a buffer.
//!
//! - The error (ideal PTS − contiguous PTS) is low-pass filtered, because clock
//!   readings jitter by a fraction of a millisecond.
//! - Within `DEADBAND` nothing is corrected (no needless chatter).
//! - Beyond `RESYNC` (a jump: start, latency change, lost frames), the output jumps
//!   to the ideal PTS once, and the caller marks the buffer DISCONT.

/// Errors beyond this resync instead of being corrected gradually.
const RESYNC_NS: i64 = 20_000_000;
/// Errors within this are left alone.
const DEADBAND_NS: f64 = 250_000.0;
/// At most this share of a buffer's frames is inserted or dropped (0.1 % = 1000 ppm,
/// well above real crystal drift of ~±100 ppm).
const MAX_CORRECTION: f64 = 0.001;
/// Low-pass filter weight of a new error measurement.
const ALPHA: f64 = 0.1;

/// Where the next buffer goes, and how many frames to add (> 0) or remove (< 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// PTS in ns of running time.
    pub pts_ns: i64,
    /// Frames to insert (positive) or drop (negative), see [`stretch`].
    pub correction: i64,
    /// The output jumped: mark the buffer DISCONT.
    pub resync: bool,
}

#[derive(Debug, Default)]
pub struct Aligner {
    /// Running time (ns) of the last resync, and frames output since then: the next
    /// contiguous PTS is `origin_ns + frames * 1e9 / rate`, without rounding drift.
    origin_ns: i64,
    frames: i64,
    rate: u32,
    /// Filtered error in ns: ideal minus contiguous PTS. Positive: play later (insert).
    error_ns: f64,
    synced: bool,
    /// Frames inserted (+) / dropped (−) since the last resync, for diagnostics.
    corrected: i64,
}

impl Aligner {
    /// Forgets the position: the next buffer resyncs.
    pub fn reset(&mut self) {
        self.synced = false;
    }

    /// The filtered error in ns (ideal minus output PTS) and the frames corrected since
    /// the last resync, for diagnostics.
    pub fn stats(&self) -> (f64, i64) {
        (self.error_ns, self.corrected)
    }

    fn next_pts_ns(&self) -> i64 {
        self.origin_ns + (i128::from(self.frames) * 1_000_000_000 / i128::from(self.rate)) as i64
    }

    /// Places a buffer of `n_frames` whose first frame ideally plays at `ideal_ns`.
    pub fn place(&mut self, ideal_ns: i64, n_frames: usize, rate: u32) -> Placement {
        let n = n_frames as i64;
        if !self.synced || rate != self.rate || (ideal_ns - self.next_pts_ns()).abs() > RESYNC_NS {
            *self = Self {
                origin_ns: ideal_ns,
                frames: n,
                rate,
                error_ns: 0.0,
                synced: true,
                corrected: 0,
            };
            return Placement {
                pts_ns: ideal_ns,
                correction: 0,
                resync: true,
            };
        }

        let pts_ns = self.next_pts_ns();
        self.error_ns += ALPHA * ((ideal_ns - pts_ns) as f64 - self.error_ns);
        let mut correction = 0;
        if self.error_ns.abs() > DEADBAND_NS {
            let max = ((n as f64 * MAX_CORRECTION).ceil() as i64).max(1);
            let wanted = (self.error_ns * f64::from(rate) / 1e9).round() as i64;
            correction = wanted.clamp(-max, max);
            // The correction moves the following buffers; account for it right away
            // instead of waiting for the filter to notice.
            self.error_ns -= correction as f64 * 1e9 / f64::from(rate);
        }
        // Never drop a whole buffer.
        correction = correction.max(1 - n);
        self.frames += n + correction;
        self.corrected += correction;
        Placement {
            pts_ns,
            correction,
            resync: false,
        }
    }
}

/// Inserts (`correction > 0`, by repeating frames) or drops (`< 0`) single frames of
/// interleaved `samples`, evenly spread over the buffer.
pub fn stretch(samples: &[f32], channels: usize, correction: i64) -> Vec<f32> {
    let channels = channels.max(1);
    let n = samples.len() / channels;
    let k = correction.unsigned_abs() as usize;
    if k == 0 || n == 0 {
        return samples.to_vec();
    }
    // The centers of k equal segments: frame indices to repeat or drop.
    let mut points = (0..k).map(|i| (2 * i + 1) * n / (2 * k)).peekable();
    let mut out = Vec::with_capacity((n as i64 + correction).max(0) as usize * channels);
    for (f, frame) in samples.chunks_exact(channels).enumerate() {
        let mut hits = 0;
        while points.next_if_eq(&f).is_some() {
            hits += 1;
        }
        let copies = if correction > 0 {
            1 + hits
        } else {
            usize::from(hits == 0)
        };
        for _ in 0..copies {
            out.extend_from_slice(frame);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const N: usize = 1024;

    fn buffer_ns(frames: i64) -> f64 {
        frames as f64 * 1e9 / f64::from(RATE)
    }

    #[test]
    fn first_buffer_resyncs_and_the_next_one_is_contiguous() {
        let mut a = Aligner::default();
        let p = a.place(1_000_000_000, N, RATE);
        assert_eq!((p.pts_ns, p.correction, p.resync), (1_000_000_000, 0, true));
        let ideal = 1_000_000_000 + buffer_ns(N as i64) as i64 + 50_000; // 50 µs jitter
        let p = a.place(ideal, N, RATE);
        assert!(!p.resync);
        assert_eq!(p.pts_ns, 1_000_000_000 + buffer_ns(N as i64) as i64);
        assert_eq!(
            p.correction, 0,
            "jitter within the deadband must not be corrected"
        );
    }

    #[test]
    fn a_jump_resyncs() {
        let mut a = Aligner::default();
        a.place(0, N, RATE);
        let p = a.place(buffer_ns(N as i64) as i64 - 65_000_000, N, RATE); // latency change
        assert!(p.resync);
    }

    /// A card that is `ppm` slow (or fast for negative values): the ideal PTS of each
    /// buffer advances by less (more) than the buffer's duration. The error must stay
    /// within the deadband plus about one correction, without resyncs.
    fn follows_drift(ppm: f64) {
        let mut a = Aligner::default();
        let mut ideal = 0.0;
        let mut worst: f64 = 0.0;
        let mut total = 0;
        for i in 0..20_000 {
            // 20 000 × 21.3 ms ≈ 7 minutes
            let p = a.place(ideal as i64 + (i % 7) * 20_000, N, RATE); // ±0.1 ms jitter
            assert!(!p.resync || i == 0, "resync at buffer {i}");
            if i > 500 {
                worst = worst.max((p.pts_ns as f64 - ideal).abs());
            }
            total += p.correction;
            ideal += buffer_ns(N as i64) * (1.0 + ppm * 1e-6);
        }
        assert!(
            worst < 1_000_000.0,
            "error up to {:.0} µs at {ppm} ppm",
            worst / 1000.0
        );
        let expected = 20_000.0 * N as f64 * ppm * 1e-6;
        assert!(
            (total as f64 - expected).abs() < expected.abs() * 0.05 + 50.0,
            "corrected {total} frames, expected about {expected:.0}"
        );
    }

    #[test]
    fn follows_a_slow_card() {
        follows_drift(300.0);
    }

    #[test]
    fn follows_a_fast_card() {
        follows_drift(-300.0);
    }

    #[test]
    fn follows_no_drift_without_corrections() {
        let mut a = Aligner::default();
        let mut total = 0;
        for i in 0..5_000 {
            let p = a.place(buffer_ns(i * N as i64) as i64, N, RATE);
            total += p.correction.abs();
        }
        assert_eq!(total, 0);
    }

    #[test]
    fn stretch_inserts_and_drops_whole_frames() {
        let samples: Vec<f32> = (0..20).map(|i| i as f32).collect(); // 10 stereo frames
        let longer = stretch(&samples, 2, 2);
        assert_eq!(longer.len(), 24);
        let shorter = stretch(&samples, 2, -3);
        assert_eq!(shorter.len(), 14);
        // Frames stay intact (L, R pairs of consecutive values) and in order.
        for out in [&longer, &shorter] {
            for f in out.as_chunks::<2>().0.iter() {
                assert_eq!(f[1], f[0] + 1.0);
            }
            assert!(out.as_chunks::<2>().0.iter().map(|f| f[0]).is_sorted());
        }
        assert_eq!(stretch(&samples, 2, 0), samples);
    }
}
