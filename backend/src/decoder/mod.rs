//! Serial-protocol decoders for captured analog traces.
//!
//! A capture arrives as volts. [`common::threshold_volts`] turns a channel into a logic
//! trace, and one of the protocol decoders turns that into bytes:
//!
//! | Protocol | Channels | Framing comes from |
//! |---|---|---|
//! | [`uart`] | one line | a least-squares bit grid + frame validation |
//! | [`spi`] | clock + data (+ optional select) | chip select, else idle-clock gaps |
//! | [`i2c`] | SCL + SDA | START/STOP conditions — self-framing |
//!
//! All of it is pure logic, so it runs on a saved capture with no instrument attached.
//!
//! ```
//! use mso5202d::decoder::{common, uart, uart::UartOptions};
//!
//! # let volts: Vec<f64> = Vec::new();
//! let trace = common::threshold_volts(&volts);
//! let bytes = uart::decode(&trace, UartOptions::default());
//! # let _ = bytes;
//! ```

pub mod common;
pub mod i2c;
pub mod spi;
pub mod uart;

/// What a decoded event represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A data byte.
    Byte,
    /// An address byte (I²C).
    Address,
    /// A START condition (I²C).
    Start,
    /// A repeated START (I²C).
    RepeatedStart,
    /// A STOP condition (I²C).
    Stop,
}

impl Kind {
    /// Whether this event carries a decoded byte value, as opposed to being a bus marker.
    pub fn carries_value(self) -> bool {
        matches!(self, Kind::Byte | Kind::Address)
    }
}

/// One decoded element of a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Index of the first sample this event spans.
    pub start: usize,
    /// Index of the last sample this event spans.
    pub end: usize,
    /// The decoded byte, or `None` for a bus marker.
    pub value: Option<u8>,
    /// Whether the event checked out — framing and parity for UART, ACK for I²C.
    pub ok: bool,
    /// What the event is.
    pub kind: Kind,
}

impl Event {
    /// A short display string, matching how a scope annotates a decoded bus.
    pub fn text(&self) -> String {
        match (self.kind, self.value) {
            (Kind::Start, _) => "S".into(),
            (Kind::RepeatedStart, _) => "Sr".into(),
            (Kind::Stop, _) => "P".into(),
            (_, Some(value)) => {
                format!("{value:02X}{}", if self.ok { "" } else { "!" })
            }
            (_, None) => String::new(),
        }
    }
}

/// The byte values from a decode, ignoring bus markers.
pub fn values(events: &[Event]) -> Vec<u8> {
    events
        .iter()
        .filter(|e| e.kind.carries_value())
        .filter_map(|e| e.value)
        .collect()
}
