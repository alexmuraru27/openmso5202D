//! The operation vocabulary: one `Op` is one **semantic operation**, at the altitude a
//! user would recognise — "Turning on CH1", "Setting acquisition depth to 512K".
//!
//! Key presses, retries, menu navigation and read-back verification all happen *inside* an
//! op. They are implementation detail and never appear as separate steps.
//!
//! Because ops carry their own label, a plan is self-describing: the total step count and
//! every step's description fall out of the data before anything runs, which is what a
//! progress bar needs.

use crate::control::csv::CsvSource;
use crate::control::trigger::{Adjustable, TriggerSetup};
use crate::settings::{Coupling, Probe, StoreDepth};

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
    /// Set a channel's input coupling.
    SetCoupling {
        /// Channel number, 1 or 2.
        channel: u8,
        /// Desired coupling.
        coupling: Coupling,
    },
    /// Limit a channel's bandwidth to 20 MHz, or leave it full.
    SetBandwidthLimit {
        /// Channel number, 1 or 2.
        channel: u8,
        /// Whether the 20 MHz limit should be on.
        limited: bool,
    },
    /// Invert a channel.
    SetInvert {
        /// Channel number, 1 or 2.
        channel: u8,
        /// Whether the trace should be inverted.
        inverted: bool,
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
    /// Configure what the trigger looks for: type, source, slope, mode, coupling and the
    /// per-type options.
    ///
    /// One op rather than one per field: an operator thinks "set the trigger", and the
    /// fields are not independently meaningful — the type decides which of the others even
    /// exist. It reports sub-progress as each field lands, because a full change is several
    /// seconds of key presses and a bar that does not move looks like a hang.
    SetTrigger {
        /// The configuration to apply.
        setup: TriggerSetup,
    },
    /// Drive one of the trigger's knob-only values to a target.
    ///
    /// These have no keyed entry — pulse width, slope thresholds, overtime, video line — so
    /// the only way to set one is to walk the multipurpose knob to it. Doing that once here
    /// is why the panel can offer them as ordinary settings instead of a live control that
    /// costs a USB round trip per click.
    SetTriggerValue {
        /// Which value.
        what: Adjustable,
        /// Where it should end up, in the units its field is stored in.
        target: i64,
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

    /// Arm a single sequence and return **without** waiting for it to fire.
    ///
    /// Splitting the arm from the wait is what allows an external stimulus to be released
    /// while the scope is armed: arm, start the signal, then [`Op::WaitCaptured`]. Doing
    /// both in one step would either miss the stimulus or force a trigger before it
    /// arrived.
    ArmSingle,

    /// Wait for an armed single sequence to capture and stop.
    ///
    /// A real trigger is strongly preferred, but if no edge arrives within a short grace the
    /// trigger is **forced once** so a scope whose level sits off the signal still yields a
    /// record instead of timing out — the same nudge `_trigger_record` makes. Only a scope
    /// that never stops even after forcing is a failure.
    WaitCaptured {
        /// How long to wait for a trigger, in seconds.
        timeout_s: u64,
    },

    /// Export the captured record for one source to the memory card as a CSV.
    ///
    /// This is how a record longer than the screen buffer is retrieved: deep records are
    /// not exposed over USB at any depth, so the scope writes them to the card and they are
    /// read back with [`Op::Download`].
    SaveCsv {
        /// Which trace to export.
        source: CsvSource,
    },

    /// Read back the CSV that [`Op::SaveCsv`] wrote for `source`.
    Download {
        /// Which trace's export to fetch.
        source: CsvSource,
    },

    /// Delete **every** exported waveform CSV from the memory card.
    ///
    /// Irreversible, and it clears the whole card of `WaveData*.csv` — not merely the
    /// files this plan created, because the front-panel delete key acts on whatever the
    /// file list has selected and cannot be aimed at specific files.
    ///
    /// Deletion goes through the front panel, never a shell `rm`: the shell is root on the
    /// instrument with no undo, and the delete key is the path the firmware expects.
    ClearCard,
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
            Op::SetCoupling { channel, coupling } => {
                format!("Setting CH{channel} coupling to {}", coupling_label(*coupling))
            }
            Op::SetBandwidthLimit { channel, limited: true } => {
                format!("Limiting CH{channel} to 20 MHz")
            }
            Op::SetBandwidthLimit { channel, limited: false } => {
                format!("Removing CH{channel} bandwidth limit")
            }
            Op::SetInvert { channel, inverted: true } => format!("Inverting CH{channel}"),
            Op::SetInvert { channel, inverted: false } => format!("Un-inverting CH{channel}"),
            Op::SetVoltsPerDiv { channel, millivolts } => {
                format!("Setting CH{channel} to {}/div", format_volts(*millivolts))
            }
            Op::SetTimePerDiv { nanoseconds } => {
                format!("Setting timebase to {}/div", format_time(*nanoseconds))
            }
            Op::SetTrigger { setup } => format!(
                "Setting the {} trigger on {}",
                setup.kind.name(),
                setup.source.name()
            ),
            Op::SetTriggerValue { what, .. } => format!("Setting {}", what.label().to_lowercase()),
            Op::SetTriggerLevel { position } => format!("Setting trigger level to {position}"),
            Op::SetDepth { depth } => {
                format!("Setting acquisition depth to {}", depth_label(*depth))
            }
            Op::ArmSingle => "Arming a single sequence".into(),
            Op::WaitCaptured { .. } => "Waiting for the trigger".into(),
            Op::SaveCsv { source } => format!("Saving {} to card", source.name()),
            Op::Download { source } => format!("Downloading {} record", source.name()),
            Op::ClearCard => "Clearing exported CSVs from card".into(),
        }
    }

}

/// `DC`, `AC`, `GND`.
fn coupling_label(coupling: Coupling) -> &'static str {
    match coupling {
        Coupling::Dc => "DC",
        Coupling::Ac => "AC",
        Coupling::Ground => "GND",
        Coupling::Unknown(_) => "an unknown setting",
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
