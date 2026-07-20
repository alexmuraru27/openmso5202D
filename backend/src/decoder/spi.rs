//! SPI decoder — clock plus one data line, with optional chip select.
//!
//! Data is sampled on the leading or trailing clock edge depending on mode. Byte framing
//! comes from a captured chip-select line if present, otherwise from idle-clock gaps, so a
//! bit-banged stream re-aligns even when the capture starts mid-byte.
//!
//! With only clock and data, and neither gaps nor chip select, byte boundaries are
//! genuinely ambiguous — nothing in the signal marks them.

use super::common::round_half_even;
use super::{Event, Kind};

/// Where a burst's byte boundary is anchored when there is no chip select.
///
/// A capture triggered mid-byte shifts every byte if grouped forward from the cut, so the
/// anchor decides which partial byte gets dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    /// Group forward from each burst's start, dropping a trailing partial.
    Start,
    /// Anchor the first burst to its end, dropping the leading partial. Correct when the
    /// transaction ended cleanly but the capture began mid-byte.
    End,
    /// Choose `End` when the clock stopped well before the record ended yet was already
    /// running at sample 0 — that combination means the capture was triggered mid-byte
    /// with a clean tail. Whole-byte bursts decode identically either way.
    #[default]
    Auto,
}

/// SPI decoding options.
#[derive(Debug, Clone, Copy)]
pub struct SpiOptions {
    /// Clock idle level.
    pub cpol: u8,
    /// Clock phase.
    pub cpha: u8,
    /// Bit order within a byte.
    pub msb_first: bool,
    /// Bits per word.
    pub bits: usize,
    /// An idle-clock gap longer than this many typical bit spacings splits bursts.
    pub word_gap: f64,
    /// A gap of 2 up to this many bit periods is treated as missed clock edges.
    pub max_missed: usize,
    /// Byte-boundary anchoring.
    pub anchor: Anchor,
    /// Ignore `cpol`/`cpha` and detect the sampling edge from the signal, so a device in
    /// any mode decodes without being told which.
    pub auto_mode: bool,
}

impl Default for SpiOptions {
    fn default() -> Self {
        Self {
            cpol: 0,
            cpha: 0,
            msb_first: true,
            bits: 8,
            word_gap: 10.0,
            max_missed: 8,
            anchor: Anchor::Auto,
            auto_mode: false,
        }
    }
}

/// Detect the sampling edge from the signals themselves, subsuming CPOL and CPHA.
///
/// Data is shifted on one clock edge and held stable across the other — the sampling edge
/// — so data transitions cluster near the shift edge and the sampling edge is the opposite
/// one. Falls back to rising (mode 0) when there is too little to tell.
pub fn detect_sample_rising(clock: &[bool], data: &[bool]) -> bool {
    let rising: Vec<usize> = (1..clock.len()).filter(|&i| clock[i] && !clock[i - 1]).collect();
    let falling: Vec<usize> = (1..clock.len()).filter(|&i| !clock[i] && clock[i - 1]).collect();
    let data_edges: Vec<usize> = (1..data.len()).filter(|&i| data[i] != data[i - 1]).collect();

    if data_edges.len() < 3 || rising.len() < 2 || falling.len() < 2 {
        return true;
    }
    let mut clock_edges: Vec<(usize, bool)> = rising
        .iter()
        .map(|&i| (i, true))
        .chain(falling.iter().map(|&i| (i, false)))
        .collect();
    clock_edges.sort_unstable();

    let mut near_rising = 0usize;
    for &d in &data_edges {
        // Nearest clock edge on either side.
        let j = clock_edges.partition_point(|&(i, _)| i < d).clamp(1, clock_edges.len() - 1);
        let (left, left_rising) = clock_edges[j - 1];
        let (right, right_rising) = clock_edges[j];
        let nearest_rising = if d - left <= right - d { left_rising } else { right_rising };
        near_rising += usize::from(nearest_rising);
    }
    let shift_on_rising = near_rising >= data_edges.len() - near_rising;
    !shift_on_rising
}

/// Decide whether the clock actually pulsed within a suspicious gap.
///
/// When two detected edges sit 2..N periods apart, the cause is either missed edges (a
/// bandwidth-limited pulse that never crossed the threshold) or a real inter-word idle.
/// Timing alone cannot separate them — but the raw analog clock can, which is the one
/// advantage a passive scope capture has over edge-triggered hardware. The middle of the
/// gap is inspected, away from the transition tails: a real idle stays near one level
/// while a missed pulse still swings.
fn gap_has_pulse(clock_analog: &[f64], a: usize, b: usize, median_period: f64) -> bool {
    let m = (round_half_even(median_period) as usize).max(1);
    let lo_index = (a + m / 2).min(b.saturating_sub(1));
    let hi_index = (b.saturating_sub(m / 2)).max(a + 2);
    let segment = clock_analog
        .get(lo_index..hi_index)
        .filter(|s| s.len() >= 3)
        .or_else(|| clock_analog.get(a + 1..b))
        .unwrap_or(&[]);
    if segment.len() < 2 {
        return true;
    }
    let lo = super::common::percentile(clock_analog, 1.0);
    let hi = super::common::percentile(clock_analog, 99.0);
    let swing = hi - lo;
    if swing < 1e-9 {
        return true;
    }
    let seg_min = segment.iter().copied().fold(f64::INFINITY, f64::min);
    let seg_max = segment.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (seg_max - seg_min) > 0.4 * swing
}

/// Decode an SPI data line clocked by `clock`.
///
/// `chip_select` (active low) frames words and gates edges when supplied. `clock_analog`,
/// when given, is used to tell a missed clock edge from a genuine inter-word gap.
pub fn decode(
    clock: &[bool],
    data: &[bool],
    chip_select: Option<&[bool]>,
    clock_analog: Option<&[f64]>,
    options: SpiOptions,
) -> Vec<Event> {
    let n = clock.len();
    let sample_rising = if options.auto_mode {
        detect_sample_rising(clock, data)
    } else {
        options.cpol == options.cpha
    };

    let clock_edges: Vec<usize> = (1..n)
        .filter(|&i| {
            if sample_rising {
                clock[i] && !clock[i - 1]
            } else {
                !clock[i] && clock[i - 1]
            }
        })
        .collect();

    let median_period = if chip_select.is_none() && clock_edges.len() > 2 {
        let mut gaps: Vec<f64> = clock_edges.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
        gaps.sort_by(f64::total_cmp);
        Some(gaps[gaps.len() / 2])
    } else {
        None
    };

    // Collect sampled bits into bursts, splitting on deselect or an idle-clock gap and
    // reconstructing edges the clock lost.
    let mut bursts: Vec<Vec<(bool, usize)>> = vec![Vec::new()];
    let mut previous_active = true;
    let mut previous_edge: Option<usize> = None;

    for &edge in &clock_edges {
        if let Some(cs) = chip_select {
            let active = !cs[edge.min(cs.len() - 1)];
            if !active {
                if !bursts.last().unwrap().is_empty() {
                    bursts.push(Vec::new());
                }
                previous_active = false;
                continue;
            }
            if !previous_active && !bursts.last().unwrap().is_empty() {
                bursts.push(Vec::new());
            }
            previous_active = true;
        } else if let (Some(median), Some(previous)) = (median_period, previous_edge) {
            let gap = (edge - previous) as f64;
            let periods = round_half_even(gap / median) as usize;
            if (2..=options.max_missed).contains(&periods)
                && (gap - periods as f64 * median).abs() <= 0.5 * median
            {
                match clock_analog {
                    Some(analog) if !gap_has_pulse(analog, previous, edge, median) => {
                        if !bursts.last().unwrap().is_empty() {
                            bursts.push(Vec::new());
                        }
                    }
                    _ => {
                        for k in 1..periods {
                            let index = previous + round_half_even(k as f64 * median) as usize;
                            push_bit(&mut bursts, data, index);
                        }
                    }
                }
            } else if options.word_gap > 0.0
                && gap > options.word_gap * median
                && !bursts.last().unwrap().is_empty()
            {
                bursts.push(Vec::new());
            }
        }
        previous_edge = Some(edge);
        push_bit(&mut bursts, data, edge);
    }
    bursts.retain(|b| !b.is_empty());

    let anchor_end = match options.anchor {
        Anchor::End => true,
        Anchor::Start => false,
        Anchor::Auto => match (median_period, clock_edges.first(), clock_edges.last()) {
            (Some(median), Some(&first), Some(&last)) if chip_select.is_none() => {
                let clean_start = first as f64 > options.word_gap * median;
                let clean_end = (n - last) as f64 > options.word_gap * median;
                clean_end && !clean_start
            }
            _ => false,
        },
    };

    let mut out = Vec::new();
    for (index, burst) in bursts.iter().enumerate() {
        let burst = if index == 0 && anchor_end {
            &burst[burst.len() % options.bits..]
        } else {
            &burst[..]
        };
        for group in burst.chunks_exact(options.bits) {
            let mut value = 0u32;
            for (j, &(bit, _)) in group.iter().enumerate() {
                if options.msb_first {
                    value = (value << 1) | u32::from(bit);
                } else {
                    value |= u32::from(bit) << j;
                }
            }
            out.push(Event {
                start: group[0].1,
                end: group[group.len() - 1].1,
                value: Some(value as u8),
                ok: true,
                kind: Kind::Byte,
            });
        }
    }
    out
}

/// Sample the data line at `index` into the current burst.
fn push_bit(bursts: &mut [Vec<(bool, usize)>], data: &[bool], index: usize) {
    let bit = data[index.min(data.len().saturating_sub(1))];
    bursts.last_mut().expect("a burst is always open").push((bit, index));
}
