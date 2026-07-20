//! The operation vocabulary: one `Op` is one **semantic operation**, at the altitude a
//! user would recognise — "Turning on CH1", "Setting acquisition depth to 512K".
//!
//! Key presses, retries, menu navigation and read-back verification all happen *inside* an
//! op. They are implementation detail and never appear as separate steps.
//!
//! Because ops carry their own label, a plan is self-describing: the total step count and
//! every step's description fall out of the data before anything runs, which is what a
//! progress bar needs.

use crate::settings::{Probe, StoreDepth};

/// One semantic operation in a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Factory Default Setup — returns the scope to a known state.
    DefaultSetup,
    /// Show or hide an analog channel.
    SetChannel {
        /// Channel number, 1 or 2.
        channel: u8,
        /// Whether the channel should end up displayed.
        on: bool,
    },
    /// Set a channel's probe attenuation.
    SetProbe {
        /// Channel number, 1 or 2.
        channel: u8,
        /// Desired attenuation.
        probe: Probe,
    },
    /// Set a channel's vertical scale.
    SetVoltsPerDiv {
        /// Channel number, 1 or 2.
        channel: u8,
        /// Desired scale in millivolts per division; must be a value the scope offers.
        millivolts: u32,
    },
    /// Set the horizontal timebase.
    SetTimePerDiv {
        /// Desired scale in nanoseconds per division; must be a value the scope offers.
        nanoseconds: u64,
    },
    /// Set the trigger level, in 1/25-division units relative to screen centre.
    SetTriggerLevel {
        /// Desired `TRIG-VPOS`.
        position: i64,
    },
    /// Set the acquisition record length.
    SetDepth {
        /// Desired store depth.
        depth: StoreDepth,
    },
}

impl Op {
    /// Human-readable description, shown as the progress step label.
    pub fn label(&self) -> String {
        match self {
            Op::DefaultSetup => "Resetting to default setup".into(),
            Op::SetChannel { channel, on: true } => format!("Turning on CH{channel}"),
            Op::SetChannel { channel, on: false } => format!("Turning off CH{channel}"),
            Op::SetProbe { channel, probe } => {
                format!("Setting CH{channel} probe to {}", probe_label(*probe))
            }
            Op::SetVoltsPerDiv { channel, millivolts } => {
                format!("Setting CH{channel} to {}/div", format_volts(*millivolts))
            }
            Op::SetTimePerDiv { nanoseconds } => {
                format!("Setting timebase to {}/div", format_time(*nanoseconds))
            }
            Op::SetTriggerLevel { position } => format!("Setting trigger level to {position}"),
            Op::SetDepth { depth } => {
                format!("Setting acquisition depth to {}", depth_label(*depth))
            }
        }
    }

}

/// `1x`, `10x`, …
fn probe_label(probe: Probe) -> String {
    match probe.factor() {
        Some(factor) => format!("{factor}x"),
        None => "unknown".into(),
    }
}

/// `4K`, `512K`, …
fn depth_label(depth: StoreDepth) -> String {
    match depth {
        StoreDepth::K4 => "4K".into(),
        StoreDepth::K40 => "40K".into(),
        StoreDepth::K512 => "512K".into(),
        StoreDepth::M1 => "1M".into(),
        StoreDepth::Unknown(code) => format!("code {code}"),
    }
}

/// Millivolts as the scope displays them: `500 mV`, `1 V`, `2.5 V`.
pub fn format_volts(millivolts: u32) -> String {
    if millivolts < 1000 {
        return format!("{millivolts} mV");
    }
    let volts = millivolts as f64 / 1000.0;
    if millivolts.is_multiple_of(1000) {
        format!("{volts:.0} V")
    } else {
        format!("{volts} V")
    }
}

/// Nanoseconds as the scope displays them: `200 ns`, `2 µs`, `1 ms`, `5 s`.
pub fn format_time(nanoseconds: u64) -> String {
    const UNITS: [(u64, &str); 4] = [
        (1_000_000_000, "s"),
        (1_000_000, "ms"),
        (1_000, "µs"),
        (1, "ns"),
    ];
    for (scale, unit) in UNITS {
        if nanoseconds >= scale {
            let value = nanoseconds as f64 / scale as f64;
            return if nanoseconds.is_multiple_of(scale) {
                format!("{value:.0} {unit}")
            } else {
                format!("{value} {unit}")
            };
        }
    }
    format!("{nanoseconds} ns")
}
