//! Tests for the settings-block layout and field decoding.

use mso5202d::settings::{
    Settings, StoreDepth, TrigState, SETTINGS_LEN, SETTINGS_PARAMS, SIGNED_FIELDS,
};

/// Build a settings block with every byte zero, then apply `edits` as (offset, byte).
fn block_with(edits: &[(usize, u8)]) -> Vec<u8> {
    let mut raw = vec![0u8; SETTINGS_LEN];
    for &(offset, byte) in edits {
        raw[offset] = byte;
    }
    raw
}

/// Byte offset of a named field, computed the same way the wire lays it out.
fn offset_of(name: &str) -> usize {
    let mut offset = 0;
    for &(field, width) in SETTINGS_PARAMS {
        if field == name {
            return offset;
        }
        offset += width;
    }
    panic!("unknown field {name}");
}

#[test]
fn param_table_covers_the_whole_block() {
    let total: usize = SETTINGS_PARAMS.iter().map(|(_, width)| width).sum();
    assert_eq!(total, SETTINGS_LEN, "param widths must sum to the block size");
}

#[test]
fn param_names_are_unique() {
    let mut names: Vec<&str> = SETTINGS_PARAMS.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count, "duplicate field name in the param table");
}

#[test]
fn signed_fields_all_exist_in_the_table() {
    for name in SIGNED_FIELDS {
        assert!(
            SETTINGS_PARAMS.iter().any(|(field, _)| field == name),
            "signed field {name} is not in the param table"
        );
    }
}

#[test]
fn parse_accepts_bare_block_and_echo_prefixed_payload() {
    let bare = block_with(&[]);
    assert!(Settings::parse(&bare).is_ok());

    let mut with_echo = vec![0x81];
    with_echo.extend_from_slice(&bare);
    assert!(Settings::parse(&with_echo).is_ok());
}

#[test]
fn parse_rejects_wrong_length() {
    assert!(Settings::parse(&[0u8; 10]).is_err());
    // Right length but the wrong leading byte is not an echo-prefixed block.
    let mut bogus = vec![0x00];
    bogus.extend_from_slice(&[0u8; SETTINGS_LEN]);
    assert!(Settings::parse(&bogus).is_err());
}

#[test]
fn multi_byte_fields_decode_little_endian() {
    // TRIG-FREQUENCY is 8 bytes wide; 0x0201 little-endian = 513.
    let offset = offset_of("TRIG-FREQUENCY");
    let settings = Settings::parse(&block_with(&[(offset, 0x01), (offset + 1, 0x02)])).unwrap();
    assert_eq!(settings.field("TRIG-FREQUENCY"), Some(513));
}

#[test]
fn signed_fields_sign_extend_from_their_width() {
    // TRIG-VPOS is 2 bytes: 0xFFFF should read as -1, not 65535.
    let offset = offset_of("TRIG-VPOS");
    let settings = Settings::parse(&block_with(&[(offset, 0xFF), (offset + 1, 0xFF)])).unwrap();
    assert_eq!(settings.field("TRIG-VPOS"), Some(65535));
    assert_eq!(settings.field_signed("TRIG-VPOS"), Some(-1));
    // field_auto applies the signed convention automatically.
    assert_eq!(settings.field_auto("TRIG-VPOS"), Some(-1));
    // ...and leaves unsigned fields alone.
    assert_eq!(settings.field_auto("TRIG-FREQUENCY"), Some(0));
}

#[test]
fn unknown_field_names_return_none() {
    let settings = Settings::parse(&block_with(&[])).unwrap();
    assert_eq!(settings.field("NO-SUCH-FIELD"), None);
    assert_eq!(settings.field_signed("NO-SUCH-FIELD"), None);
}

#[test]
fn scaling_tables_decode_volts_and_time() {
    let settings = Settings::parse(&block_with(&[
        (offset_of("VERT-CH1-VB"), 8),   // index 8 = 1000 mV/div
        (offset_of("HORIZ-WIN-TB"), 6),  // index 6 = 200 ns/div
    ]))
    .unwrap();
    assert_eq!(settings.volts_per_div_mv(1), Some(1000));
    assert_eq!(settings.time_per_div_ns(), Some(200));
    // Sample interval is time/div over 200 samples per division.
    assert_eq!(settings.sample_interval_ns(), Some(1.0));
}

#[test]
fn out_of_range_scaling_indices_are_none() {
    let settings = Settings::parse(&block_with(&[(offset_of("VERT-CH1-VB"), 99)])).unwrap();
    assert_eq!(settings.volts_per_div_mv(1), None);
}

#[test]
fn single_captured_counts_as_stopped() {
    // The bug this guards: treating a completed single-sequence as "running" makes a
    // stop request toggle Run/Stop and actually START the scope.
    assert!(TrigState::Stop.is_stopped());
    assert!(TrigState::SingleCaptured.is_stopped());
    assert!(!TrigState::Triggered.is_stopped());
    assert!(!TrigState::Auto.is_stopped());
}

#[test]
fn trig_state_and_depth_decode_known_codes() {
    assert_eq!(TrigState::from_code(5), TrigState::SingleCaptured);
    assert_eq!(TrigState::from_code(200), TrigState::Unknown(200));

    assert_eq!(StoreDepth::from_code(6), StoreDepth::K512);
    assert_eq!(StoreDepth::K512.code(), Some(6));
    // Gapped codes are genuinely unmapped, not silently coerced.
    assert_eq!(StoreDepth::from_code(3), StoreDepth::Unknown(3));
    assert_eq!(StoreDepth::Unknown(3).code(), None);
}

#[test]
fn trigger_level_scales_by_source_channel() {
    // Source CH1, 1000 mV/div, channel centred, trigger 25/25 div above centre = 1000 mV.
    let offset = offset_of("TRIG-VPOS");
    let settings = Settings::parse(&block_with(&[
        (offset_of("TRIG-SRC"), 0),
        (offset_of("VERT-CH1-VB"), 8),
        (offset, 25),
    ]))
    .unwrap();
    assert_eq!(settings.trigger_level_mv(), Some(1000.0));
}

#[test]
fn trigger_level_is_none_for_external_sources() {
    // EXT has no volts/div to scale by, so no level in millivolts.
    let settings = Settings::parse(&block_with(&[(offset_of("TRIG-SRC"), 2)])).unwrap();
    assert_eq!(settings.trigger_level_mv(), None);
}
