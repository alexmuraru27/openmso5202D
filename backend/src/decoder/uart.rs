//! UART decoder — asynchronous, LSB-first, start/stop framed.
//!
//! Decodes on a least-squares **bit grid** rather than by hunting start edges, and picks
//! the byte phase by whichever framing validates the most frames. That is what makes a
//! solid back-to-back stream decode correctly: in continuous mode every data 1→0 edge
//! mimics a start bit, so edge-hunting frames at the wrong boundary and produces garbage
//! that still looks byte-shaped.
//!
//! A capture that triggers mid-byte drops its leading partial frame and decodes the clean
//! tail.

use super::common as front;
use super::{Event, Kind};

/// Parity checking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Parity {
    /// No parity bit in the frame.
    #[default]
    None,
    /// Even parity.
    Even,
    /// Odd parity.
    Odd,
}

/// How the line's resting level is determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Idle {
    /// Idle high — the usual TTL convention, and the reliable default.
    #[default]
    High,
    /// Idle low — an inverted line.
    Low,
    /// Best effort. Unreliable on some continuous streams; prefer stating it explicitly.
    Auto,
}

/// UART decoding options.
#[derive(Debug, Clone, Copy)]
pub struct UartOptions {
    /// Sample interval in nanoseconds. Combined with [`UartOptions::baud`] this locks the
    /// bit period; without both, the period is guessed from the shortest pulse.
    pub sample_interval_ns: Option<f64>,
    /// Line rate in bits per second.
    pub baud: Option<f64>,
    /// Data bits per frame.
    pub bits: usize,
    /// Parity mode.
    pub parity: Parity,
    /// Stop bits per frame.
    pub stops: usize,
    /// Resting level.
    pub idle: Idle,
    /// Also try the time-reversed trace and keep whichever decodes more.
    pub both_ways: bool,
}

impl Default for UartOptions {
    fn default() -> Self {
        Self {
            sample_interval_ns: None,
            baud: None,
            bits: 8,
            parity: Parity::None,
            stops: 1,
            idle: Idle::High,
            both_ways: false,
        }
    }
}

/// Decode an asynchronous UART line.
pub fn decode(trace: &[bool], options: UartOptions) -> Vec<Event> {
    if options.idle == Idle::Auto {
        return decode_auto_polarity(trace, options);
    }
    if options.both_ways {
        return front::both_ways(
            |reverse| decode_once(trace, reverse, options),
            |frames| frames.len(),
        );
    }
    decode_once(trace, false, options)
}

/// Try both polarities and keep the better read.
///
/// The wrong polarity flips the start/stop template and usually validates far fewer
/// frames. When the counts are close — a framed stream can coincidentally frame the same
/// number at a shifted phase — the idle level breaks the tie.
fn decode_auto_polarity(trace: &[bool], options: UartOptions) -> Vec<Event> {
    let high = decode(
        trace,
        UartOptions {
            idle: Idle::High,
            ..options
        },
    );
    let low = decode(
        trace,
        UartOptions {
            idle: Idle::Low,
            ..options
        },
    );
    let (shorter, longer) = (high.len().min(low.len()), high.len().max(low.len()).max(1));
    if shorter as f64 >= 0.8 * longer as f64 {
        return if front::idle_level(trace) { high } else { low };
    }
    if high.len() > low.len() {
        high
    } else {
        low
    }
}

/// One-direction decode on a bit grid.
///
/// When `reverse` is set the trace is decoded time-reversed, so the frame cells run
/// stop → data (reversed) → start, and indices are mapped back to the original frame.
fn decode_once(trace: &[bool], reverse: bool, options: UartOptions) -> Vec<Event> {
    let n = trace.len();
    if n < 2 {
        return Vec::new();
    }
    let owned: Vec<bool>;
    let trace = if reverse {
        owned = trace.iter().rev().copied().collect();
        &owned[..]
    } else {
        trace
    };

    let initial_spb = match (options.baud, options.sample_interval_ns) {
        (Some(baud), Some(interval)) if baud > 0.0 && interval > 0.0 => {
            (1e9 / baud) / interval
        }
        _ => front::min_pulse(trace) as f64,
    };
    if initial_spb < 2.0 {
        return Vec::new();
    }

    let edge_indices = front::edges(trace);
    let (spb, phase) = if edge_indices.len() >= 3 {
        front::refine_period(&edge_indices, initial_spb)
    } else {
        (
            initial_spb,
            edge_indices.first().map(|&e| e as f64).unwrap_or(0.0),
        )
    };
    if spb < 2.0 {
        return Vec::new();
    }

    let parity_bits = usize::from(options.parity != Parity::None);
    let frame_len = 1 + options.bits + parity_bits + options.stops;
    let (bits, centres) = front::sample_grid(trace, spb, phase, true);
    if bits.len() < frame_len {
        return Vec::new();
    }

    let idle = options.idle == Idle::High;
    let layout = FrameLayout::new(reverse, options.bits, parity_bits, options.stops, frame_len);

    // Which windows are valid frames: start cell at the non-idle level and stop cells idle.
    let candidates = bits.len().saturating_sub(frame_len - 1);
    let matches: Vec<Option<(u8, bool)>> = (0..candidates)
        .map(|i| layout.match_frame(&bits[i..i + frame_len], idle, options))
        .collect();

    // Two framings, keeping whichever validates more:
    //   (A) fixed-offset tiling — the correct byte boundary for a gapless stream, where a
    //       wrong phase piles up stop-bit violations on real data;
    //   (B) a greedy resync walk — needed when frames are separated by idle gaps, which
    //       fixed tiling cannot follow.
    // Both skip an invalid leading frame and continue from the first clean one.
    let mut best_tiled: Vec<Event> = Vec::new();
    for offset in 0..frame_len {
        let tiled: Vec<Event> = (offset..matches.len())
            .step_by(frame_len)
            .filter_map(|i| matches[i].map(|m| layout.event(i, m, &centres, n, reverse)))
            .collect();
        if tiled.len() > best_tiled.len() {
            best_tiled = tiled;
        }
    }

    let mut walked: Vec<Event> = Vec::new();
    let mut i = 0;
    while i < matches.len() {
        match matches[i] {
            Some(m) => {
                walked.push(layout.event(i, m, &centres, n, reverse));
                i += frame_len;
            }
            None => i += 1,
        }
    }

    let mut frames = if best_tiled.len() >= walked.len() {
        best_tiled
    } else {
        walked
    };
    if reverse {
        frames.reverse();
    }
    frames
}

/// Where each cell of a frame sits, forwards or reversed.
struct FrameLayout {
    start_cell: usize,
    data_cells: Vec<usize>,
    parity_cell: Option<usize>,
    stop_cells: Vec<usize>,
    frame_len: usize,
}

impl FrameLayout {
    fn new(reverse: bool, bits: usize, parity_bits: usize, stops: usize, frame_len: usize) -> Self {
        if !reverse {
            // [start][data LSB..MSB][parity?][stop*]
            Self {
                start_cell: 0,
                data_cells: (0..bits).map(|b| 1 + b).collect(),
                parity_cell: (parity_bits > 0).then_some(1 + bits),
                stop_cells: (0..stops).map(|s| 1 + bits + parity_bits + s).collect(),
                frame_len,
            }
        } else {
            // [stop*][parity?][data MSB..LSB][start]
            let base = stops + parity_bits;
            Self {
                start_cell: frame_len - 1,
                data_cells: (0..bits).map(|b| base + (bits - 1 - b)).collect(),
                parity_cell: (parity_bits > 0).then_some(stops),
                stop_cells: (0..stops).collect(),
                frame_len,
            }
        }
    }

    /// Validate one window and extract its value, or `None` if it is not a frame.
    fn match_frame(&self, cells: &[bool], idle: bool, options: UartOptions) -> Option<(u8, bool)> {
        // The start bit sits at the opposite level to idle.
        if cells[self.start_cell] == idle {
            return None;
        }
        if self.stop_cells.iter().any(|&c| cells[c] != idle) {
            return None;
        }
        // A logical 1 is the mark level, which equals idle — so this also reads an
        // inverted line correctly.
        let mut value = 0u8;
        for (b, &cell) in self.data_cells.iter().enumerate() {
            if cells[cell] == idle {
                value |= 1 << b;
            }
        }
        let ok = match (self.parity_cell, options.parity) {
            (Some(cell), Parity::Even) => {
                (value.count_ones() as usize + usize::from(cells[cell] == idle)).is_multiple_of(2)
            }
            (Some(cell), Parity::Odd) => {
                (value.count_ones() as usize + usize::from(cells[cell] == idle)) % 2 == 1
            }
            _ => true,
        };
        Some((value, ok))
    }

    /// Build an event, mapping reversed indices back to the original frame.
    fn event(
        &self,
        i: usize,
        (value, ok): (u8, bool),
        centres: &[usize],
        n: usize,
        reverse: bool,
    ) -> Event {
        let (mut a, mut b) = (centres[i], centres[i + self.frame_len - 1]);
        if reverse {
            let (ra, rb) = (n - 1 - b, n - 1 - a);
            a = ra;
            b = rb;
        }
        Event {
            start: a.min(b),
            end: a.max(b),
            value: Some(value),
            ok,
            kind: Kind::Byte,
        }
    }
}
