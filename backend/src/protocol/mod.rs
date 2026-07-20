//! The MSO5202D `'S'`-framed wire protocol: constants + frame [`build`]/[`verify`].
//!
//! Frame layout (little-endian), see `docs/MSO5202D-protocol.md` §2:
//!
//! ```text
//!   0x53 | len_LE16 | payload… | checksum
//!          ^ len = (total frame length) - 3 = payload_len + 1
//!                                      checksum = (sum of all preceding bytes) & 0xFF ^
//! ```
//!
//! A response payload echoes the request `selector | 0x80` as its first byte, optionally
//! followed by a subtype byte (`0x00` size / `0x01` data / `0x02` end-marker) for
//! multi-frame replies.

use crate::error::{Error, Result};

/// USB vendor id of the scope.
pub const VID: u16 = 0x049F;
/// USB product id of the scope.
pub const PID: u16 = 0x505A;

/// Bulk OUT endpoint (host → scope).
pub const EP_OUT: u8 = 0x02;
/// Bulk IN endpoint (scope → host).
pub const EP_IN: u8 = 0x81;

/// The interface we claim (the device exposes exactly one).
pub const INTERFACE: u8 = 0;

/// Frame leader byte for the data/protocol channel (`'S'`).
pub const LEADER_DATA: u8 = 0x53;
/// Frame leader byte for the command/shell channel (`'C'`). Same framing, separate
/// selector map. Read-only use only — it runs commands as root on the scope.
pub const LEADER_CMD: u8 = 0x43;

/// Request selectors — the first payload byte, which the reply echoes as `selector | 0x80`.
pub mod selector {
    /// Data channel (`0x53`): read the 213-byte settings block. Echo `0x81`.
    pub const SETTINGS: u8 = 0x01;
    /// Data channel: read a file off the scope's filesystem. Echo `0x90`.
    pub const FILE_READ: u8 = 0x10;
    /// Data channel: inject a front-panel key event, `13 <keyid> <state>`. Echo `0x93`.
    pub const KEY: u8 = 0x13;
    /// Data channel: grab the rendered screen framebuffer. Echo `0xa0`.
    pub const FRAMEBUFFER: u8 = 0x20;
    /// Command channel (`0x43`): run a shell command as root. Echo `0x91`.
    pub const SHELL: u8 = 0x11;
}

/// Response multi-frame subtype bytes (second payload byte after the selector echo).
pub mod subtype {
    /// Size frame — announces the length of the data to follow.
    pub const SIZE: u8 = 0x00;
    /// Data frame — carries a chunk of the payload.
    pub const DATA: u8 = 0x01;
    /// End-marker frame — terminates a multi-frame reply; must be consumed.
    pub const END: u8 = 0x02;
    /// No-data marker (acquire with nothing to return).
    pub const NODATA: u8 = 0x03;
}

/// Build a full data-channel (`0x53`) frame around `payload`.
///
/// `frame = 0x53 | len_LE16(payload_len + 1) | payload | checksum`, where the checksum is
/// the low byte of the sum of every preceding byte.
pub fn build(payload: &[u8]) -> Vec<u8> {
    build_with(LEADER_DATA, payload)
}

/// Build a frame with an explicit leader byte — [`LEADER_DATA`] for the protocol channel
/// or [`LEADER_CMD`] for the shell/command channel. Both use identical framing.
pub fn build_with(leader: u8, payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() as u16) + 1;
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.push(leader);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(payload);
    let checksum = frame.iter().fold(0u32, |a, &b| a + b as u32) as u8;
    frame.push(checksum);
    frame
}

/// Extract a frame's payload using only its length field — no leader or checksum check.
///
/// Used for the command channel (`0x43`), whose replies do not carry a checksum we can
/// rely on. Prefer [`verify`] for data-channel frames, which validates both.
pub fn payload_of(frame: &[u8]) -> Result<&[u8]> {
    if frame.len() < 5 {
        return Err(Error::Framing(format!("frame too short: {} bytes", frame.len())));
    }
    let declared = u16::from_le_bytes([frame[1], frame[2]]) as usize;
    if declared != frame.len() - 3 {
        return Err(Error::Framing(format!(
            "length field {declared} != actual {}",
            frame.len() - 3
        )));
    }
    Ok(&frame[3..frame.len() - 1])
}

/// Validate a complete frame and return its payload (`selector_echo | subtype | data…`).
///
/// Checks the leader byte, that the length field matches the actual length, and the
/// trailing checksum.
pub fn verify(frame: &[u8]) -> Result<&[u8]> {
    if frame.len() < 5 {
        return Err(Error::Framing(format!("frame too short: {} bytes", frame.len())));
    }
    if frame[0] != LEADER_DATA {
        return Err(Error::Framing(format!("bad leader 0x{:02x}", frame[0])));
    }
    let declared = u16::from_le_bytes([frame[1], frame[2]]) as usize;
    if declared != frame.len() - 3 {
        return Err(Error::Framing(format!(
            "length field {declared} != actual {}",
            frame.len() - 3
        )));
    }
    let sum = frame[..frame.len() - 1].iter().fold(0u32, |a, &b| a + b as u32) as u8;
    if sum != frame[frame.len() - 1] {
        return Err(Error::Framing("checksum mismatch".into()));
    }
    Ok(&frame[3..frame.len() - 1])
}

/// The declared total length of a frame given its first 3 bytes (leader + len_LE16).
/// Returns `None` if fewer than 3 bytes are available yet.
pub(crate) fn frame_total_len(head: &[u8]) -> Option<usize> {
    if head.len() < 3 {
        return None;
    }
    Some(u16::from_le_bytes([head[1], head[2]]) as usize + 3)
}
