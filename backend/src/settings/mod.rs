//! The 213-byte settings block: field table, typed access, and the scaling tables.
//!
//! Pure logic — no I/O. [`crate::Device::read_settings`] fetches the bytes (selector
//! `0x01`) and hands them here.
//!
//! The block is the parameter list from the scope's own `/protocol.inf`, laid out back to
//! back in wire order with **no padding**. Every multi-byte field is **little-endian**;
//! the fields in [`SIGNED_FIELDS`] are two's-complement signed.
//!
//! The block is **read-only** by policy: the scope is configured through key events, never
//! through a `0x11` block write — a raw field write skips the firmware side effects a real
//! key runs (LEDs, on-screen state, acquisition reconfig, SD-card detection).

mod tables;

pub use tables::{lookup, ACQ_DEPTH_NAMES, MENU_NAMES, TB_TO_NS, VB_TO_MV};

use crate::error::{Error, Result};

/// Total size of the settings block, in bytes (`/protocol.inf` `[TOTAL]`).
pub const SETTINGS_LEN: usize = 213;

/// Position/level fields are expressed in 1/25-division units.
pub const DIV_UNIT: i64 = 25;

/// Horizontal sample density: the record is 200 samples per division.
pub const SAMPLES_PER_DIV: u64 = 200;

/// `(name, width_in_bytes)` for every field, in wire order. Widths sum to [`SETTINGS_LEN`].
pub const SETTINGS_PARAMS: &[(&str, usize)] = &[
    ("VERT-CH1-DISP", 1), ("VERT-CH1-VB", 1), ("VERT-CH1-COUP", 1), ("VERT-CH1-20MHZ", 1),
    ("VERT-CH1-FINE", 1), ("VERT-CH1-PROBE", 1), ("VERT-CH1-RPHASE", 1), ("VERT-CH1-CNT-FINE", 1),
    ("VERT-CH1-POS", 2),
    ("VERT-CH2-DISP", 1), ("VERT-CH2-VB", 1), ("VERT-CH2-COUP", 1), ("VERT-CH2-20MHZ", 1),
    ("VERT-CH2-FINE", 1), ("VERT-CH2-PROBE", 1), ("VERT-CH2-RPHASE", 1), ("VERT-CH2-CNT-FINE", 1),
    ("VERT-CH2-POS", 2),
    ("TRIG-STATE", 1), ("TRIG-TYPE", 1), ("TRIG-SRC", 1), ("TRIG-MODE", 1), ("TRIG-COUP", 1),
    ("TRIG-VPOS", 2), ("TRIG-FREQUENCY", 8),
    ("TRIG-HOLDTIME-MIN", 8), ("TRIG-HOLDTIME-MAX", 8), ("TRIG-HOLDTIME", 8),
    ("TRIG-EDGE-SLOPE", 1),
    ("TRIG-VIDEO-NEG", 1), ("TRIG-VIDEO-PAL", 1), ("TRIG-VIDEO-SYN", 1), ("TRIG-VIDEO-LINE", 2),
    ("TRIG-PULSE-NEG", 1), ("TRIG-PULSE-WHEN", 1), ("TRIG-PULSE-TIME", 8),
    ("TRIG-SLOPE-SET", 1), ("TRIG-SLOPE-WIN", 1), ("TRIG-SLOPE-WHEN", 1),
    ("TRIG-SLOPE-V1", 2), ("TRIG-SLOPE-V2", 2), ("TRIG-SLOPE-TIME", 8),
    ("TRIG-SWAP-CH1-TYPE", 1), ("TRIG-SWAP-CH1-MODE", 1), ("TRIG-SWAP-CH1-COUP", 1),
    ("TRIG-SWAP-CH1-EDGE-SLOPE", 1), ("TRIG-SWAP-CH1-VIDEO-NEG", 1), ("TRIG-SWAP-CH1-VIDEO-PAL", 1),
    ("TRIG-SWAP-CH1-VIDEO-SYN", 1), ("TRIG-SWAP-CH1-VIDEO-LINE", 2),
    ("TRIG-SWAP-CH1-PULSE-NEG", 1), ("TRIG-SWAP-CH1-PULSE-WHEN", 1), ("TRIG-SWAP-CH1-PULSE-TIME", 8),
    ("TRIG-SWAP-CH1-OVERTIME-NEG", 1), ("TRIG-SWAP-CH1-OVERTIME-TIME", 8),
    ("TRIG-SWAP-CH2-TYPE", 1), ("TRIG-SWAP-CH2-MODE", 1), ("TRIG-SWAP-CH2-COUP", 1),
    ("TRIG-SWAP-CH2-EDGE-SLOPE", 1), ("TRIG-SWAP-CH2-VIDEO-NEG", 1), ("TRIG-SWAP-CH2-VIDEO-PAL", 1),
    ("TRIG-SWAP-CH2-VIDEO-SYN", 1), ("TRIG-SWAP-CH2-VIDEO-LINE", 2),
    ("TRIG-SWAP-CH2-PULSE-NEG", 1), ("TRIG-SWAP-CH2-PULSE-WHEN", 1), ("TRIG-SWAP-CH2-PULSE-TIME", 8),
    ("TRIG-SWAP-CH2-OVERTIME-NEG", 1), ("TRIG-SWAP-CH2-OVERTIME-TIME", 8),
    ("TRIG-OVERTIME-NEG", 1), ("TRIG-OVERTIME-TIME", 8),
    ("HORIZ-TB", 1), ("HORIZ-WIN-TB", 1), ("HORIZ-WIN-STATE", 1), ("HORIZ-TRIGTIME", 8),
    ("MATH-DISP", 1), ("MATH-MODE", 1), ("MATH-FFT-SRC", 1), ("MATH-FFT-WIN", 1),
    ("MATH-FFT-FACTOR", 1), ("MATH-FFT-DB", 1),
    ("DISPLAY-MODE", 1), ("DISPLAY-PERSIST", 1), ("DISPLAY-FORMAT", 1), ("DISPLAY-CONTRAST", 1),
    ("DISPLAY-MAXCONTRAST", 1), ("DISPLAY-GRID-KIND", 1), ("DISPLAY-GRID-BRIGHT", 1),
    ("DISPLAY-MAXGRID-BRIGHT", 1),
    ("ACQURIE-MODE", 1), ("ACQURIE-AVG-CNT", 1), ("ACQURIE-TYPE", 1), ("ACQURIE-STORE-DEPTH", 1),
    ("MEASURE-ITEM1-SRC", 1), ("MEASURE-ITEM1", 1), ("MEASURE-ITEM2-SRC", 1), ("MEASURE-ITEM2", 1),
    ("MEASURE-ITEM3-SRC", 1), ("MEASURE-ITEM3", 1), ("MEASURE-ITEM4-SRC", 1), ("MEASURE-ITEM4", 1),
    ("MEASURE-ITEM5-SRC", 1), ("MEASURE-ITEM5", 1), ("MEASURE-ITEM6-SRC", 1), ("MEASURE-ITEM6", 1),
    ("MEASURE-ITEM7-SRC", 1), ("MEASURE-ITEM7", 1), ("MEASURE-ITEM8-SRC", 1), ("MEASURE-ITEM8", 1),
    ("CONTROL-TYPE", 1), ("CONTROL-MENUID", 1), ("CONTROL-DISP-MENU", 1),
    ("LA-SWI", 1), ("LA-CHANNEL-STATE", 2), ("LA-CURRENT-CHANNEL", 1),
    ("LA-D7-D0-THRESHOLD-TYPE", 1), ("LA-D15-D8-THRESHOLD-TYPE", 1),
    ("LA-D7-D0-USER-THRESHOLD-VOLT", 2), ("LA-D15-D8-USER-THRESHOLD-VOLT", 2),
];

/// Fields stored as two's-complement signed integers. Positions and trigger levels go
/// negative (below screen centre), as does the horizontal delay (post-trigger).
pub const SIGNED_FIELDS: &[&str] = &[
    "VERT-CH1-POS", "VERT-CH2-POS", "TRIG-VPOS", "TRIG-SLOPE-V1", "TRIG-SLOPE-V2",
    "HORIZ-TRIGTIME", "LA-D7-D0-USER-THRESHOLD-VOLT", "LA-D15-D8-USER-THRESHOLD-VOLT",
];

/// Acquisition/trigger state (`TRIG-STATE`).
///
/// `SingleCaptured` is the subtle one: a completed single-sequence leaves the scope
/// **stopped** with the Run/Stop button lit red. Treating it as "running" and pressing
/// Run/Stop would *start* the scope. Use [`TrigState::is_stopped`] rather than comparing
/// against `Stop` alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrigState {
    /// Stopped.
    Stop,
    /// Normal mode, waiting for a trigger that has not occurred.
    Wait,
    /// Auto mode, free-running while searching for a trigger.
    Auto,
    /// Triggered and running.
    Triggered,
    /// Scan/roll mode.
    Scan,
    /// Single sequence complete — captured and **stopped**.
    SingleCaptured,
    /// Re-arming.
    Arming,
    /// A code we have not mapped.
    Unknown(u8),
}

impl TrigState {
    /// Decode the raw `TRIG-STATE` byte.
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Stop,
            1 => Self::Wait,
            2 => Self::Auto,
            3 => Self::Triggered,
            4 => Self::Scan,
            5 => Self::SingleCaptured,
            6 => Self::Arming,
            other => Self::Unknown(other),
        }
    }

    /// Whether the acquisition is halted. **Both** `Stop` and `SingleCaptured` count.
    pub fn is_stopped(self) -> bool {
        matches!(self, Self::Stop | Self::SingleCaptured)
    }
}

/// Acquisition record length (`ACQURIE-STORE-DEPTH`). Codes are gapped because depths that
/// are greyed out in the current mode still occupy enum slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreDepth {
    /// 4 K — the screen-sized record.
    K4,
    /// 40 K.
    K40,
    /// 512 K.
    K512,
    /// 1 M — single-channel only.
    M1,
    /// A code we have not mapped.
    Unknown(u8),
}

impl StoreDepth {
    /// Decode the raw `ACQURIE-STORE-DEPTH` byte.
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::K4,
            4 => Self::K40,
            6 => Self::K512,
            7 => Self::M1,
            other => Self::Unknown(other),
        }
    }

    /// The wire code for this depth, if known.
    pub fn code(self) -> Option<u8> {
        Some(match self {
            Self::K4 => 0,
            Self::K40 => 4,
            Self::K512 => 6,
            Self::M1 => 7,
            Self::Unknown(_) => return None,
        })
    }
}

/// Probe attenuation (`VERT-CHx-PROBE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// 1×
    X1,
    /// 10×
    X10,
    /// 100×
    X100,
    /// 1000×
    X1000,
    /// A code we have not mapped.
    Unknown(u8),
}

impl Probe {
    /// Decode the raw `VERT-CHx-PROBE` byte.
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::X1,
            1 => Self::X10,
            2 => Self::X100,
            3 => Self::X1000,
            other => Self::Unknown(other),
        }
    }

    /// The attenuation factor, if known.
    pub fn factor(self) -> Option<u32> {
        Some(match self {
            Self::X1 => 1,
            Self::X10 => 10,
            Self::X100 => 100,
            Self::X1000 => 1000,
            Self::Unknown(_) => return None,
        })
    }
}

/// Channel input coupling (`VERT-CHx-COUP`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coupling {
    /// DC coupled.
    Dc,
    /// AC coupled.
    Ac,
    /// Input grounded.
    Ground,
    /// A code we have not mapped.
    Unknown(u8),
}

impl Coupling {
    /// The `VERT-CHx-COUP` code.
    pub fn code(self) -> u8 {
        match self {
            Self::Dc => 0,
            Self::Ac => 1,
            Self::Ground => 2,
            Self::Unknown(code) => code,
        }
    }

    /// Decode the raw `VERT-CHx-COUP` byte.
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Dc,
            1 => Self::Ac,
            2 => Self::Ground,
            other => Self::Unknown(other),
        }
    }
}

/// A decoded snapshot of the scope's settings block.
///
/// Holds the raw bytes and reads fields on demand. Named fields are looked up through
/// [`SETTINGS_PARAMS`]; the accessor methods cover the ones control logic needs, and
/// [`Settings::field`] / [`Settings::field_signed`] reach anything else by name.
#[derive(Debug, Clone)]
pub struct Settings {
    raw: [u8; SETTINGS_LEN],
}

impl Settings {
    /// Parse a settings payload.
    ///
    /// Accepts either the raw 213 parameter bytes or the full reply payload with its
    /// leading `0x81` selector echo (214 bytes), which is what the wire delivers.
    pub fn parse(payload: &[u8]) -> Result<Self> {
        let body = match payload.len() {
            SETTINGS_LEN => payload,
            len if len == SETTINGS_LEN + 1 && payload[0] == 0x81 => &payload[1..],
            len => {
                return Err(Error::Framing(format!(
                    "not a settings payload: {len} bytes (want {SETTINGS_LEN} or {} with 0x81 echo)",
                    SETTINGS_LEN + 1
                )))
            }
        };
        let mut raw = [0u8; SETTINGS_LEN];
        raw.copy_from_slice(body);
        Ok(Self { raw })
    }

    /// The undecoded parameter bytes.
    pub fn raw(&self) -> &[u8; SETTINGS_LEN] {
        &self.raw
    }

    /// Byte offset and width of a named field, or `None` if the name is unknown.
    fn locate(name: &str) -> Option<(usize, usize)> {
        let mut offset = 0;
        for &(field, width) in SETTINGS_PARAMS {
            if field == name {
                return Some((offset, width));
            }
            offset += width;
        }
        None
    }

    /// Read a named field as an unsigned little-endian integer.
    pub fn field(&self, name: &str) -> Option<u64> {
        let (offset, width) = Self::locate(name)?;
        let mut value = 0u64;
        for (i, &byte) in self.raw[offset..offset + width].iter().enumerate() {
            value |= (byte as u64) << (8 * i);
        }
        Some(value)
    }

    /// Read a named field as a signed little-endian integer, sign-extending from its width.
    pub fn field_signed(&self, name: &str) -> Option<i64> {
        let (_, width) = Self::locate(name)?;
        let raw = self.field(name)?;
        let bits = 8 * width as u32;
        Some(if bits < 64 && raw & (1 << (bits - 1)) != 0 {
            raw as i64 - (1i64 << bits)
        } else {
            raw as i64
        })
    }

    /// Read a named field, applying the sign convention from [`SIGNED_FIELDS`].
    pub fn field_auto(&self, name: &str) -> Option<i64> {
        if SIGNED_FIELDS.contains(&name) {
            self.field_signed(name)
        } else {
            self.field(name).map(|v| v as i64)
        }
    }

    // --- common accessors ------------------------------------------------------

    /// Current acquisition/trigger state.
    pub fn trig_state(&self) -> TrigState {
        TrigState::from_code(self.field("TRIG-STATE").unwrap_or(0) as u8)
    }

    /// Which on-screen menu is open (`CONTROL-MENUID`). Menu ids are named in
    /// [`MENU_NAMES`].
    pub fn menu_id(&self) -> u8 {
        self.field("CONTROL-MENUID").unwrap_or(0) as u8
    }

    /// Human-readable name of the open menu, or `None` for an id we have not mapped.
    pub fn menu_name(&self) -> Option<&'static str> {
        lookup(MENU_NAMES, self.menu_id())
    }

    /// Whether channel `ch` (1 or 2) is displayed.
    pub fn channel_shown(&self, ch: u8) -> bool {
        self.field(&format!("VERT-CH{ch}-DISP")) == Some(1)
    }

    /// Channel `ch` volts/division, in millivolts. `None` if the index is unmapped.
    ///
    /// Note the firmware quirk that 10 V/div also stores index 0 (wraps mod 11), so this
    /// reports 2 mV/div for both — the ambiguity is inherent to the field.
    /// **At the probe tip**, in millivolts per division.
    ///
    /// [`Settings::volts_per_div_mv`] reports the ladder position, which the instrument
    /// keeps at its 1× value whatever the probe is set to — measured: switching CH1 through
    /// 1× / 10× / 100× / 1000× left `VERT-CH1-VB` and the millivolt figure untouched while
    /// the scope's own display went 100 mV → 1.00 V → 10.0 V → 100 V per division. So the
    /// real scale at the probe tip is the ladder value multiplied by the attenuation, and
    /// anything reporting volts to a user has to say *this*, not the ladder.
    pub fn input_volts_per_div_mv(&self, ch: u8) -> Option<u32> {
        let factor = self.probe(ch).factor()?;
        Some(self.volts_per_div_mv(ch)? * factor)
    }

    /// The vertical scale's position on the scope's **ladder**, in millivolts per division.
    ///
    /// This is what the volts/division knob steps through and what the settings block holds,
    /// so it is the right value to converge on — but it ignores the probe, so it is *not*
    /// what the signal measures. Use [`Settings::input_volts_per_div_mv`] for that.
    pub fn volts_per_div_mv(&self, ch: u8) -> Option<u32> {
        let index = self.field(&format!("VERT-CH{ch}-VB"))? as usize;
        VB_TO_MV.get(index).copied()
    }

    /// Channel `ch` probe attenuation.
    pub fn probe(&self, ch: u8) -> Probe {
        Probe::from_code(self.field(&format!("VERT-CH{ch}-PROBE")).unwrap_or(0) as u8)
    }

    /// Channel `ch` input coupling.
    pub fn coupling(&self, ch: u8) -> Coupling {
        Coupling::from_code(self.field(&format!("VERT-CH{ch}-COUP")).unwrap_or(0) as u8)
    }

    /// Channel `ch` vertical position, in 1/25-division units (signed).
    pub fn channel_position(&self, ch: u8) -> i64 {
        self.field_signed(&format!("VERT-CH{ch}-POS")).unwrap_or(0)
    }

    /// Displayed time/division in nanoseconds (the `HORIZ-WIN-TB` knob position).
    pub fn time_per_div_ns(&self) -> Option<u64> {
        TB_TO_NS.get(self.field("HORIZ-WIN-TB")? as usize).copied()
    }

    /// Acquisition time/division in nanoseconds (`HORIZ-TB`, clamped at 200 ns/div — the
    /// faster settings are zoom/interpolation).
    pub fn acquisition_time_per_div_ns(&self) -> Option<u64> {
        TB_TO_NS.get(self.field("HORIZ-TB")? as usize).copied()
    }

    /// Sample interval of the **screen buffer**, in nanoseconds (time/div ÷ 200).
    ///
    /// Screen record only, and not valid past the ADC ceiling at the fastest timebases. A
    /// deep capture carries its own true interval in the exported CSV header.
    pub fn sample_interval_ns(&self) -> Option<f64> {
        self.time_per_div_ns()
            .map(|tdiv| tdiv as f64 / SAMPLES_PER_DIV as f64)
    }

    /// Acquisition record length.
    pub fn store_depth(&self) -> StoreDepth {
        StoreDepth::from_code(self.field("ACQURIE-STORE-DEPTH").unwrap_or(0) as u8)
    }

    /// Trigger level in 1/25-division units, relative to screen centre (signed).
    pub fn trigger_position(&self) -> i64 {
        self.field_signed("TRIG-VPOS").unwrap_or(0)
    }

    /// Trigger level in millivolts, or `None` when the source is not CH1/CH2 (EXT and
    /// AC-line have no volts/div to scale by) or the V/div index is unmapped.
    ///
    /// `level = (TRIG-VPOS − channel_position) × volts_per_div ÷ 25`.
    pub fn trigger_level_mv(&self) -> Option<f64> {
        let source_ch = match self.field("TRIG-SRC")? {
            0 => 1,
            1 => 2,
            _ => return None,
        };
        // At the probe tip: a level is a voltage on the signal, not a position on the
        // ladder, so the attenuation belongs in it.
        let volts_per_div = self.input_volts_per_div_mv(source_ch)? as f64;
        let offset = (self.trigger_position() - self.channel_position(source_ch)) as f64;
        Some(offset * volts_per_div / DIV_UNIT as f64)
    }

    /// Whether the logic-analyzer pod is enabled.
    pub fn la_enabled(&self) -> bool {
        self.field("LA-SWI") == Some(1)
    }

    /// Logic-analyzer channel enable mask — bit `N` is D`N`.
    pub fn la_channel_mask(&self) -> u16 {
        self.field("LA-CHANNEL-STATE").unwrap_or(0) as u16
    }
}
