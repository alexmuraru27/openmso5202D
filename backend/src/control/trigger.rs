//! Trigger configuration through the front-panel trigger menu.
//!
//! Every value here is set the same way the operator would: navigate to the page that owns
//! it, then press its softkey until the settings block reads the wanted value. Nothing is
//! written to the settings block directly — see the module docs of [`crate::settings`] for
//! why a raw block write is the wrong tool.
//!
//! The softkey map this drives was discovered on hardware and is recorded in
//! `docs/MSO5202D-statemachines.md` §3.9. Two of its findings shape this module:
//!
//! - **The trigger key cannot navigate.** It opens whichever page matches the *current*
//!   trigger type, so after changing the type it lands somewhere else entirely. Pages are
//!   reached by cycling the type softkey until `TRIG-TYPE` reads the wanted code, which is
//!   what [`goto_type`] does.
//! - **The selectable set depends on the type.** The source softkey cycles a ring of five
//!   on Edge but only two on Overtime, because the scope refuses the rest. Asking for a
//!   source the current type does not offer is a real error, not a lost press, and it is
//!   reported as one.

use std::thread::sleep;
use std::time::Duration;

use tracing::debug;

use super::converge::{self, MENU_SETTLE, SETTLE};
use crate::device::{Device, Key, Knob, Turn};
use crate::error::{Error, Result};
use crate::settings::Settings;

/// What the trigger looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerType {
    /// A rising or falling edge.
    Edge,
    /// Video sync pulses.
    Video,
    /// A pulse of a given width.
    Pulse,
    /// An edge with a given transition time between two thresholds.
    Slope,
    /// A signal that stays at a level longer than a given time.
    Overtime,
    /// Alternating: a separate trigger per channel, taken in turn.
    Alter,
}

impl TriggerType {
    /// The `TRIG-TYPE` code.
    pub fn code(self) -> u8 {
        match self {
            Self::Edge => 0,
            Self::Video => 1,
            Self::Pulse => 2,
            Self::Slope => 3,
            Self::Overtime => 4,
            Self::Alter => 5,
        }
    }

    /// The menu ids this type's first page can carry.
    ///
    /// Edge has two. `5` is the page the type softkey lands on; `11` is the *trigger base*
    /// page, which the scope also shows for an Edge trigger depending on how the menu was
    /// reached. Treating only one of them as valid rejects a perfectly correct state — the
    /// error this fixes was "Edge trigger selected but the scope is showing menu 11, not 5",
    /// raised when the type was already Edge so no softkey was pressed and the page never
    /// changed.
    fn menus(self) -> &'static [u8] {
        match self {
            Self::Edge => &[5, 11],
            Self::Video => &[8],
            Self::Pulse => &[6],
            Self::Slope => &[22],
            Self::Overtime => &[38],
            Self::Alter => &[24],
        }
    }

    /// The menu id of its second page, for the types that have one.
    fn second_page(self) -> Option<u8> {
        match self {
            Self::Pulse => Some(7),
            Self::Slope => Some(23),
            Self::Overtime => Some(39),
            Self::Edge | Self::Video | Self::Alter => None,
        }
    }

    /// Whether the front-panel trigger **level** knob does anything in this type.
    ///
    /// It does not in Slope, which compares against two thresholds of its own rather than a
    /// single level — measured on hardware: three level-knob presses moved `TRIG-VPOS` by 3
    /// in Edge, Video, Pulse and Overtime, and by **0** in Slope, on both menu pages. A
    /// driver that converges on the level regardless spins against an unmoving value and
    /// then reports an end stop, which is what made setting a Slope trigger fail.
    pub fn has_level(self) -> bool {
        !matches!(self, Self::Slope)
    }

    /// The continuous parameters this type offers, in the order a panel lists them.
    pub fn adjustables(self) -> &'static [Adjustable] {
        match self {
            Self::Pulse => &[Adjustable::PulseWidth],
            Self::Slope => &[
                Adjustable::SlopeV1,
                Adjustable::SlopeV2,
                Adjustable::SlopeTime,
            ],
            Self::Overtime => &[Adjustable::OvertimeTime],
            Self::Edge | Self::Video | Self::Alter => &[],
        }
    }

    /// How many sources this type offers — the scope refuses the rest.
    fn source_ring(self) -> u32 {
        match self {
            Self::Edge => 5,
            Self::Video | Self::Pulse | Self::Slope => 4,
            Self::Overtime => 2,
            // Alter drives both channels; the source selector does not apply.
            Self::Alter => 2,
        }
    }

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Edge => "Edge",
            Self::Video => "Video",
            Self::Pulse => "Pulse",
            Self::Slope => "Slope",
            Self::Overtime => "Overtime",
            Self::Alter => "Alter",
        }
    }
}

/// A sub-type an Alter channel can use. Slope and Alter itself are not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlterType {
    /// Rising or falling edge.
    Edge,
    /// Video sync pulses.
    Video,
    /// A pulse of a given width.
    Pulse,
    /// A level held longer than a given time.
    Overtime,
}

impl AlterType {
    /// The `TRIG-SWAP-CHx-TYPE` code.
    pub fn code(self) -> u8 {
        match self {
            Self::Edge => 0,
            Self::Video => 1,
            Self::Pulse => 2,
            Self::Overtime => 3,
        }
    }

    /// The menu id this sub-type shows for `channel`.
    ///
    /// The ids and the codes are in **different orders** — ids run Edge, Pulse, Video, O.T.
    /// while codes run Edge, Video, Pulse, O.T. — so neither can be derived from the other
    /// by arithmetic.
    fn menu(self, channel: u8) -> u8 {
        let base = if channel == 1 { 26 } else { 30 };
        base + match self {
            Self::Edge => 0,
            Self::Pulse => 1,
            Self::Video => 2,
            Self::Overtime => 3,
        }
    }

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Edge => "Edge",
            Self::Video => "Video",
            Self::Pulse => "Pulse",
            Self::Overtime => "Overtime",
        }
    }
}

/// One channel's trigger inside Alter mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlterChannel {
    /// What this channel triggers on.
    pub kind: AlterType,
    /// Edge direction, or pulse/video polarity.
    pub polarity: Polarity,
    /// How the comparator sees this channel.
    pub coupling: TriggerCoupling,
    /// Pulse only: how the measured width is compared.
    pub qualifier: Qualifier,
    /// Video only: which standard.
    pub video_standard: VideoStandard,
    /// Video only: which part of the frame.
    pub video_sync: VideoSync,
}

impl AlterChannel {
    /// Whether this channel counts as holding `wanted`.
    ///
    /// Only the fields that belong to `wanted.kind` are compared. The scope keeps values in
    /// the others regardless — a channel set to Edge still reports some `PULSE-WHEN` — so
    /// comparing them would fail on settings neither side ever applied.
    pub fn matches(&self, wanted: &AlterChannel) -> bool {
        if self.kind != wanted.kind || self.polarity != wanted.polarity {
            return false;
        }
        match wanted.kind {
            // Video's page has no Coupling box, so the field is whatever an earlier
            // sub-type left behind and comparing it would fail on a setting neither side
            // ever applied.
            AlterType::Video => {
                self.video_standard == wanted.video_standard
                    && self.video_sync == wanted.video_sync
            }
            AlterType::Pulse => {
                self.coupling == wanted.coupling && self.qualifier == wanted.qualifier
            }
            AlterType::Edge | AlterType::Overtime => self.coupling == wanted.coupling,
        }
    }
}

impl Default for AlterChannel {
    fn default() -> Self {
        Self {
            kind: AlterType::Edge,
            polarity: Polarity::Positive,
            coupling: TriggerCoupling::Dc,
            qualifier: Qualifier::Greater,
            video_standard: VideoStandard::Ntsc,
            video_sync: VideoSync::AllLines,
        }
    }
}

/// What the trigger watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerSource {
    /// Analog channel 1.
    Ch1,
    /// Analog channel 2.
    Ch2,
    /// External trigger input.
    External,
    /// External trigger input, divided by five.
    ExternalDiv5,
    /// The mains waveform. Edge trigger only.
    AcLine,
}

impl TriggerSource {
    /// The `TRIG-SRC` code.
    pub fn code(self) -> u8 {
        match self {
            Self::Ch1 => 0,
            Self::Ch2 => 1,
            Self::External => 2,
            Self::ExternalDiv5 => 3,
            Self::AcLine => 4,
        }
    }

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ch1 => "CH1",
            Self::Ch2 => "CH2",
            Self::External => "EXT",
            Self::ExternalDiv5 => "EXT/5",
            Self::AcLine => "AC line",
        }
    }
}

/// When the scope draws without a trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    /// Free-run if no trigger arrives.
    Auto,
    /// Wait indefinitely for a trigger.
    Normal,
}

impl TriggerMode {
    /// The `TRIG-MODE` code.
    pub fn code(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Normal => 1,
        }
    }
}

/// How the trigger comparator sees the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerCoupling {
    /// Pass everything.
    Dc,
    /// Block DC.
    Ac,
    /// Reject noise.
    NoiseReject,
    /// Reject high frequencies.
    HighFrequencyReject,
    /// Reject low frequencies.
    LowFrequencyReject,
}

impl TriggerCoupling {
    /// The `TRIG-COUP` code.
    pub fn code(self) -> u8 {
        match self {
            Self::Dc => 0,
            Self::Ac => 1,
            Self::NoiseReject => 2,
            Self::HighFrequencyReject => 3,
            Self::LowFrequencyReject => 4,
        }
    }
}

/// Which way the signal must cross the level, or which way a pulse points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// Rising edge / positive pulse / normal video.
    Positive,
    /// Falling edge / negative pulse / inverted video.
    Negative,
}

impl Polarity {
    /// The code these fields share: 0 = positive, 1 = negative.
    pub fn code(self) -> u8 {
        match self {
            Self::Positive => 0,
            Self::Negative => 1,
        }
    }
}

/// How a measured width or time is compared with the set one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qualifier {
    /// Equal to.
    Equal,
    /// Not equal to.
    NotEqual,
    /// Greater than.
    Greater,
    /// Less than.
    Less,
}

impl Qualifier {
    /// The code shared by `TRIG-PULSE-WHEN` and `TRIG-SLOPE-WHEN`.
    pub fn code(self) -> u8 {
        match self {
            Self::Equal => 0,
            Self::NotEqual => 1,
            Self::Greater => 2,
            Self::Less => 3,
        }
    }
}

/// Which video standard the sync separator expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoStandard {
    /// NTSC, 525 lines.
    Ntsc,
    /// PAL or SECAM, 625 lines.
    PalSecam,
}

impl VideoStandard {
    /// The `TRIG-VIDEO-PAL` code.
    pub fn code(self) -> u8 {
        match self {
            Self::Ntsc => 0,
            Self::PalSecam => 1,
        }
    }
}

/// Which part of the video frame to sync on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoSync {
    /// Every line.
    AllLines,
    /// One numbered line.
    LineNumber,
    /// The odd field.
    OddField,
    /// The even field.
    EvenField,
    /// Every field.
    AllFields,
}

impl VideoSync {
    /// The `TRIG-VIDEO-SYN` code.
    pub fn code(self) -> u8 {
        match self {
            Self::AllLines => 0,
            Self::LineNumber => 1,
            Self::OddField => 2,
            Self::EvenField => 3,
            Self::AllFields => 4,
        }
    }
}

/// A continuous trigger parameter — one the scope offers no keyed entry for, only its
/// multipurpose knob.
///
/// Each is reached by opening the page that owns it and pressing the softkey in its slot,
/// which hands the knob that parameter. The slot model is what makes this predictable: on
/// every trigger page `Fn0` is the title bar, `Fn1`–`Fn5` are the five boxes top to bottom,
/// and `Fn6` is the page turn. A key in an empty slot changes nothing and leaves the knob
/// wherever it was — which is exactly how an earlier guess at these keys appeared to work
/// while pointing at the wrong slot entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adjustable {
    /// Pulse: the width the qualifier compares against.
    PulseWidth,
    /// Slope: the first (upper) threshold.
    SlopeV1,
    /// Slope: the second (lower) threshold.
    SlopeV2,
    /// Slope: the transition time the qualifier compares against.
    SlopeTime,
    /// Overtime: the time the signal must stay at a level.
    OvertimeTime,
    /// Alter, CH1 on Pulse: the width the qualifier compares against.
    AlterCh1PulseWidth,
    /// Alter, CH1 on Overtime: the time the level must be held.
    AlterCh1OvertimeTime,
    /// Alter, CH2 on Pulse.
    AlterCh2PulseWidth,
    /// Alter, CH2 on Overtime.
    AlterCh2OvertimeTime,
    /// Video with Sync = LineNumber: which line to trigger on.
    VideoLine,
    /// Alter, CH1 on Video with Sync = LineNumber.
    AlterCh1VideoLine,
    /// Alter, CH2 on Video with Sync = LineNumber.
    AlterCh2VideoLine,
}

impl Adjustable {
    /// The settings-block field this parameter lives in.
    pub fn field(self) -> &'static str {
        match self {
            Self::PulseWidth => "TRIG-PULSE-TIME",
            Self::SlopeV1 => "TRIG-SLOPE-V1",
            Self::SlopeV2 => "TRIG-SLOPE-V2",
            Self::SlopeTime => "TRIG-SLOPE-TIME",
            Self::OvertimeTime => "TRIG-OVERTIME-TIME",
            Self::AlterCh1PulseWidth => "TRIG-SWAP-CH1-PULSE-TIME",
            Self::AlterCh1OvertimeTime => "TRIG-SWAP-CH1-OVERTIME-TIME",
            Self::AlterCh2PulseWidth => "TRIG-SWAP-CH2-PULSE-TIME",
            Self::AlterCh2OvertimeTime => "TRIG-SWAP-CH2-OVERTIME-TIME",
            Self::VideoLine => "TRIG-VIDEO-LINE",
            Self::AlterCh1VideoLine => "TRIG-SWAP-CH1-VIDEO-LINE",
            Self::AlterCh2VideoLine => "TRIG-SWAP-CH2-VIDEO-LINE",
        }
    }

    /// The Alter channel this belongs to, if any.
    fn alter_channel(self) -> Option<u8> {
        match self {
            Self::AlterCh1PulseWidth | Self::AlterCh1OvertimeTime | Self::AlterCh1VideoLine => {
                Some(1)
            }
            Self::AlterCh2PulseWidth | Self::AlterCh2OvertimeTime | Self::AlterCh2VideoLine => {
                Some(2)
            }
            _ => None,
        }
    }

    /// The Alter sub-type whose page carries it.
    fn alter_type(self) -> Option<AlterType> {
        match self {
            Self::AlterCh1PulseWidth | Self::AlterCh2PulseWidth => Some(AlterType::Pulse),
            Self::AlterCh1OvertimeTime | Self::AlterCh2OvertimeTime => Some(AlterType::Overtime),
            Self::AlterCh1VideoLine | Self::AlterCh2VideoLine => Some(AlterType::Video),
            _ => None,
        }
    }

    /// Whether it is on the type's second page. Overtime's time is on the **first**, and
    /// the Alter values are on a channel page, which is reached another way entirely.
    fn on_second_page(self) -> bool {
        matches!(
            self,
            Self::PulseWidth | Self::SlopeV1 | Self::SlopeV2 | Self::SlopeTime
        )
    }

    /// The softkey whose slot owns it, if one has to be pressed.
    ///
    /// The video line number has none. Once Sync reads `LineNumber` the knob already owns
    /// the line — measured on hardware — and pressing the Sync box again would cycle Sync
    /// away from `LineNumber` and lose the very parameter being adjusted.
    fn key(self) -> Option<Key> {
        Some(match self {
            // "Vertical" — one box holding both thresholds, selected by TRIG-SLOPE-WIN.
            Self::SlopeV1 | Self::SlopeV2 => Key::Fn3,
            Self::PulseWidth | Self::SlopeTime | Self::OvertimeTime => Key::Fn5,
            // On an Alter channel page the boxes sit higher: Pulse puts Set PW in slot 4
            // (When is above it), Overtime puts its time in slot 3.
            Self::AlterCh1PulseWidth | Self::AlterCh2PulseWidth => Key::Fn4,
            Self::AlterCh1OvertimeTime | Self::AlterCh2OvertimeTime => Key::Fn3,
            Self::VideoLine | Self::AlterCh1VideoLine | Self::AlterCh2VideoLine => {
                return None
            }
        })
    }

    /// Which value of `TRIG-SLOPE-WIN` the Vertical box must be showing, if it applies.
    fn slope_window(self) -> Option<u8> {
        match self {
            Self::SlopeV1 => Some(0),
            Self::SlopeV2 => Some(1),
            _ => None,
        }
    }

    /// Whether the value is a time (picoseconds) rather than a level.
    pub fn is_time(self) -> bool {
        matches!(
            self,
            Self::PulseWidth
                | Self::SlopeTime
                | Self::OvertimeTime
                | Self::AlterCh1PulseWidth
                | Self::AlterCh1OvertimeTime
                | Self::AlterCh2PulseWidth
                | Self::AlterCh2OvertimeTime
        )
    }

    /// The trigger type this parameter belongs to.
    fn belongs_to(self) -> TriggerType {
        match self {
            Self::PulseWidth => TriggerType::Pulse,
            Self::SlopeV1 | Self::SlopeV2 | Self::SlopeTime => TriggerType::Slope,
            Self::OvertimeTime => TriggerType::Overtime,
            Self::VideoLine => TriggerType::Video,
            Self::AlterCh1PulseWidth
            | Self::AlterCh1OvertimeTime
            | Self::AlterCh1VideoLine
            | Self::AlterCh2PulseWidth
            | Self::AlterCh2OvertimeTime
            | Self::AlterCh2VideoLine => TriggerType::Alter,
        }
    }

    /// The Sync box's softkey, for the line number — the parameter it owns.
    ///
    /// Pressing it is how the knob is handed the line number, but each press also advances
    /// Sync, so it must be pressed a whole ring to come back to where it started.
    fn sync_key(self) -> Option<Key> {
        match self {
            Self::VideoLine => Some(Key::Fn5),
            // The Alter channel pages carry one box fewer, so Sync sits a slot higher.
            Self::AlterCh1VideoLine | Self::AlterCh2VideoLine => Some(Key::Fn4),
            _ => None,
        }
    }

    /// How far one knob press moves this value, in the units its field is stored in.
    ///
    /// Measured on hardware: the times step 10 ns (400 consecutive presses across
    /// 0.5–4.5 µs, constant throughout), and the thresholds and the line number step one
    /// unit. Used by a UI to offer the same lattice the instrument does; the driver itself
    /// learns the step from the read-back rather than trusting this.
    pub fn step(self) -> i64 {
        if self.is_time() {
            10_000 // picoseconds
        } else {
            1
        }
    }

    /// Whether the value is a plain count rather than a time or a level.
    pub fn is_count(self) -> bool {
        matches!(
            self,
            Self::VideoLine | Self::AlterCh1VideoLine | Self::AlterCh2VideoLine
        )
    }

    /// Human-readable name, as the scope labels it.
    pub fn label(self) -> &'static str {
        match self {
            Self::PulseWidth => "Pulse width",
            Self::SlopeV1 => "Threshold V1",
            Self::SlopeV2 => "Threshold V2",
            Self::SlopeTime => "Slope time",
            Self::OvertimeTime => "Overtime",
            Self::AlterCh1PulseWidth => "CH1 pulse width",
            Self::AlterCh1OvertimeTime => "CH1 overtime",
            Self::AlterCh2PulseWidth => "CH2 pulse width",
            Self::AlterCh2OvertimeTime => "CH2 overtime",
            Self::VideoLine => "Line number",
            Self::AlterCh1VideoLine => "CH1 line number",
            Self::AlterCh2VideoLine => "CH2 line number",
        }
    }
}

/// The complete trigger configuration a caller can ask for.
///
/// Only the fields that belong to `kind` are applied — asking for a video standard while
/// the type is Edge would mean navigating to a page that is not open, so those values are
/// carried but ignored. That keeps the UI free to remember every type's settings while
/// only one is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerSetup {
    /// What the trigger looks for.
    pub kind: TriggerType,
    /// What it watches.
    pub source: TriggerSource,
    /// Whether it free-runs without a trigger. Not offered by Video.
    pub mode: TriggerMode,
    /// How the comparator sees the source. Not offered by Video or Overtime.
    pub coupling: TriggerCoupling,
    /// Edge direction, pulse/video polarity, or slope direction — whichever the type uses.
    pub polarity: Polarity,
    /// Video: which standard.
    pub video_standard: VideoStandard,
    /// Video: which part of the frame.
    pub video_sync: VideoSync,
    /// Pulse and Slope: how the measured time is compared.
    pub qualifier: Qualifier,
    /// Alter: CH1's own trigger.
    pub alter_ch1: AlterChannel,
    /// Alter: CH2's own trigger.
    pub alter_ch2: AlterChannel,
}

impl Default for TriggerSetup {
    fn default() -> Self {
        Self {
            kind: TriggerType::Edge,
            source: TriggerSource::Ch1,
            mode: TriggerMode::Auto,
            coupling: TriggerCoupling::Dc,
            polarity: Polarity::Positive,
            video_standard: VideoStandard::Ntsc,
            video_sync: VideoSync::AllLines,
            qualifier: Qualifier::Greater,
            alter_ch1: AlterChannel::default(),
            alter_ch2: AlterChannel::default(),
        }
    }
}

impl TriggerSetup {
    /// Whether the scope holding `self` counts as holding `wanted`.
    ///
    /// Only the fields that belong to the requested type are compared. The rest are carried
    /// so a UI can remember every type's settings, but they are never applied while another
    /// type is live, so the scope has no reason to agree about them.
    pub fn matches(&self, wanted: &TriggerSetup) -> bool {
        if self.kind != wanted.kind {
            return false;
        }
        // Alter has no single source or polarity — each channel carries its own.
        if wanted.kind == TriggerType::Alter {
            return self.alter_ch1.matches(&wanted.alter_ch1)
                && self.alter_ch2.matches(&wanted.alter_ch2);
        }
        if self.source != wanted.source || self.polarity != wanted.polarity {
            return false;
        }
        match wanted.kind {
            TriggerType::Video => {
                self.video_standard == wanted.video_standard
                    && self.video_sync == wanted.video_sync
            }
            // Overtime keeps its coupling on a page this module does not drive.
            TriggerType::Overtime => self.mode == wanted.mode,
            TriggerType::Pulse | TriggerType::Slope => {
                self.mode == wanted.mode
                    && self.coupling == wanted.coupling
                    && self.qualifier == wanted.qualifier
            }
            TriggerType::Edge => self.mode == wanted.mode && self.coupling == wanted.coupling,
            TriggerType::Alter => unreachable!("handled above"),
        }
    }
}

impl TriggerSetup {
    /// The continuous parameters this configuration offers.
    ///
    /// A method on the setup rather than the type, because under Alter it depends on what
    /// each *channel* is set to: only a channel on Pulse has a width, only one on Overtime
    /// has a hold time.
    pub fn adjustables(&self) -> Vec<Adjustable> {
        if self.kind == TriggerType::Video {
            // The line number is only a parameter while Sync is set to trigger on one.
            return if self.video_sync == VideoSync::LineNumber {
                vec![Adjustable::VideoLine]
            } else {
                Vec::new()
            };
        }
        if self.kind != TriggerType::Alter {
            return self.kind.adjustables().to_vec();
        }
        let mut out = Vec::new();
        for (channel, width, hold, line) in [
            (
                self.alter_ch1,
                Adjustable::AlterCh1PulseWidth,
                Adjustable::AlterCh1OvertimeTime,
                Adjustable::AlterCh1VideoLine,
            ),
            (
                self.alter_ch2,
                Adjustable::AlterCh2PulseWidth,
                Adjustable::AlterCh2OvertimeTime,
                Adjustable::AlterCh2VideoLine,
            ),
        ] {
            match channel.kind {
                AlterType::Pulse => out.push(width),
                AlterType::Overtime => out.push(hold),
                AlterType::Video if channel.video_sync == VideoSync::LineNumber => out.push(line),
                AlterType::Edge | AlterType::Video => {}
            }
        }
        out
    }
}

/// Read the trigger configuration the scope is currently holding.
pub fn read(settings: &Settings) -> Option<TriggerSetup> {
    let code = |name: &str| settings.field(name).unwrap_or(0) as u8;
    let kind = match code("TRIG-TYPE") {
        0 => TriggerType::Edge,
        1 => TriggerType::Video,
        2 => TriggerType::Pulse,
        3 => TriggerType::Slope,
        4 => TriggerType::Overtime,
        5 => TriggerType::Alter,
        _ => return None,
    };
    // The polarity field is whichever one this type uses.
    let polarity_field = match kind {
        TriggerType::Edge => "TRIG-EDGE-SLOPE",
        TriggerType::Video => "TRIG-VIDEO-NEG",
        TriggerType::Pulse => "TRIG-PULSE-NEG",
        TriggerType::Slope => "TRIG-SLOPE-SET",
        TriggerType::Overtime => "TRIG-OVERTIME-NEG",
        // In Alter the main fields mirror whichever channel page was last open, so none of
        // them describes the trigger; the per-channel fields below do.
        TriggerType::Alter => "TRIG-EDGE-SLOPE",
    };
    Some(TriggerSetup {
        kind,
        source: match code("TRIG-SRC") {
            0 => TriggerSource::Ch1,
            1 => TriggerSource::Ch2,
            2 => TriggerSource::External,
            3 => TriggerSource::ExternalDiv5,
            _ => TriggerSource::AcLine,
        },
        mode: match code("TRIG-MODE") {
            0 => TriggerMode::Auto,
            _ => TriggerMode::Normal,
        },
        coupling: match code("TRIG-COUP") {
            0 => TriggerCoupling::Dc,
            1 => TriggerCoupling::Ac,
            2 => TriggerCoupling::NoiseReject,
            3 => TriggerCoupling::HighFrequencyReject,
            _ => TriggerCoupling::LowFrequencyReject,
        },
        polarity: match code(polarity_field) {
            0 => Polarity::Positive,
            _ => Polarity::Negative,
        },
        video_standard: match code("TRIG-VIDEO-PAL") {
            0 => VideoStandard::Ntsc,
            _ => VideoStandard::PalSecam,
        },
        video_sync: match code("TRIG-VIDEO-SYN") {
            0 => VideoSync::AllLines,
            1 => VideoSync::LineNumber,
            2 => VideoSync::OddField,
            3 => VideoSync::EvenField,
            _ => VideoSync::AllFields,
        },
        qualifier: match code(match kind {
            TriggerType::Slope => "TRIG-SLOPE-WHEN",
            _ => "TRIG-PULSE-WHEN",
        }) {
            0 => Qualifier::Equal,
            1 => Qualifier::NotEqual,
            2 => Qualifier::Greater,
            _ => Qualifier::Less,
        },
        alter_ch1: read_alter_channel(settings, 1),
        alter_ch2: read_alter_channel(settings, 2),
    })
}

/// Decode one channel's Alter trigger from its `TRIG-SWAP-CHx-*` fields.
fn read_alter_channel(settings: &Settings, channel: u8) -> AlterChannel {
    let code = |suffix: &str| {
        settings
            .field(&format!("TRIG-SWAP-CH{channel}-{suffix}"))
            .unwrap_or(0) as u8
    };
    let kind = match code("TYPE") {
        1 => AlterType::Video,
        2 => AlterType::Pulse,
        3 => AlterType::Overtime,
        _ => AlterType::Edge,
    };
    let polarity_code = match kind {
        AlterType::Edge => code("EDGE-SLOPE"),
        AlterType::Video => code("VIDEO-NEG"),
        AlterType::Pulse => code("PULSE-NEG"),
        AlterType::Overtime => code("OVERTIME-NEG"),
    };
    AlterChannel {
        kind,
        polarity: if polarity_code == 0 { Polarity::Positive } else { Polarity::Negative },
        coupling: match code("COUP") {
            0 => TriggerCoupling::Dc,
            1 => TriggerCoupling::Ac,
            2 => TriggerCoupling::NoiseReject,
            3 => TriggerCoupling::HighFrequencyReject,
            _ => TriggerCoupling::LowFrequencyReject,
        },
        qualifier: match code("PULSE-WHEN") {
            0 => Qualifier::Equal,
            1 => Qualifier::NotEqual,
            2 => Qualifier::Greater,
            _ => Qualifier::Less,
        },
        video_standard: if code("VIDEO-PAL") == 0 {
            VideoStandard::Ntsc
        } else {
            VideoStandard::PalSecam
        },
        video_sync: match code("VIDEO-SYN") {
            0 => VideoSync::AllLines,
            1 => VideoSync::LineNumber,
            2 => VideoSync::OddField,
            3 => VideoSync::EvenField,
            _ => VideoSync::AllFields,
        },
    }
}

// --- softkeys, by what they do (statemachines.md §3.9) ----------------------

/// Cycles `TRIG-TYPE`, and the open menu with it.
const KEY_TYPE: Key = Key::Fn1;
/// Cycles `TRIG-SRC`, over a ring whose length depends on the type.
const KEY_SOURCE: Key = Key::Fn2;
/// Toggles the type's edge/polarity field.
const KEY_POLARITY: Key = Key::Fn3;
/// Toggles `TRIG-MODE` — except on Video, where it is the standard.
const KEY_MODE: Key = Key::Fn4;
/// Cycles `TRIG-COUP` — except on Video, where it is the sync selector.
const KEY_COUPLING: Key = Key::Fn5;
/// Advances to a type's second page.
const KEY_PAGE: Key = Key::Fn6;
/// On page 2, cycles the `When` qualifier.
const KEY_QUALIFIER: Key = Key::Fn4;

/// Apply a whole trigger configuration.
///
/// The type is set first: it decides which page is open, and therefore what every other
/// softkey means.
pub fn apply(device: &Device, setup: &TriggerSetup) -> Result<()> {
    apply_reporting(device, setup, &mut |_, _| {})
}

/// Apply a whole trigger configuration, reporting progress as each field lands.
///
/// Every field is a run of verified key presses that can take a second or more, so a caller
/// driving a progress bar needs to hear about them individually — a single "done" at the end
/// looks like a hang. `progress` is called with (fields settled, fields total).
pub fn apply_reporting(
    device: &Device,
    setup: &TriggerSetup,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    // Do nothing at all if the scope already holds this configuration. Applying is a menu
    // navigation and a run of verified key presses — seconds of USB traffic that also takes
    // over the front panel — so the common case of "already right" should cost one settings
    // read, not a walk through the trigger tree.
    if read(&device.read_settings()?).is_some_and(|current| current.matches(setup)) {
        debug!(kind = setup.kind.name(), "trigger already set; nothing to do");
        progress(FIELDS, FIELDS);
        return Ok(());
    }

    let mut done = 0u64;
    let step = |progress: &mut dyn FnMut(u64, u64), done: &mut u64| {
        *done += 1;
        progress(*done, FIELDS);
    };

    set_type(device, setup.kind)?;
    step(progress, &mut done);

    // Alter has no shared source, polarity, mode or coupling — each channel is configured
    // on its own page, so the rest of this function does not apply.
    if setup.kind == TriggerType::Alter {
        apply_alter_channel(device, 1, &setup.alter_ch1)?;
        step(progress, &mut done);
        apply_alter_channel(device, 2, &setup.alter_ch2)?;
        progress(FIELDS, FIELDS);
        debug!("alter trigger configured");
        return Ok(());
    }

    let ring = setup.kind.source_ring();
    if u32::from(setup.source.code()) >= ring {
        return Err(Error::Unexpected(format!(
            "{} trigger does not offer {} as a source",
            setup.kind.name(),
            setup.source.name()
        )));
    }
    cycle(device, KEY_SOURCE, ring, "TRIG-SRC", setup.source.code())?;
    step(progress, &mut done);

    let polarity_field = match setup.kind {
        TriggerType::Edge => "TRIG-EDGE-SLOPE",
        TriggerType::Video => "TRIG-VIDEO-NEG",
        TriggerType::Pulse => "TRIG-PULSE-NEG",
        TriggerType::Slope => "TRIG-SLOPE-SET",
        TriggerType::Overtime => "TRIG-OVERTIME-NEG",
        TriggerType::Alter => unreachable!("handled above"),
    };
    cycle(device, KEY_POLARITY, 2, polarity_field, setup.polarity.code())?;
    step(progress, &mut done);

    if setup.kind == TriggerType::Video {
        // Video's Fn4/Fn5 are Standard and Sync, not Mode and Coupling.
        cycle(
            device,
            KEY_MODE,
            2,
            "TRIG-VIDEO-PAL",
            setup.video_standard.code(),
        )?;
        step(progress, &mut done);
        cycle(device, KEY_COUPLING, 5, "TRIG-VIDEO-SYN", setup.video_sync.code())?;
        step(progress, &mut done);
    } else {
        cycle(device, KEY_MODE, 2, "TRIG-MODE", setup.mode.code())?;
        step(progress, &mut done);
        if setup.kind == TriggerType::Overtime {
            // Overtime's page 1 slot 5 is its time, not coupling; coupling is the only box
            // on page 2.
            if device.read_settings()?.field("TRIG-COUP") != Some(u64::from(setup.coupling.code()))
            {
                open_second_page(device, setup.kind)?;
                cycle(device, KEY_COUPLING, 5, "TRIG-COUP", setup.coupling.code())?;
                open_trigger(device)?;
            }
        } else {
            cycle(device, KEY_COUPLING, 5, "TRIG-COUP", setup.coupling.code())?;
        }
        step(progress, &mut done);
    }

    // The When qualifier lives on page 2, so it costs a page turn — only taken for the
    // types that have one.
    if matches!(setup.kind, TriggerType::Pulse | TriggerType::Slope) {
        let field = if setup.kind == TriggerType::Slope {
            "TRIG-SLOPE-WHEN"
        } else {
            "TRIG-PULSE-WHEN"
        };
        if device.read_settings()?.field(field) != Some(u64::from(setup.qualifier.code())) {
            open_second_page(device, setup.kind)?;
            cycle(device, KEY_QUALIFIER, 4, field, setup.qualifier.code())?;
            // Leave the scope on page 1, both because that is where an operator expects to
            // find it and because every other entry point assumes it.
            open_trigger(device)?;
        }
        step(progress, &mut done);
    }

    progress(FIELDS, FIELDS);
    debug!(kind = setup.kind.name(), "trigger configured");
    Ok(())
}

/// Fields [`apply_reporting`] walks, for the progress denominator: type, source, polarity,
/// mode (or video standard), coupling (or video sync), and the qualifier.
const FIELDS: u64 = 6;

/// Configure one channel's trigger inside Alter mode.
///
/// Each channel has its own page per sub-type, reached from the Alter base page. The slot
/// layout differs by sub-type — a list too long for one box gets two keys, one per scroll
/// direction — so the keys are looked up rather than assumed.
fn apply_alter_channel(device: &Device, channel: u8, wanted: &AlterChannel) -> Result<()> {
    // Open this channel's page from the Alter base.
    let open_key = if channel == 1 { KEY_ALTER_CH1 } else { KEY_ALTER_CH2 };
    open_alter_channel(device, channel, open_key)?;

    // The sub-type decides what the remaining slots mean, so it goes first.
    let type_field = format!("TRIG-SWAP-CH{channel}-TYPE");
    cycle(device, KEY_TYPE, ALTER_TYPE_RING, &type_field, wanted.kind.code())?;
    confirm_menu(device, wanted.kind.menu(channel))?;

    let field = |suffix: &str| format!("TRIG-SWAP-CH{channel}-{suffix}");
    let polarity_field = match wanted.kind {
        AlterType::Edge => field("EDGE-SLOPE"),
        AlterType::Video => field("VIDEO-NEG"),
        AlterType::Pulse => field("PULSE-NEG"),
        AlterType::Overtime => field("OVERTIME-NEG"),
    };
    cycle(device, Key::Fn2, 2, &polarity_field, wanted.polarity.code())?;

    match wanted.kind {
        AlterType::Edge => {
            // Coupling spans two slots with ▲/▼ arrows; the ▼ key wraps, so one direction
            // reaches every value.
            cycle(device, Key::Fn4, 5, &field("COUP"), wanted.coupling.code())?;
        }
        AlterType::Pulse => {
            cycle(device, Key::Fn3, 4, &field("PULSE-WHEN"), wanted.qualifier.code())?;
            cycle(device, Key::Fn5, 5, &field("COUP"), wanted.coupling.code())?;
        }
        AlterType::Video => {
            cycle(device, Key::Fn3, 2, &field("VIDEO-PAL"), wanted.video_standard.code())?;
            cycle(device, Key::Fn4, 5, &field("VIDEO-SYN"), wanted.video_sync.code())?;
        }
        AlterType::Overtime => {
            // Slot 3 is the overtime time (knob-only); coupling is the ▲/▼ pair below it.
            cycle(device, Key::Fn4, 5, &field("COUP"), wanted.coupling.code())?;
        }
    }

    // Back to the Alter base, so the other channel can be opened the same way.
    for _ in 0..4 {
        if device.read_settings()?.menu_id() == ALTER_MENU {
            return Ok(());
        }
        device.press(KEY_ALTER_BACK)?;
        sleep(MENU_SETTLE);
    }
    Err(Error::Unexpected(format!(
        "could not return to the Alter base page from CH{channel}"
    )))
}

/// Press the CH1/CH2 button on the Alter base page until that channel's page is showing.
fn open_alter_channel(device: &Device, channel: u8, key: Key) -> Result<()> {
    let pages: Vec<u8> = [
        AlterType::Edge,
        AlterType::Pulse,
        AlterType::Video,
        AlterType::Overtime,
    ]
    .iter()
    .map(|t| t.menu(channel))
    .collect();

    for _ in 0..4 {
        let menu = device.read_settings()?.menu_id();
        if pages.contains(&menu) {
            return Ok(());
        }
        if menu != ALTER_MENU {
            // Somewhere else entirely — get back to the base first.
            device.press(KEY_ALTER_BACK)?;
            sleep(MENU_SETTLE);
            continue;
        }
        device.press(key)?;
        sleep(MENU_SETTLE);
    }
    Err(Error::Unexpected(format!(
        "the Alter CH{channel} page did not open (showing menu {})",
        device.read_settings()?.menu_id()
    )))
}

/// Confirm the scope is showing `wanted`, allowing for a press that has not landed yet.
fn confirm_menu(device: &Device, wanted: u8) -> Result<()> {
    for _ in 0..4 {
        if device.read_settings()?.menu_id() == wanted {
            return Ok(());
        }
        sleep(MENU_SETTLE);
    }
    Err(Error::Unexpected(format!(
        "expected menu {wanted}, scope is showing {}",
        device.read_settings()?.menu_id()
    )))
}

/// Select a trigger type, leaving its first page open.
pub fn set_type(device: &Device, kind: TriggerType) -> Result<()> {
    goto_type(device, kind)?;
    Ok(())
}

/// Cycle the type softkey until `TRIG-TYPE` reads this type, and confirm the page opened.
///
/// The trigger key is only used to get *into* the trigger tree; from there navigation is by
/// type, because the trigger key always reopens the page matching the current type.
fn goto_type(device: &Device, kind: TriggerType) -> Result<()> {
    open_trigger(device)?;
    converge::cycle_until(
        device,
        KEY_TYPE,
        TYPE_RING,
        |settings| settings.field("TRIG-TYPE").map(|v| v as i64),
        i64::from(kind.code()),
    )?;
    // The menu should have followed the type. Where it has not, do not give up: pressing
    // the trigger key reopens the page for whatever type is now selected, which is exactly
    // the page wanted. Only a scope that will not land there after several tries is a real
    // failure — a later softkey aimed at the wrong page would change the wrong setting.
    const RENORMALISE: u32 = 3;
    for _ in 0..RENORMALISE {
        let menu = device.read_settings()?.menu_id();
        if kind.menus().contains(&menu) {
            return Ok(());
        }
        device.press(Key::MenuTrigger)?;
        sleep(MENU_SETTLE);
    }
    Err(Error::Unexpected(format!(
        "{} trigger is selected but the scope will not show its menu (showing {}, wanted one \
         of {:?})",
        kind.name(),
        device.read_settings()?.menu_id(),
        kind.menus()
    )))
}

/// Trigger types the type softkey cycles through, Alter included.
const TYPE_RING: u32 = 6;

/// The Alter base page.
const ALTER_MENU: u8 = 24;
/// Positions in the video Sync box — pressing it this many times returns to the start.
const VIDEO_SYNC_RING: u32 = 5;
/// Sub-types an Alter channel offers: Edge, Video, Pulse, Overtime.
const ALTER_TYPE_RING: u32 = 4;
/// On the Alter base page, opens CH1's own trigger page.
const KEY_ALTER_CH1: Key = Key::Fn2;
/// …and CH2's.
const KEY_ALTER_CH2: Key = Key::Fn3;
/// On a channel page, returns to the Alter base — the slot a paged menu gives its page turn.
const KEY_ALTER_BACK: Key = Key::Fn6;

/// Press the trigger key until one of the **first** trigger pages is showing.
///
/// It must be a first page specifically, not merely a trigger page: the type softkey only
/// selects the type on page 1, and on page 2 the same key does something else entirely. A
/// caller that left the scope on page 2 — setting a When qualifier does exactly that —
/// would otherwise press `Fn1` forever without the type ever moving.
///
/// Checking against known ids rather than "a menu is open" matters for the same reason at
/// the other end: straight after a Default Setup the scope shows menu 25, which would be
/// taken for a trigger page and every softkey aimed at the wrong menu.
fn open_trigger(device: &Device) -> Result<u8> {
    const ATTEMPTS: u32 = 6;
    for _ in 0..ATTEMPTS {
        let settings = device.read_settings()?;
        let menu = settings.menu_id();
        // The bar has to be *visible*, not merely selected. `CONTROL-MENUID` keeps its
        // value when the bar is hidden (`Fn0` toggles it), and softkeys sent to a hidden
        // bar do nothing at all — which looks exactly like a run of dropped presses.
        let showing = settings.field("CONTROL-DISP-MENU") == Some(1);
        if showing && FIRST_PAGES.contains(&menu) {
            return Ok(menu);
        }
        device.press(Key::MenuTrigger)?;
        sleep(MENU_SETTLE);
    }
    let settings = device.read_settings()?;
    Err(Error::Unexpected(format!(
        "the trigger menu did not open on a first page (menu {}, bar {})",
        settings.menu_id(),
        if settings.field("CONTROL-DISP-MENU") == Some(1) { "shown" } else { "hidden" }
    )))
}

/// The trigger pages whose `Fn1` is the type selector: one per type, plus the base page (11)
/// and Alter (24), which this module does not drive but can still cycle away from.
const FIRST_PAGES: [u8; 7] = [5, 6, 8, 11, 22, 24, 38];

/// Turn to a type's second page, confirming the menu changed.
fn open_second_page(device: &Device, kind: TriggerType) -> Result<()> {
    let Some(wanted) = kind.second_page() else {
        return Err(Error::Unexpected(format!(
            "{} trigger has no second page",
            kind.name()
        )));
    };
    const ATTEMPTS: u32 = 4;
    for _ in 0..ATTEMPTS {
        if device.read_settings()?.menu_id() == wanted {
            return Ok(());
        }
        device.press(KEY_PAGE)?;
        sleep(MENU_SETTLE);
    }
    Err(Error::Unexpected(format!(
        "{} page 2 (menu {wanted}) did not open",
        kind.name()
    )))
}

/// Press a softkey until its field reads `target`, or report that the scope will not take it.
fn cycle(device: &Device, key: Key, ring: u32, field: &str, target: u8) -> Result<()> {
    converge::cycle_until(
        device,
        key,
        ring,
        |settings| settings.field(field).map(|v| v as i64),
        i64::from(target),
    )
    .map_err(|e| Error::Unexpected(format!("could not set {field} to {target}: {e}")))?;
    Ok(())
}

/// Read the trigger level, or `None` if the reading would not be about CH1.
///
/// Everywhere but Alter this is simply `TRIG-VPOS`. Under Alter the field is shared: it
/// holds the level of whichever channel `TRIG-SRC` names, and the alternation moves that on
/// its own — so a read taken at the wrong moment reports the *other* channel's level as
/// though it were this one. Reading it only while `TRIG-SRC` says CH1 is what makes the
/// value mean something. `[verified: with the level knob turned twelve times across both
/// channel pages, only CH1's on-screen level moved — 0.00 V → 240 mV → 480 mV — while CH2's
/// stayed at 0.00 V throughout.]`
pub fn level_for_convergence(settings: &Settings) -> Option<i64> {
    let alternating = settings.field("TRIG-TYPE") == Some(u64::from(TriggerType::Alter.code()));
    if alternating && settings.field("TRIG-SRC") != Some(0) {
        return None;
    }
    Some(settings.trigger_position())
}

/// Set the trigger level, in the scope's 1/25-division units relative to screen centre.
///
/// The level knob is a genuine up/down control rather than a ring, so this converges on the
/// value instead of cycling.
pub fn set_level(device: &Device, position: i64) -> Result<i64> {
    converge::converge(device, Knob::TriggerLevel, position, 0, level_for_convergence)
}

/// Snap the trigger level to the source channel's ground.
///
/// This is the level knob's *push*, which is a different function from the panel's
/// "Set 50 %" softkey — and unlike that softkey, it does respond to an injected key event.
pub fn level_to_ground(device: &Device) -> Result<()> {
    device.press(Key::TriggerLevelZero)?;
    sleep(SETTLE);
    Ok(())
}

/// Nudge the trigger level by one step, for a UI that offers fine adjustment.
pub fn nudge_level(device: &Device, direction: Turn) -> Result<i64> {
    device.turn(Knob::TriggerLevel, direction, 1)?;
    sleep(SETTLE);
    Ok(device.read_settings()?.trigger_position())
}

/// Nudge a continuous parameter by `steps` of the multipurpose knob.
///
/// Nudging rather than setting is deliberate. These parameters have no keyed entry: the only
/// way to move them is one knob step per USB round trip, and a round trip costs the settle
/// time a verified press needs. Walking from the scope's minimum to a typical value would be
/// hundreds of steps and minutes of traffic, so a "type a value" control would be promising
/// something the link cannot deliver.
pub fn nudge(
    device: &Device,
    kind: TriggerType,
    what: Adjustable,
    direction: Turn,
    steps: u32,
) -> Result<Option<i64>> {
    hand_knob_the(device, kind, what)?;
    for _ in 0..steps {
        device.turn(Knob::Multipurpose, direction, 1)?;
        sleep(SETTLE);
    }
    let value = device.read_settings()?.field_signed(what.field());
    // Leave the scope on page 1, as every other entry point assumes.
    open_trigger(device)?;
    Ok(value)
}

/// Gap between knob presses when a run of them is fired without reading in between.
///
/// Measured: 40 presses landed in full at 200, 120, 80 and 50 ms apart, and started being
/// dropped at 30 ms (36/40) and 20 ms (28/40). 60 ms keeps a margin over the point where the
/// single-slot key mailbox begins to lose them, and is nearly seven times faster than the
/// settle a *verified* press needs — which is what makes it practical to walk a value
/// hundreds of steps.
const KNOB_SETTLE: Duration = Duration::from_millis(60);

/// Most presses fired in one run before reading back.
///
/// Sized to cover the whole remaining distance in a single run for any realistic target, so
/// a walk costs one read at the start, one run, and one read to confirm — rather than a read
/// every few presses. The cap only exists to bound a target the value cannot reach.
const KNOB_BATCH: u32 = 600;

/// Drive a continuous parameter to `target`, in the units its field is stored in.
///
/// Converges on the read-back rather than computing a press count, because the step is a
/// property of the instrument: it is 10 ns for the times and one unit for the thresholds and
/// the line number, but nothing guarantees that everywhere in the range. Presses are fired
/// in runs and the value read between runs, so a long walk costs one read per forty presses
/// instead of one per press.
pub fn set_value(
    device: &Device,
    kind: TriggerType,
    what: Adjustable,
    target: i64,
) -> Result<i64> {
    /// Runs before giving up — enough for a walk of several thousand steps.
    const ROUNDS: u32 = 60;

    hand_knob_the(device, kind, what)?;
    let read = |device: &Device| -> Result<i64> {
        device
            .read_settings()?
            .field_signed(what.field())
            .ok_or_else(|| Error::Unexpected(format!("{} is not readable", what.label())))
    };

    let mut value = read(device)?;
    for _ in 0..ROUNDS {
        if value == target {
            break;
        }
        let direction = if value < target { Turn::Up } else { Turn::Down };
        // Aim at the remaining distance, but never overshoot a batch: the step is learnt
        // from what the last run achieved rather than assumed.
        let step = knob_step(device, what, direction, &mut value)?;
        if step == 0 {
            // The value will not move — an end stop, or a parameter the mode does not have.
            break;
        }
        // Fire the whole remaining distance, then look: a dropped press or a step that is
        // not quite what it was leaves the value short, and the next round makes it up.
        let remaining = (target - value).abs() / step;
        let presses = remaining.clamp(0, i64::from(KNOB_BATCH)) as u32;
        for _ in 0..presses {
            device.turn(Knob::Multipurpose, direction, 1)?;
            sleep(KNOB_SETTLE);
        }
        sleep(SETTLE);
        let moved = read(device)?;
        if moved == value {
            break;
        }
        value = moved;
    }

    open_trigger(device)?;
    Ok(value)
}

/// One press in `direction`, returning how far the value moved. Updates `value`.
fn knob_step(
    device: &Device,
    what: Adjustable,
    direction: Turn,
    value: &mut i64,
) -> Result<i64> {
    device.turn(Knob::Multipurpose, direction, 1)?;
    sleep(SETTLE);
    let moved = device
        .read_settings()?
        .field_signed(what.field())
        .unwrap_or(*value);
    let step = (moved - *value).abs();
    *value = moved;
    Ok(step)
}

/// Navigate to wherever `what` lives and hand the multipurpose knob that parameter.
fn hand_knob_the(device: &Device, kind: TriggerType, what: Adjustable) -> Result<()> {
    if what.belongs_to() != kind {
        return Err(Error::Unexpected(format!(
            "{} trigger has no {}",
            kind.name(),
            what.label()
        )));
    }
    goto_type(device, kind)?;

    if let (Some(channel), Some(sub_type)) = (what.alter_channel(), what.alter_type()) {
        let open_key = if channel == 1 { KEY_ALTER_CH1 } else { KEY_ALTER_CH2 };
        open_alter_channel(device, channel, open_key)?;
        // The value only exists while that channel is on the sub-type that owns it.
        let type_field = format!("TRIG-SWAP-CH{channel}-TYPE");
        cycle(device, KEY_TYPE, ALTER_TYPE_RING, &type_field, sub_type.code())?;
        confirm_menu(device, sub_type.menu(channel))?;
    } else if what.on_second_page() {
        open_second_page(device, kind)?;
    }

    match (what.slope_window(), what.key(), what.sync_key()) {
        (Some(window), Some(key), _) => {
            cycle(device, key, 2, "TRIG-SLOPE-WIN", window).map(|_| ())?
        }
        (_, Some(key), _) => {
            device.press(key)?;
            sleep(SETTLE);
        }
        // The line number has no box of its own: it belongs to the Sync box, and the knob
        // only picks it up once that box is selected. Selecting it means pressing it — which
        // also advances Sync — so it is pressed a whole ring, arriving back at the value it
        // started on with the box now selected.
        (_, None, Some(sync)) => {
            for _ in 0..VIDEO_SYNC_RING {
                device.press(sync)?;
                sleep(SETTLE);
            }
        }
        (_, None, None) => {}
    }
    Ok(())
}
