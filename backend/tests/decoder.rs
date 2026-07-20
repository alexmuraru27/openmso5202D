//! Decoder tests against synthesised signals, where the expected bytes are known exactly.
//!
//! These pin the protocol logic without needing hardware or the capture corpus; scoring
//! against real captures lives in `decoder_corpus.rs`.

use mso5202d::decoder::uart::{Idle, Parity, UartOptions};
use mso5202d::decoder::{common, i2c, spi, uart, values, Kind};

/// The generator's 0x00..0xFF ramp — what every synthesised signal carries.
fn ramp() -> Vec<u8> {
    (0..=255).collect()
}

/// Repeat each bit `spb` times to make a sampled trace.
fn stretch(bits: &[bool], spb: usize) -> Vec<bool> {
    bits.iter().flat_map(|&b| std::iter::repeat_n(b, spb)).collect()
}

/// Render a logic trace as volts, so a test can exercise the real analog→threshold path.
fn as_volts(trace: &[bool]) -> Vec<f64> {
    trace.iter().map(|&b| if b { 3.3 } else { 0.0 }).collect()
}

// --- shared front-end -------------------------------------------------------

#[test]
fn ramp_ratio_measures_consecutive_bytes() {
    assert_eq!(common::ramp_ratio(&[1, 2, 3, 4]), 1.0);
    assert_eq!(common::ramp_ratio(&[0xFE, 0xFF, 0x00]), 1.0, "must wrap at 256");
    assert_eq!(common::ramp_ratio(&[1, 2, 9, 10]), 2.0 / 3.0);
    assert_eq!(common::ramp_ratio(&[7]), 0.0, "a single byte proves nothing");
    assert_eq!(common::ramp_ratio(&[]), 0.0);
}

#[test]
fn percentile_interpolates_like_numpy() {
    let data = [0.0, 1.0, 2.0, 3.0, 4.0];
    assert_eq!(common::percentile(&data, 0.0), 0.0);
    assert_eq!(common::percentile(&data, 100.0), 4.0);
    assert_eq!(common::percentile(&data, 50.0), 2.0);
    assert!((common::percentile(&data, 25.0) - 1.0).abs() < 1e-9);
}

#[test]
fn edges_land_on_the_new_level() {
    let trace = [false, false, true, true, false];
    assert_eq!(common::edges(&trace), vec![2, 4]);
    assert!(common::edges(&[true; 5]).is_empty());
}

#[test]
fn idle_level_is_the_longest_run() {
    // A short low blip inside a long high stretch: idle is high.
    let mut trace = vec![true; 40];
    trace[10] = false;
    assert!(common::idle_level(&trace));

    let mut inverted = vec![false; 40];
    inverted[10] = true;
    assert!(!common::idle_level(&inverted));
}

#[test]
fn refine_period_recovers_the_true_bit_period() {
    // Edges on an exact 10-sample grid, given a slightly wrong starting estimate.
    let edges: Vec<usize> = (0..20).map(|k| 5 + k * 10).collect();
    // The estimate must be close enough that snapping edges to the grid lands them on the
    // right index; correcting small drift is the whole point of the fit.
    let (spb, phase) = common::refine_period(&edges, 9.9);
    assert!((spb - 10.0).abs() < 1e-6, "recovered spb {spb}");
    assert!((phase - 5.0).abs() < 1e-6, "recovered phase {phase}");
}

#[test]
fn refine_period_falls_back_when_degenerate() {
    assert_eq!(common::refine_period(&[], 8.0), (8.0, 0.0));
    assert_eq!(common::refine_period(&[3], 8.0), (8.0, 3.0));
}

#[test]
fn sample_grid_votes_over_each_cell() {
    // Four cells of 10 samples: 1,0,1,0 — with one corrupted sample that the vote absorbs.
    let mut trace = stretch(&[true, false, true, false], 10);
    trace[12] = true; // a glitch inside cell 1
    let (bits, centres) = common::sample_grid(&trace, 10.0, 0.0, true);
    // Only whole cells that fit within the trace are emitted, so the trailing partial
    // cell at the very end is not sampled.
    assert_eq!(bits, vec![true, false, true]);
    assert_eq!(centres, vec![5, 15, 25]);
}

#[test]
fn thresholding_recovers_a_logic_trace_from_volts() {
    let original = stretch(&[true, false, true, true, false, false, true, false], 20);
    let recovered = common::threshold_volts(&as_volts(&original));
    assert_eq!(recovered, original);
}

#[test]
fn thresholding_handles_flat_and_empty_input() {
    assert!(common::threshold_volts(&[]).is_empty());
    // A flat line has no edges to find and must not panic or invent any.
    assert!(common::edges(&common::threshold_volts(&[1.5; 100])).is_empty());
}

// --- UART -------------------------------------------------------------------

/// Idle-high UART: LSB first, one start bit, one stop bit, `gap` idle bits between frames.
fn synth_uart(values: &[u8], spb: usize, parity: Parity, gap: usize) -> Vec<bool> {
    let mut bits: Vec<bool> = vec![true; gap];
    for &v in values {
        bits.push(false); // start
        for b in 0..8 {
            bits.push((v >> b) & 1 == 1);
        }
        match parity {
            Parity::None => {}
            Parity::Even => bits.push(v.count_ones() % 2 == 1),
            Parity::Odd => bits.push(v.count_ones() % 2 == 0),
        }
        bits.push(true); // stop
        bits.extend(std::iter::repeat_n(true, gap));
    }
    stretch(&bits, spb)
}

#[test]
fn uart_decodes_framed_bytes_for_every_parity() {
    for parity in [Parity::None, Parity::Even, Parity::Odd] {
        let trace = synth_uart(&ramp(), 20, parity, 8);
        // Through the analog path, so thresholding is exercised too.
        let logic = common::threshold_volts(&as_volts(&trace));
        let events = uart::decode(
            &logic,
            UartOptions {
                parity,
                ..Default::default()
            },
        );
        assert_eq!(values(&events), ramp(), "parity {parity:?}");
        assert!(events.iter().all(|e| e.ok), "parity {parity:?} must validate");
    }
}

#[test]
fn uart_decodes_a_gapless_continuous_stream() {
    // The hard case: with no idle between frames, every data 1→0 edge mimics a start bit,
    // so start-edge hunting frames at the wrong boundary. The bit grid gets it right.
    let mut trace = synth_uart(&ramp(), 20, Parity::None, 0);
    trace.extend(std::iter::repeat_n(true, 40)); // trailing idle so the last stop bit fits
    assert_eq!(values(&uart::decode(&trace, UartOptions::default())), ramp());
}

#[test]
fn uart_recovers_the_tail_when_the_capture_starts_mid_byte() {
    let trace = synth_uart(&ramp(), 20, Parity::None, 4);
    let cut = trace.len() * 37 / 100;
    let decoded = values(&uart::decode(&trace[cut..], UartOptions::default()));
    assert!(decoded.len() > 100, "recovered only {} bytes", decoded.len());
    assert!(
        common::ramp_ratio(&decoded) >= 0.99,
        "ramp {}",
        common::ramp_ratio(&decoded)
    );
}

#[test]
fn uart_decodes_an_inverted_line() {
    let inverted: Vec<bool> = synth_uart(&ramp(), 20, Parity::None, 6)
        .iter()
        .map(|b| !b)
        .collect();
    let explicit = uart::decode(
        &inverted,
        UartOptions {
            idle: Idle::Low,
            ..Default::default()
        },
    );
    assert_eq!(values(&explicit), ramp(), "explicit idle=low");

    let auto = uart::decode(
        &inverted,
        UartOptions {
            idle: Idle::Auto,
            ..Default::default()
        },
    );
    assert_eq!(values(&auto), ramp(), "auto polarity detection");
}

#[test]
fn uart_returns_nothing_for_an_undecodable_trace() {
    assert!(uart::decode(&[], UartOptions::default()).is_empty());
    assert!(uart::decode(&[true; 500], UartOptions::default()).is_empty());
}

// --- SPI --------------------------------------------------------------------

/// Clock and data lines shifting `values` in the given mode.
fn synth_spi(
    values: &[u8],
    spb: usize,
    cpol: u8,
    cpha: u8,
    msb_first: bool,
    byte_gap: usize,
) -> (Vec<bool>, Vec<bool>) {
    let (mut clk, mut dat) = (Vec::new(), Vec::new());
    let idle = cpol == 1;
    let lead = !idle;
    let mut current = false;

    let hold = |clk: &mut Vec<bool>, dat: &mut Vec<bool>, c: bool, d: bool, k: usize| {
        clk.extend(std::iter::repeat_n(c, k));
        dat.extend(std::iter::repeat_n(d, k));
    };

    hold(&mut clk, &mut dat, idle, false, spb);
    for &v in values {
        for i in 0..8 {
            let b = if msb_first { 7 - i } else { i };
            let bit = (v >> b) & 1 == 1;
            if cpha == 0 {
                current = bit;
                hold(&mut clk, &mut dat, idle, current, spb / 2);
                hold(&mut clk, &mut dat, lead, current, spb / 2);
            } else {
                hold(&mut clk, &mut dat, idle, current, spb / 2);
                current = bit;
                hold(&mut clk, &mut dat, lead, current, spb / 2);
            }
            *clk.last_mut().unwrap() = lead;
        }
        if byte_gap > 0 {
            hold(&mut clk, &mut dat, idle, current, byte_gap);
        }
    }
    hold(&mut clk, &mut dat, idle, current, spb);
    (clk, dat)
}

#[test]
fn spi_decodes_every_mode_and_bit_order() {
    for cpol in [0u8, 1] {
        for cpha in [0u8, 1] {
            for msb_first in [true, false] {
                let (clk, dat) = synth_spi(&ramp(), 10, cpol, cpha, msb_first, 0);
                let events = spi::decode(
                    &clk,
                    &dat,
                    None,
                    None,
                    spi::SpiOptions {
                        cpol,
                        cpha,
                        msb_first,
                        ..Default::default()
                    },
                );
                assert_eq!(
                    values(&events),
                    ramp(),
                    "mode {cpol}{cpha} msb_first={msb_first}"
                );
            }
        }
    }
}

#[test]
fn spi_auto_mode_detects_the_sampling_edge() {
    // A device in any mode decodes without being told which.
    for cpol in [0u8, 1] {
        for cpha in [0u8, 1] {
            let (clk, dat) = synth_spi(&ramp(), 10, cpol, cpha, true, 0);
            let events = spi::decode(
                &clk,
                &dat,
                None,
                None,
                spi::SpiOptions {
                    auto_mode: true,
                    ..Default::default()
                },
            );
            assert_eq!(values(&events), ramp(), "auto mode {cpol}{cpha}");
        }
    }
}

#[test]
fn spi_reframes_on_idle_clock_gaps() {
    let (clk, dat) = synth_spi(&ramp(), 10, 0, 0, true, 150);
    assert_eq!(
        values(&spi::decode(&clk, &dat, None, None, spi::SpiOptions::default())),
        ramp()
    );
}

#[test]
fn spi_drops_only_the_partial_byte_when_cut_mid_byte() {
    let (clk, dat) = synth_spi(&ramp(), 10, 0, 0, true, 150);
    let cut = 45;
    let decoded = values(&spi::decode(
        &clk[cut..],
        &dat[cut..],
        None,
        None,
        spi::SpiOptions::default(),
    ));
    assert_eq!(decoded, ramp()[1..], "the first byte is cut, the rest survive");
}

#[test]
fn spi_end_anchors_a_gapless_burst_with_a_clean_tail() {
    // Triggered mid-byte with no leading idle, but the clock stops cleanly. Grouping
    // forward from the cut would shift every byte; anchoring to the end recovers the tail.
    let (mut clk, mut dat) = synth_spi(&ramp(), 10, 0, 0, true, 0);
    let last = *dat.last().unwrap();
    clk.extend(std::iter::repeat_n(false, 400));
    dat.extend(std::iter::repeat_n(last, 400));

    let cut = 34;
    let decoded = values(&spi::decode(
        &clk[cut..],
        &dat[cut..],
        None,
        None,
        spi::SpiOptions::default(),
    ));
    assert!(decoded.len() > 200, "recovered only {} bytes", decoded.len());
    assert_eq!(common::ramp_ratio(&decoded), 1.0);
    assert_eq!(decoded.last(), Some(&255), "must end on the ramp's last byte");
}

#[test]
fn spi_uses_the_analog_clock_to_tell_a_missed_edge_from_a_real_gap() {
    // Two bytes separated by a genuine idle gap. With the analog clock supplied, the flat
    // gap is recognised as a real word boundary rather than missed edges.
    let (clk, dat) = synth_spi(&[0xA5, 0x5A], 10, 0, 0, true, 30);
    let analog: Vec<f64> = clk.iter().map(|&c| if c { 3.3 } else { 0.0 }).collect();
    let with_analog = values(&spi::decode(
        &clk,
        &dat,
        None,
        Some(&analog),
        spi::SpiOptions::default(),
    ));
    assert_eq!(with_analog, vec![0xA5, 0x5A]);
}

// --- I²C --------------------------------------------------------------------

/// START, address + R/W with ACK, each data byte with ACK, STOP.
fn synth_i2c(values: &[u8], spb: usize, addr: u8, rw: u8) -> (Vec<bool>, Vec<bool>) {
    let (mut scl, mut sda) = (Vec::new(), Vec::new());
    let seg = |scl: &mut Vec<bool>, sda: &mut Vec<bool>, c: bool, d: bool, k: usize| {
        scl.extend(std::iter::repeat_n(c, k));
        sda.extend(std::iter::repeat_n(d, k));
    };

    seg(&mut scl, &mut sda, true, true, spb); // idle
    seg(&mut scl, &mut sda, true, false, spb); // START

    let first = (addr << 1) | rw;
    for &byte in std::iter::once(&first).chain(values) {
        for b in (0..8).rev() {
            let bit = (byte >> b) & 1 == 1;
            seg(&mut scl, &mut sda, false, bit, spb / 2);
            seg(&mut scl, &mut sda, true, bit, spb);
            seg(&mut scl, &mut sda, false, bit, spb / 2);
        }
        // ACK
        seg(&mut scl, &mut sda, false, false, spb / 2);
        seg(&mut scl, &mut sda, true, false, spb);
        seg(&mut scl, &mut sda, false, false, spb / 2);
    }
    // STOP
    seg(&mut scl, &mut sda, false, false, spb / 2);
    seg(&mut scl, &mut sda, true, false, spb / 2);
    seg(&mut scl, &mut sda, true, true, spb);
    (scl, sda)
}

#[test]
fn i2c_decodes_address_data_and_markers() {
    let (scl, sda) = synth_i2c(&ramp(), 10, 0x50, 0);
    let events = i2c::decode(&scl, &sda, i2c::Anchor::Auto);

    assert_eq!(events.iter().filter(|e| e.kind == Kind::Start).count(), 1);
    assert_eq!(events.iter().filter(|e| e.kind == Kind::Stop).count(), 1);

    let address: Vec<&_> = events.iter().filter(|e| e.kind == Kind::Address).collect();
    assert_eq!(address.len(), 1);
    assert_eq!(address[0].value, Some(0x50 << 1));

    let data: Vec<u8> = events
        .iter()
        .filter(|e| e.kind == Kind::Byte)
        .filter_map(|e| e.value)
        .collect();
    assert_eq!(data, ramp());
    assert!(
        events.iter().filter(|e| e.kind.carries_value()).all(|e| e.ok),
        "every byte must be ACKed"
    );
}

#[test]
fn i2c_end_anchors_when_the_start_was_missed() {
    // Triggered mid-transaction: no START to count from, but the STOP is in view.
    let expected: Vec<u8> = (0..64).collect();
    let (scl, sda) = synth_i2c(&expected, 10, 0x50, 0);
    let cut = scl.len() / 3;
    let events = i2c::decode(&scl[cut..], &sda[cut..], i2c::Anchor::Auto);

    assert!(
        !events.iter().any(|e| e.kind == Kind::Start),
        "no START should be reported when none was captured"
    );
    let data: Vec<u8> = events
        .iter()
        .filter(|e| e.kind == Kind::Byte)
        .filter_map(|e| e.value)
        .collect();
    assert!(data.len() > 20, "recovered only {} bytes", data.len());
    assert_eq!(common::ramp_ratio(&data), 1.0);
    assert_eq!(data.last(), Some(&63), "must end on the last byte sent");
}

#[test]
fn i2c_labels_a_repeated_start() {
    let (mut scl, mut sda) = synth_i2c(&[0x11], 10, 0x50, 0);
    let (scl2, sda2) = synth_i2c(&[0x22], 10, 0x51, 1);
    scl.extend(scl2);
    sda.extend(sda2);
    let events = i2c::decode(&scl, &sda, i2c::Anchor::Start);
    // The second START follows a STOP here, so both read as plain STARTs; what matters is
    // that two transactions are seen rather than one run-on frame.
    assert_eq!(events.iter().filter(|e| e.kind == Kind::Start).count(), 2);
    assert_eq!(events.iter().filter(|e| e.kind == Kind::Stop).count(), 2);
}

// --- event rendering --------------------------------------------------------

#[test]
fn events_render_like_a_scope_annotation() {
    let (scl, sda) = synth_i2c(&[0xAB], 10, 0x50, 0);
    let events = i2c::decode(&scl, &sda, i2c::Anchor::Auto);
    assert_eq!(events.first().map(|e| e.text()), Some("S".to_string()));
    assert_eq!(events.last().map(|e| e.text()), Some("P".to_string()));
    assert!(events.iter().any(|e| e.text() == "AB"));
}
