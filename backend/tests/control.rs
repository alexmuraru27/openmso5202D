//! Tests for the control layer's pure logic: op labels, weights, and the progress model.
//! Executing a plan needs hardware and is covered by `tests/hardware.rs`.

use mso5202d::control::ops::{format_time, format_volts};
use mso5202d::control::{Op, ProgressEvent, StepState};
use mso5202d::settings::{Probe, StoreDepth};

// --- labels -----------------------------------------------------------------

#[test]
fn labels_read_like_the_user_facing_operation() {
    assert_eq!(Op::DefaultSetup.label(), "Resetting to default setup");
    assert_eq!(
        Op::SetChannel { channel: 1, on: true }.label(),
        "Turning on CH1"
    );
    assert_eq!(
        Op::SetChannel { channel: 2, on: false }.label(),
        "Turning off CH2"
    );
    assert_eq!(
        Op::SetProbe { channel: 1, probe: Probe::X1 }.label(),
        "Setting CH1 probe to 1x"
    );
    assert_eq!(
        Op::SetDepth { depth: StoreDepth::K512 }.label(),
        "Setting acquisition depth to 512K"
    );
    assert_eq!(
        Op::SetVoltsPerDiv { channel: 1, millivolts: 1000 }.label(),
        "Setting CH1 to 1 V/div"
    );
    assert_eq!(
        Op::SetTimePerDiv { nanoseconds: 2000 }.label(),
        "Setting timebase to 2 µs/div"
    );
    assert_eq!(Op::CaptureSingle.label(), "Capturing a single sequence");
    assert_eq!(
        Op::SaveCsv { source: CsvSource::Ch1 }.label(),
        "Saving CH1 to card"
    );
    assert_eq!(
        Op::Download { source: CsvSource::Ch2 }.label(),
        "Downloading CH2 record"
    );
    assert_eq!(Op::ClearCard.label(), "Clearing exported CSVs from card");
}

#[test]
fn volts_are_formatted_the_way_the_scope_shows_them() {
    assert_eq!(format_volts(2), "2 mV");
    assert_eq!(format_volts(500), "500 mV");
    assert_eq!(format_volts(1000), "1 V");
    assert_eq!(format_volts(5000), "5 V");
    assert_eq!(format_volts(2500), "2.5 V");
}

#[test]
fn times_are_formatted_the_way_the_scope_shows_them() {
    assert_eq!(format_time(2), "2 ns");
    assert_eq!(format_time(200), "200 ns");
    assert_eq!(format_time(2_000), "2 µs");
    assert_eq!(format_time(40_000), "40 µs");
    assert_eq!(format_time(1_000_000), "1 ms");
    assert_eq!(format_time(5_000_000_000), "5 s");
}

// --- progress model ---------------------------------------------------------

/// An event in a three-step plan.
fn event(index: usize, state: StepState) -> ProgressEvent {
    ProgressEvent {
        index,
        total: 3,
        label: "step".into(),
        state,
    }
}

#[test]
fn events_carry_the_plan_shape_from_the_first_one() {
    // A linear bar needs index and total, and total must be final from the start —
    // the plan's length is known before anything runs.
    let first = event(0, StepState::Started);
    assert_eq!(first.index, 0);
    assert_eq!(first.total, 3);
}

#[test]
fn sub_progress_is_available_for_long_steps() {
    // Steps that dominate a run (writing to the card, downloading) report their own
    // progress; this is what replaces per-step weighting.
    let halfway = event(2, StepState::Advanced { done: 50, total: 100 });
    assert_eq!(
        halfway.state,
        StepState::Advanced { done: 50, total: 100 }
    );
    assert!(halfway.to_string().contains("50/100"));
}

#[test]
fn a_failed_step_reports_its_cause() {
    let failed = event(1, StepState::Failed { error: "end stop".into() });
    let rendered = failed.to_string();
    assert!(rendered.contains("FAILED"), "{rendered}");
    assert!(rendered.contains("end stop"), "{rendered}");
    assert!(rendered.contains("[2/3]"), "step number must be 1-based: {rendered}");
}

#[test]
fn events_render_for_a_log_line() {
    assert!(event(0, StepState::Started).to_string().contains("[1/3]"));
    assert!(event(0, StepState::Completed { elapsed_ms: 42 })
        .to_string()
        .contains("42 ms"));
}

// --- CSV menu screen reading ------------------------------------------------

use mso5202d::control::csv::{
    is_wavedata, save_in_progress, selected_source, wavedata_files, wavedata_number, CsvSource,
};
use mso5202d::device::screen::{Screenshot, FRAMEBUFFER_BYTES, SCREEN_WIDTH};
use mso5202d::device::FileEntry;

/// A painted rectangle: horizontal band, vertical band, RGB565 colour.
type Rect = ((usize, usize), (usize, usize), u16);

/// Build a screenshot, painting rectangles of RGB565 colour.
fn screen_with(regions: &[Rect]) -> Screenshot {
    let mut raw = vec![0u8; FRAMEBUFFER_BYTES];
    for &((x0, x1), (y0, y1), colour) in regions {
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * SCREEN_WIDTH + x) * 2;
                raw[i..i + 2].copy_from_slice(&colour.to_le_bytes());
            }
        }
    }
    Screenshot::from_rgb565(&raw).expect("full-size buffer")
}

/// The three Source radio dots: two identical (unselected) and one distinct (selected).
fn screen_with_source(selected: usize) -> Screenshot {
    const ROWS: [(usize, usize); 3] = [(58, 72), (80, 94), (102, 116)];
    let regions: Vec<_> = ROWS
        .iter()
        .enumerate()
        .map(|(row, &band)| {
            // Selected dot is filled bright; unselected dots are identical dim rings.
            let colour = if row == selected { 0xFFFF } else { 0x2104 };
            ((656usize, 676usize), band, colour)
        })
        .collect();
    screen_with(&regions)
}

#[test]
fn source_radio_is_read_as_the_odd_one_out() {
    // Deliberately not colour-matching: the selected dot is whichever differs from the
    // identical pair, so the reading survives any firmware theme.
    assert_eq!(selected_source(&screen_with_source(0)), Some(CsvSource::Ch1));
    assert_eq!(selected_source(&screen_with_source(1)), Some(CsvSource::Ch2));
    assert_eq!(selected_source(&screen_with_source(2)), Some(CsvSource::La));
}

#[test]
fn a_uniform_radio_group_is_rejected_rather_than_guessed() {
    // All three dots identical — the menu is not on screen, or the grab was poor. Guessing
    // here would silently export the wrong channel.
    let blank = screen_with(&[]);
    assert_eq!(selected_source(&blank), None);
}

#[test]
fn the_busy_banner_is_detected() {
    // While this banner is up the scope ignores key presses.
    let busy = screen_with(&[((160, 535), (230, 245), 0xFC00)]); // orange
    assert!(save_in_progress(&busy));

    let idle = screen_with(&[]);
    assert!(!save_in_progress(&idle));

    // A banner-coloured patch elsewhere on screen must not read as busy.
    let elsewhere = screen_with(&[((0, 100), (400, 460), 0xFC00)]);
    assert!(!save_in_progress(&elsewhere));
}

// --- exported file naming ---------------------------------------------------

fn entry(name: &str, size: u64) -> FileEntry {
    FileEntry { name: name.into(), size, is_dir: false }
}

#[test]
fn wavedata_files_are_recognised_and_ordered() {
    let listing = vec![
        entry("WaveData1412.csv", 40064),
        entry("notes.txt", 10),
        entry("WaveData1410.csv", 400064),
        entry("pic_141_1.bmp", 100),
        entry("WaveData1411.csv", 4064),
    ];
    let found = wavedata_files(&listing);
    let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["WaveData1410.csv", "WaveData1411.csv", "WaveData1412.csv"]);
}

#[test]
fn only_exported_waveforms_match() {
    assert!(is_wavedata("WaveData1410.csv"));
    assert!(is_wavedata("wavedata9.CSV"), "matching must be case-insensitive");
    assert!(!is_wavedata("WaveData1410.txt"));
    assert!(!is_wavedata("pic_141_1.bmp"));
    assert!(!is_wavedata(""));
}

#[test]
fn file_numbers_order_by_sequence_not_string() {
    // String ordering would put 1410 after 999; the embedded number must win.
    assert!(wavedata_number("WaveData1410.csv") > wavedata_number("WaveData999.csv"));
    assert_eq!(wavedata_number("WaveData1410.csv"), 1410);
    assert_eq!(wavedata_number("nodigits.csv"), 0);
}
