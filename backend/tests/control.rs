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
