//! Error type for the driver. Everything fallible returns [`Result`].

use std::time::Duration;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Failures that can occur talking to the scope.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The scope (VID:PID `049f:505a`) was not found on the USB bus.
    #[error("MSO5202D (049f:505a) not found — is it plugged in and powered?")]
    NotFound,

    /// A framing/checksum/length error while parsing a response frame.
    #[error("protocol framing error: {0}")]
    Framing(String),

    /// The device did not answer a transaction within the timeout.
    #[error("no response after {0:?}")]
    Timeout(Duration),

    /// A lower-level libusb error (I/O, access denied, pipe stall, …).
    #[error("usb error: {0}")]
    Usb(#[from] rusb::Error),
}
