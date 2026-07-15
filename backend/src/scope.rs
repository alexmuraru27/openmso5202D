//! [`Scope`] — a thin high-level facade over [`Transport`].
//!
//! Only the handful of operations needed to exercise the transport live here for now
//! (settings poll, file read, front-panel key events). Waveform acquisition, settings
//! decode, framebuffer grab, and the deep-capture state machine are future layers that
//! will build on the same [`Transport::transact`] primitive.

use std::time::Duration;

use crate::error::Result;
use crate::protocol::subtype;
use crate::usb::Transport;

/// High-level handle to the oscilloscope.
pub struct Scope {
    transport: Transport,
}

impl Scope {
    /// Connect to the scope, performing a USB reset first (the safe default for a fresh
    /// session — clears a wedged link).
    pub fn connect() -> Result<Self> {
        Ok(Self {
            transport: Transport::open(true)?,
        })
    }

    /// Connect **without** a USB reset. Required for anything that drives the scope's SD
    /// card (deep capture / Save-to-CSV): a reset disturbs the scope's own USB host
    /// controller and makes the card "undetected".
    pub fn connect_no_reset() -> Result<Self> {
        Ok(Self {
            transport: Transport::open(false)?,
        })
    }

    /// Borrow the underlying transport for raw transactions.
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// Re-establish the connection after an unrecoverable USB error.
    pub fn reconnect(&mut self, reset: bool) -> Result<()> {
        self.transport.reconnect(reset)
    }

    /// Poll selector `0x01` → the 213-byte settings block (the payload after the `0x81`
    /// selector echo). Decoding is a future layer; this returns the raw bytes.
    pub fn read_settings(&self) -> Result<Vec<u8>> {
        let payload = self.transport.transact(&[0x01])?;
        // payload = [0x81, settings…]; strip the selector echo.
        Ok(payload.get(1..).unwrap_or(&[]).to_vec())
    }

    /// Send a front-panel key event (selector `0x13`): `keyid` from `/keyprotocol.inf`
    /// (0-indexed), `count` press count. Returns the key-ack payload.
    pub fn send_key(&self, keyid: u8, count: u8) -> Result<Vec<u8>> {
        self.transport.transact(&[0x13, keyid, count])
    }

    /// Read a file from the scope's embedded Linux over USB (selector `0x10`).
    ///
    /// The reply is multi-frame: any number of DATA frames (subtype `0x01`) terminated by
    /// an END frame (subtype `0x02`). A single frame caps at 64 KB, so large files span
    /// many frames — loop until the end-marker.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let timeout = Duration::from_millis(4000);
        let mut req = vec![0x10, 0x00];
        req.extend_from_slice(path.as_bytes());

        let mut frame = self.transport.transact_with(&req, timeout, 2)?;
        let mut data = Vec::new();
        loop {
            match frame.get(1).copied() {
                Some(subtype::DATA) => data.extend_from_slice(&frame[2..]),
                Some(subtype::END) => break,
                _ => break,
            }
            match self.transport.recv(timeout) {
                Ok(next) => frame = next,
                Err(_) => break,
            }
        }
        Ok(data)
    }
}
