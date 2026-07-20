//! Tests for parsing the scope's exported waveform CSVs.

use mso5202d::waveform::parse_csv;

const ANALOG: &str = "\
#timebase=2000000(ns)
,#voltbase=1000000(mv/100)
#size=4
1.00000E-07,3.360
2.00000E-07,3.360
3.00000E-07,0.040
4.00000E-07,0.040
";

const LOGIC: &str = "\
#timebase=200000000(ns)
,#threshold=1400(mv)
#size=3
1.00000E-06,65535
2.00000E-06,0
3.00000E-06,258
";

#[test]
fn analog_export_parses_headers_and_samples() {
    let parsed = parse_csv(ANALOG).unwrap();
    assert!(!parsed.is_logic());
    assert_eq!(parsed.size, Some(4));
    assert_eq!(parsed.len(), 4);
    assert_eq!(parsed.volts.as_deref(), Some(&[3.36, 3.36, 0.04, 0.04][..]));
    // #voltbase is µV/div despite its label, so 1000000 is 1 V/div = 1000 mV.
    assert_eq!(parsed.volts_per_div_mv, Some(1000.0));
    // #timebase is picoseconds despite its label: 2000000 ps = 2 µs/div.
    assert_eq!(parsed.timebase_ps, Some(2_000_000));
    assert!(parsed.words.is_none());
}

#[test]
fn sample_interval_comes_from_the_data_not_the_header() {
    // The header timebase is a screen tag; deeper records sample faster, so the true
    // interval can only come from the timestamps.
    let parsed = parse_csv(ANALOG).unwrap();
    let dt = parsed.dt_s.expect("interval");
    assert!((dt - 1e-7).abs() < 1e-15, "dt {dt}");
}

#[test]
fn logic_export_parses_as_words() {
    let parsed = parse_csv(LOGIC).unwrap();
    assert!(parsed.is_logic());
    assert_eq!(parsed.words.as_deref(), Some(&[65535u16, 0, 258][..]));
    assert_eq!(parsed.threshold_mv, Some(1400));
    assert!(parsed.volts.is_none(), "a logic export carries no volts");
}

#[test]
fn a_body_without_headers_still_parses() {
    let parsed = parse_csv("1.0,0.5\n2.0,1.5\n").unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed.volts.as_deref(), Some(&[0.5, 1.5][..]));
    assert_eq!(parsed.size, None);
}

#[test]
fn malformed_rows_are_skipped_rather_than_failing_the_file() {
    // The files come off embedded firmware; one bad line should not lose the record.
    let text = "#size=2\n1.0,0.5\ngarbage\n\n2.0,1.5\n";
    let parsed = parse_csv(text).unwrap();
    assert_eq!(parsed.len(), 2);
}

#[test]
fn an_empty_export_is_empty_not_an_error() {
    let parsed = parse_csv("").unwrap();
    assert!(parsed.is_empty());
    assert_eq!(parsed.dt_s, None);
}
