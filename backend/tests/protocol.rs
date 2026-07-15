//! Integration tests for the `'S'`-frame wire format. These live outside `src/` and
//! exercise only the crate's public API (`mso5202d::protocol::{build, verify}`).

use mso5202d::protocol::{build, verify, LEADER_DATA};

#[test]
fn build_then_verify_roundtrips() {
    let payload = [0x01u8, 0x02, 0x03];
    let frame = build(&payload);
    // 0x53 | len_LE16 | payload | ck
    assert_eq!(frame[0], LEADER_DATA);
    assert_eq!(u16::from_le_bytes([frame[1], frame[2]]), payload.len() as u16 + 1);
    assert_eq!(verify(&frame).unwrap(), payload);
}

#[test]
fn verify_rejects_bad_checksum() {
    let mut frame = build(&[0x01, 0x02]);
    *frame.last_mut().unwrap() ^= 0xFF;
    assert!(verify(&frame).is_err());
}

#[test]
fn verify_rejects_bad_length() {
    let mut frame = build(&[0x01, 0x02]);
    frame[1] = frame[1].wrapping_add(1);
    assert!(verify(&frame).is_err());
}
