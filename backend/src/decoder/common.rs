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
/// producing a burst of false edges. Samples before the first forcing one take `initial`.
///
/// `initial` must be judged against the record's *global* rails, never the band in force at
/// sample 0. With a local band those thresholds collapse onto the signal during a long idle
/// — the envelope over a flat stretch is flat — so nothing forces the state and the seed
/// decides the level for as long as the line stays quiet. Comparing against a collapsed
/// band makes that seed a coin toss settled by one LSB of noise, and the wrong call held an
/// idle-low line high right up to the first transfer.
fn schmitt(
    sig: &[f64],
    lo: &dyn Fn(usize) -> f64,
    hi: &dyn Fn(usize) -> f64,
    initial: bool,
) -> Vec<bool> {
    if sig.is_empty() {
        return Vec::new();
    }
    let mut state = initial;
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
    schmitt(sig, &|_| mid - band, &|_| mid + band, sig[0] > mid)
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
    //
    // Its hysteresis is deliberately *narrow*. A wide band is the right choice for the
    // digitisation itself, but here it is actively harmful: a bandwidth-limited fast line
    // no longer reaches its rails — a 20 MHz clock through a 1× probe swings only over the
    // middle third of the record's span — so a ±0.15·span band never sees a crossing, the
    // period comes out as the whole record, and the envelope window grows so wide that the
    // local pass degenerates into the global one it exists to replace. Since this pass only
    // has to yield a *timescale*, sensitivity beats noise immunity: the band still clears
    // the digitiser's LSB by an order of magnitude, and the median of the gaps shrugs off
    // the odd spurious edge.
    const COARSE_BAND: f64 = 0.05;
    let coarse = schmitt(
        sig,
        &|_| lo + span * (0.5 - COARSE_BAND),
        &|_| lo + span * (0.5 + COARSE_BAND),
        sig[0] > lo + span * 0.5,
    );
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

    schmitt(sig, &|i| mid[i] - band[i], &|i| mid[i] + band[i], sig[0] > lo + span * 0.5)
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

/// Refine samples-per-bit and phase so the edges land on a common grid.
///
/// Every edge sits on a bit boundary — an integer number of bit periods from any other — so
/// the true period is the one that puts all of them on grid lines at once. Recovering it
/// from the record matters because the nominal period usually is not the real one: a
/// transmitter's clock divider rarely hits the requested rate exactly, and an error of a
/// tenth of a percent, harmless over a few bytes, walks a decode a whole bit out of step
/// across a deep record.
///
/// The search is what makes this reliable. Snapping edges to a grid and regressing is the
/// obvious method and it is treacherous: the snapping is only valid once the period is
/// already close, so on a jittery capture it settles into a false basin and returns a
/// period *worse* than the one it started from. Measured on a 350-byte capture, that basin
/// sat 0.4 % away from the truth and cost two thirds of the bytes. So the period is found
/// first by direct search — maximising how tightly the edges cluster around grid lines,
/// which is well defined however far off the starting guess is — and the regression is used
/// only to polish the winner, and only if it stays in the basin the search picked.
///
/// The search is hierarchical: a short prefix cannot resolve the period finely but brackets
/// it cheaply, and each doubling of the span both sharpens the resolution and narrows the
/// bracket to a few steps around the previous answer.
///
/// Falls back to the input estimate if the fit is degenerate.
pub fn refine_period(edge_indices: &[usize], initial_spb: f64) -> (f64, f64) {
    /// Edges in the first search stage.
    const FIRST_SPAN: usize = 128;
    /// How far the true period may sit from the nominal one, as a fraction.
    const REACH: f64 = 0.02;
    /// Step size as a fraction of a bit period of accumulated drift across the span — fine
    /// enough that the true period cannot fall between two steps.
    const DRIFT_PER_STEP: f64 = 0.25;

    let fallback = (
        initial_spb,
        edge_indices.first().map(|&e| e as f64).unwrap_or(0.0),
    );
    if edge_indices.len() < 3 || initial_spb < 1.0 {
        return fallback;
    }

    let mut spb = initial_spb;
    let (mut low, mut high) = (-REACH, REACH);
    let mut span = FIRST_SPAN;
    loop {
        let take = span.min(edge_indices.len());
        let window = &edge_indices[..take];
        let cells = (window[take - 1] - window[0]) as f64 / spb;
        let step = (DRIFT_PER_STEP / cells.max(1.0)).min(REACH / 4.0);

        let mut best = (f64::NEG_INFINITY, spb);
        let mut offset = low;
        while offset <= high {
            let candidate = spb * (1.0 + offset);
            let score = grid_concentration(window, candidate);
            if score > best.0 {
                best = (score, candidate);
            }
            offset += step;
        }
        spb = best.1;

        if take == edge_indices.len() {
            break;
        }
        // The next span resolves finer; only a couple of this stage's steps of doubt remain.
        low = -2.0 * step;
        high = 2.0 * step;
        span *= 2;
    }

    // Polish by regression, which is now snapping against a period already in the right
    // basin — but discard it if it wanders out of one search step, which means it is not.
    let tolerance = spb * DRIFT_PER_STEP
        / ((edge_indices[edge_indices.len() - 1] - edge_indices[0]) as f64 / spb).max(1.0);
    match fit_grid(edge_indices, spb) {
        Some((refined, phase)) if (refined - spb).abs() <= tolerance => (refined, phase),
        _ => (spb, edge_indices[0] as f64),
    }
}

/// How tightly the edges cluster on a grid of period `spb`, in [0, 1].
///
/// This is the magnitude of the edge train's Fourier component at that period: every edge
/// contributes a unit vector at its phase within the cell, so they add up when the period is
/// right and cancel when it is not. Phase is measured circularly, which is what makes the
/// score meaningful at any distance from the truth — an edge that has drifted past a cell
/// boundary counts as slightly early rather than as a whole period of error.
fn grid_concentration(edge_indices: &[usize], spb: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for &e in edge_indices {
        let angle = std::f64::consts::TAU * (e as f64 / spb);
        re += angle.cos();
        im += angle.sin();
    }
    (re * re + im * im).sqrt() / edge_indices.len() as f64
}

/// One least-squares pass: snap each edge to a grid index using `spb`, then fit `e = phase + k·spb`.
fn fit_grid(edge_indices: &[usize], spb: f64) -> Option<(f64, f64)> {
    if edge_indices.len() < 3 {
        return None;
    }
    let first = edge_indices[0] as f64;
    let grid: Vec<f64> = edge_indices
        .iter()
        .map(|&e| round_half_even((e as f64 - first) / spb))
        .collect();
    if grid[grid.len() - 1] == grid[0] {
        return None;
    }

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
        return None;
    }
    let refined = (n * sum_ke - sum_k * sum_e) / denominator;
    if refined <= 1.0 || !refined.is_finite() {
        return None;
    }
    Some((refined, (sum_e - refined * sum_k) / n))
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

    for k in k0..k1 {
        let (bit, index) = sample_cell(trace, phase + (k as f64 + 0.5) * spb, spb, vote);
        bits.push(bit);
        centres.push(index);
    }
    (bits, centres)
}

/// Sample one bit cell centred at `centre` samples, returning its level and centre index.
///
/// With `vote`, the level is the majority over the cell's middle half rather than the one
/// sample at the centre, which absorbs edge jitter. Below four samples per bit there is no
/// middle to speak of, so the centre sample is taken as-is.
pub fn sample_cell(trace: &[bool], centre: f64, spb: f64, vote: bool) -> (bool, usize) {
    let n = trace.len();
    let index = (round_half_even(centre) as i64).clamp(0, n as i64 - 1) as usize;
    if !vote || spb < 4.0 {
        return (trace[index], index);
    }
    let half = ((spb * 0.25) as usize).max(1) as i64;
    let mut high = 0usize;
    let mut total = 0usize;
    for offset in -half..=half {
        let i = (index as i64 + offset).clamp(0, n as i64 - 1) as usize;
        high += trace[i] as usize;
        total += 1;
    }
    (high * 2 >= total, index)
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
