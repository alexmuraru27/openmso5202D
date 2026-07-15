//! `mso5202d` — reverse-engineered USB driver for the Hantek MSO5202D
//! oscilloscope (USB `049f:505a`).
//!
//! This crate is the Rust port of the Python `Scope` class (see `scripts/mso5202d.py`
//! and `docs/MSO5202D-protocol.md`). It is layered so higher-level features build on
//! a small, well-understood foundation:
//!
//! - [`protocol`] — the `'S'`-framed wire format ([`protocol::build`] / [`protocol::verify`])
//!   and the wire constants (VID/PID, endpoints, selectors).
//! - [`usb::Transport`] — the low-level driver: connect / reconnect / reset, interface
//!   binding, and the reader-thread-before-write [`Transport::transact`] dance the device
//!   requires.
//! - [`Scope`] — a thin high-level facade over the transport (settings poll, file read,
//!   key events) that later GUI/decoding layers consume.
//!
//! Everything that touches hardware needs the scope plugged in and either the udev rule
//! from `70-mso5202d.rules` installed or the process running as root.

pub mod error;
pub mod protocol;
pub mod scope;
pub mod usb;

pub use error::{Error, Result};
pub use protocol::{PID, VID};
pub use scope::Scope;
pub use usb::Transport;
