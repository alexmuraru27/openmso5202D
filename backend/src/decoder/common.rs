//! Shared front-end for the protocol decoders.
//!
//! Everything the per-protocol decoders have in common lives here so each of those holds
//! only its protocol logic:
//!
//! - analog → logic thresholding against the signal's **local envelope**,
//! - edge extraction and bit-period estimation,
//! - a **bit grid**: fit a drift-free sample grid to the edges by least squares, then
//!   sample each bit by majority vote over its middle,
//! - [`both_ways`]: run a decoder forwards and backwards and keep the better result.
//!
//! Pure logic — no hardware.

use std::collections::VecDeque;

/// Fraction of adjacent byte pairs that step by +1 (mod 256).
///
/// The test generator emits a 0x00..0xFF ramp, so this measures how much of a decode is
/// actually correct — far more informative than a byte count, which a desynced decoder can
/// inflate with garbage.
pub fn ramp_ratio(values: &[u8]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let good = values
        .windows(2)
        .filter(|w| w[1] == w[0].wrapping_add(1))
        .count();
    good as f64 / (values.len() - 1) as f64
}

/// Round half to **even**, matching NumPy and Python 3.
///
/// Rust's `f64::round` rounds halves away from zero, which puts an exact `.5` on the other
/// side and shifts a grid index by one. That only bites when a value lands precisely on a
/// half — but bit-grid maths produces exact halves readily (an integer period divided by
/// two), so the reference's tie-breaking has to be reproduced.
pub fn round_half_even(x: f64) -> f64 {
    let floor = x.floor();
    let fraction = x - floor;
    // On an exact tie, go to whichever neighbour is even; otherwise round normally.
    let round_up = if fraction == 0.5 {
        (floor as i64) % 2 != 0
    } else {
        fraction > 0.5
    };
    if round_up {
        floor + 1.0
    } else {
        floor
    }
}

/// Linear-interpolated percentile of `sig`, matching NumPy's default.
pub fn percentile(sig: &[f64], q: f64) -> f64 {
    if sig.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = sig.to_vec();
    sorted.sort_by(f64::total_cmp);
    let position = (q / 100.0) * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        return sorted[lower];
    }
    let frac = position - lower as f64;
    sorted[lower] + frac * (sorted[upper] - sorted[lower])
}

/// Schmitt trigger with per-sample thresholds.
///
/// A sample above `hi` forces the state high, below `lo` forces it low, and in between the
/// state holds — hysteresis, which is what stops noise around the trigger point from
/// producing a burst of false edges. Samples before the first forcing one take the level
/// implied by the first sample.
fn schmitt(sig: &[f64], lo: &dyn Fn(usize) -> f64, hi: &dyn Fn(usize) -> f64) -> Vec<bool> {
    if sig.is_empty() {
        return Vec::new();
    }
    // Until something forces the state, fall back to which side of the band we start on.
    let mut state = sig[0] > (lo(0) + hi(0)) / 2.0;
    let mut out = Vec::with_capacity(sig.len());
    for (i, &value) in sig.iter().enumerate() {
        if value > hi(i) {
            state = true;
        } else if value < lo(i) {
            state = false;
        }
        out.push(state);
    }
    out
}

/// Global Schmitt trigger: one fixed threshold for the whole record.
///
/// Rails are the 0.1/99.9 percentiles rather than min/max so a single glitch cannot set the
/// scale. Correct for clean signals, and the fallback for a line with too few transitions
/// to gauge a period.
pub fn threshold_global(sig: &[f64]) -> Vec<bool> {
    if sig.is_empty() {
        return Vec::new();
    }
    let (lo, hi) = (percentile(sig, 0.1), percentile(sig, 99.9));
    let span = hi - lo;
    if span < 1e-12 {
        return vec![false; sig.len()];
    }
    let mid = lo + span * 0.5;
    let band = span * 0.3 / 2.0;
    schmitt(sig, &|_| mid - band, &|_| mid + band)
}

/// Digitize against the signal's **local** envelope rather than one global level.
///
/// A sliding max/min over roughly 1.5 bit periods tracks the signal's own high and low as
/// they drift, triggering at the local midpoint. This is what recovers a fast line whose
/// low droops during active bursts (AC coupling, limited bandwidth): a single global
/// threshold sits above the drooped low and silently drops edges, while a local midpoint
/// follows the droop and keeps them.
///
/// The hysteresis band scales with the local swing but is floored against the global span,
/// so idle noise — where the local envelope collapses — cannot chatter.
pub fn threshold_local(sig: &[f64]) -> Vec<bool> {
    const HYSTERESIS_FRACTION: f64 = 0.2;
    const FLOOR_FRACTION: f64 = 0.12;

    if sig.is_empty() {
        return Vec::new();
    }
    let (lo, hi) = (percentile(sig, 0.1), percentile(sig, 99.9));
    let span = hi - lo;
    if span < 1e-12 {
        return vec![false; sig.len()];
    }

    // A coarse global pass first, purely to estimate the bit period the envelope window
    // needs to span.
    let coarse = schmitt(sig, &|_| lo + span * 0.35, &|_| lo + span * 0.65);
    let transitions = edges(&coarse);
    if transitions.len() < 4 {
        return threshold_global(sig);
    }
    let mut gaps: Vec<f64> = transitions
        .windows(2)
        .map(|w| (w[1] - w[0]) as f64)
        .collect();
    gaps.sort_by(f64::total_cmp);
    let period = gaps[gaps.len() / 2] * 2.0;
    let window = (period * 1.5).round().max(3.0) as usize;

    let local_hi = sliding_extreme(sig, window, true);
    let local_lo = sliding_extreme(sig, window, false);
    let mid: Vec<f64> = local_hi
        .iter()
        .zip(&local_lo)
        .map(|(h, l)| (h + l) * 0.5)
        .collect();
    let band: Vec<f64> = local_hi
        .iter()
        .zip(&local_lo)
        .map(|(h, l)| ((h - l) * HYSTERESIS_FRACTION).max(span * FLOOR_FRACTION))
        .collect();

    schmitt(sig, &|i| mid[i] - band[i], &|i| mid[i] + band[i])
}

/// Threshold an already-in-volts analog trace into a logic trace.
pub fn threshold_volts(volts: &[f64]) -> Vec<bool> {
    threshold_local(volts)
}

/// Sliding window maximum (or minimum) with edge clamping, in linear time.
///
/// Uses a monotonic deque so a wide window over a deep record stays O(n) — a naive scan
/// would be O(n·window), which on a 400 000-sample record with a 300-sample window is
/// slow enough to matter.
fn sliding_extreme(sig: &[f64], window: usize, want_max: bool) -> Vec<f64> {
    let n = sig.len();
    if n == 0 {
        return Vec::new();
    }
    let half_left = window.saturating_sub(1) / 2;
    let half_right = window / 2;

    let mut out = vec![0.0; n];
    let mut deque: VecDeque<usize> = VecDeque::new();
    let mut next = 0usize;

    for (i, slot) in out.iter_mut().enumerate() {
        let right = (i + half_right).min(n - 1);
        while next <= right {
            while let Some(&back) = deque.back() {
                let dominated = if want_max {
                    sig[back] <= sig[next]
                } else {
                    sig[back] >= sig[next]
                };
                if dominated {
                    deque.pop_back();
                } else {
                    break;
                }
            }
            deque.push_back(next);
            next += 1;
        }
        let left = i.saturating_sub(half_left);
        while let Some(&front) = deque.front() {
            if front < left {
                deque.pop_front();
            } else {
                break;
            }
        }
        *slot = sig[*deque.front().expect("window is never empty")];
    }
    out
}

/// Indices where the level changes; an edge lands on the sample at the new level.
pub fn edges(trace: &[bool]) -> Vec<usize> {
    (1..trace.len()).filter(|&i| trace[i] != trace[i - 1]).collect()
}

/// Shortest constant run, in samples — a rough estimate of one bit period.
pub fn min_pulse(trace: &[bool]) -> usize {
    let e = edges(trace);
    if e.len() < 2 {
        return 0;
    }
    e.windows(2).map(|w| w[1] - w[0]).min().unwrap_or(0)
}

/// The level a line rests at, taken as the level held during its longest constant run.
///
/// An idle stretch outlasts any run within a frame, which makes this a reliable way to
/// tell an idle-high line from an inverted one.
pub fn idle_level(trace: &[bool]) -> bool {
    if trace.len() < 2 {
        return true;
    }
    let mut bounds = vec![0usize];
    bounds.extend(edges(trace));
    bounds.push(trace.len());

    let mut best = (0usize, 0usize); // (length, start)
    for w in bounds.windows(2) {
        let length = w[1] - w[0];
        if length > best.0 {
            best = (length, w[0]);
        }
    }
    trace[best.1]
}

/// Refine samples-per-bit and phase by least-squares fitting the edges to an integer grid.
///
/// Every edge sits on a bit boundary — an integer number of bit periods from any other —
/// so snapping each to its nearest grid index and regressing recovers the true period from
/// the data. That removes the slow drift a nominal (baud-derived) period accumulates across
/// a long record, which is what otherwise walks a decode out of alignment near the end.
///
/// Falls back to the input estimate if the fit is degenerate.
pub fn refine_period(edge_indices: &[usize], initial_spb: f64) -> (f64, f64) {
    let fallback = (
        initial_spb,
        edge_indices.first().map(|&e| e as f64).unwrap_or(0.0),
    );
    if edge_indices.len() < 3 || initial_spb < 1.0 {
        return fallback;
    }
    let first = edge_indices[0] as f64;
    let grid: Vec<f64> = edge_indices
        .iter()
        .map(|&e| round_half_even((e as f64 - first) / initial_spb))
        .collect();
    if grid[grid.len() - 1] == grid[0] {
        return fallback;
    }

    // Least squares for e = phase + k·spb.
    let n = grid.len() as f64;
    let sum_k: f64 = grid.iter().sum();
    let sum_e: f64 = edge_indices.iter().map(|&e| e as f64).sum();
    let sum_kk: f64 = grid.iter().map(|k| k * k).sum();
    let sum_ke: f64 = grid
        .iter()
        .zip(edge_indices)
        .map(|(k, &e)| k * e as f64)
        .sum();

    let denominator = n * sum_kk - sum_k * sum_k;
    if denominator.abs() < 1e-12 {
        return fallback;
    }
    let spb = (n * sum_ke - sum_k * sum_e) / denominator;
    if spb <= 1.0 || !spb.is_finite() {
        return fallback;
    }
    let phase = (sum_e - spb * sum_k) / n;
    (spb, phase)
}

/// Sample every bit cell on the grid `phase + k·spb`.
///
/// Returns one bit per cell plus the centre sample index of each. With `vote`, a cell is
/// the majority over its middle half, which absorbs edge jitter that a single centre
/// sample would be at the mercy of.
pub fn sample_grid(trace: &[bool], spb: f64, phase: f64, vote: bool) -> (Vec<bool>, Vec<usize>) {
    let n = trace.len();
    if n == 0 || spb < 1.0 {
        return (Vec::new(), Vec::new());
    }
    // Centre the phase in (-spb/2, spb/2]. Taking it modulo spb directly can yield ≈spb
    // instead of 0 for a near-exact multiple, which would start the grid one cell late and
    // shift every bit by one.
    let phase = (phase + 0.5 * spb).rem_euclid(spb) - 0.5 * spb;

    let k0 = ((0.0 - phase) / spb).floor() as i64;
    let k1 = ((n - 1) as f64 - phase) / spb;
    let k1 = k1 as i64; // truncates toward zero, as the reference does
    if k1 <= k0 {
        return (Vec::new(), Vec::new());
    }

    let mut bits = Vec::with_capacity((k1 - k0) as usize);
    let mut centres = Vec::with_capacity((k1 - k0) as usize);
    let half = ((spb * 0.25) as usize).max(1);

    for k in k0..k1 {
        let centre = phase + (k as f64 + 0.5) * spb;
        let index = (round_half_even(centre) as i64).clamp(0, n as i64 - 1) as usize;
        centres.push(index);

        if !vote || spb < 4.0 {
            bits.push(trace[index]);
            continue;
        }
        let mut high = 0usize;
        let mut total = 0usize;
        for offset in -(half as i64)..=(half as i64) {
            let i = (index as i64 + offset).clamp(0, n as i64 - 1) as usize;
            high += trace[i] as usize;
            total += 1;
        }
        bits.push(high * 2 >= total);
    }
    (bits, centres)
}

/// Run a decoder forwards and on the time-reversed trace, keeping the better result.
///
/// A capture whose start is corrupt — triggered mid-byte — but whose end is clean still
/// yields its tail this way, exactly where a forward-only pass desyncs and loses it.
pub fn both_ways<T>(
    decode: impl Fn(bool) -> Vec<T>,
    score: impl Fn(&[T]) -> usize,
) -> Vec<T> {
    let forward = decode(false);
    let reverse = decode(true);
    if score(&reverse) > score(&forward) {
        reverse
    } else {
        forward
    }
}
