//! Front-panel keys and knobs.
//!
//! A key is identified on the wire by its **0-based index in the scope's
//! `/keyprotocol.inf`** — that index is the `keyid` byte of a `0x13` frame. [`Key`]
//! enumerates all 49 of them.
//!
//! Knobs have no separate protocol: each is a pair of ± key ids (plus, for some, a push
//! key). [`Knob`] groups them so callers can say "turn the timebase" rather than tracking
//! which raw id means which direction.

/// A front-panel key, by its `/keyprotocol.inf` index.
///
/// The eight `Fn*` keys are the bezel softkeys; **what they do depends on the menu that is
/// currently open**, so they are only meaningful in the context of a known menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Key {
    /// Bezel softkey 1 (`FN-0-KEY`).
    Fn0 = 0,
    /// Bezel softkey 2.
    Fn1 = 1,
    /// Bezel softkey 3.
    Fn2 = 2,
    /// Bezel softkey 4.
    Fn3 = 3,
    /// Bezel softkey 5.
    Fn4 = 4,
    /// Bezel softkey 6.
    Fn5 = 5,
    /// Bezel softkey 7.
    Fn6 = 6,
    /// Bezel softkey 8.
    Fn7 = 7,

    /// Multipurpose knob, turn counter-clockwise / previous.
    MultiLeft = 8,
    /// Multipurpose knob, turn clockwise / next.
    MultiRight = 9,
    /// Multipurpose knob push (zero / select).
    MultiPush = 10,

    /// Save/Recall menu.
    MenuSaveRecall = 11,
    /// Measure menu.
    MenuMeasure = 12,
    /// Acquire menu.
    MenuAcquire = 13,
    /// Utility menu.
    MenuUtility = 14,
    /// Cursor menu.
    MenuCursor = 15,
    /// Display menu.
    MenuDisplay = 16,

    /// Autoset.
    Autoset = 17,
    /// Single sequence — arms one trigger-aligned acquisition, lands stopped.
    Single = 18,
    /// Run/Stop (a toggle).
    RunStop = 19,
    /// Help.
    Help = 20,
    /// Default Setup (factory reset to a known state).
    DefaultSetup = 21,
    /// Setup ("STU").
    Setup = 22,
    /// Math menu.
    MenuMath = 23,

    /// CH1 button — **toggles** CH1 shown/hidden and opens its menu.
    Ch1Menu = 24,
    /// CH1 vertical position −.
    Ch1PositionDown = 25,
    /// CH1 vertical position +.
    Ch1PositionUp = 26,
    /// CH1 position knob push (sets `VERT-CH1-POS` to 0).
    Ch1PositionZero = 27,
    /// CH1 volts/div −.
    Ch1VoltsDown = 28,
    /// CH1 volts/div +.
    Ch1VoltsUp = 29,

    /// CH2 button — **toggles** CH2 shown/hidden and opens its menu.
    Ch2Menu = 30,
    /// CH2 vertical position −.
    Ch2PositionDown = 31,
    /// CH2 vertical position +.
    Ch2PositionUp = 32,
    /// CH2 position knob push (sets `VERT-CH2-POS` to 0).
    Ch2PositionZero = 33,
    /// CH2 volts/div −.
    Ch2VoltsDown = 34,
    /// CH2 volts/div +.
    Ch2VoltsUp = 35,

    /// Horizontal menu.
    MenuHorizontal = 36,
    /// Horizontal delay −.
    DelayDown = 37,
    /// Horizontal delay +.
    DelayUp = 38,
    /// Horizontal position push (sets `HORIZ-TRIGTIME` to 0).
    DelayZero = 39,
    /// SEC/DIV, named "−" but **verified to move to a faster timebase** on this firmware.
    TimeBaseFaster = 40,
    /// SEC/DIV, named "+" but **verified to move to a slower timebase** on this firmware.
    TimeBaseSlower = 41,

    /// Trigger menu.
    MenuTrigger = 42,
    /// Trigger level −.
    TriggerLevelDown = 43,
    /// Trigger level +.
    TriggerLevelUp = 44,
    /// Trigger level knob push (snaps the level to channel ground).
    TriggerLevelZero = 45,
    /// Trigger "Set 50 %".
    ///
    /// **Verified inert over USB injection** — the physical key works, but a `0x13` event
    /// does not move `TRIG-VPOS`.
    TriggerSet50 = 46,
    /// Force trigger.
    ForceTrigger = 47,
    /// Probe check / compensation.
    ProbeCheck = 48,
}

impl Key {
    /// The `keyid` byte to put in a `0x13` frame.
    pub fn id(self) -> u8 {
        self as u8
    }
}

/// Direction of a knob turn, in terms of the **value** the knob controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Decrease the value (e.g. a smaller volts/div, a faster timebase).
    Down,
    /// Increase the value.
    Up,
}

/// A front-panel knob: a ± key pair, and for most a push action.
///
/// Directions are expressed in terms of the **value**, not the vendor's key names. That
/// matters for [`Knob::TimePerDiv`], whose `SUB`/`ADD` key names are inverted on this
/// firmware: [`Turn::Down`] here always means "smaller time/div" (faster).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Knob {
    /// CH1 volts/division.
    Ch1VoltsPerDiv,
    /// CH2 volts/division.
    Ch2VoltsPerDiv,
    /// CH1 vertical position.
    Ch1Position,
    /// CH2 vertical position.
    Ch2Position,
    /// Horizontal time/division.
    TimePerDiv,
    /// Horizontal delay (trigger time offset).
    HorizontalDelay,
    /// Trigger level.
    TriggerLevel,
    /// The general-purpose multipurpose knob.
    Multipurpose,
}

impl Knob {
    /// The key that moves this knob in `direction`.
    pub fn key(self, direction: Turn) -> Key {
        let (down, up) = match self {
            Self::Ch1VoltsPerDiv => (Key::Ch1VoltsDown, Key::Ch1VoltsUp),
            Self::Ch2VoltsPerDiv => (Key::Ch2VoltsDown, Key::Ch2VoltsUp),
            Self::Ch1Position => (Key::Ch1PositionDown, Key::Ch1PositionUp),
            Self::Ch2Position => (Key::Ch2PositionDown, Key::Ch2PositionUp),
            // Value semantics: Down = smaller time/div = faster. The key *names* are
            // inverted on this firmware, which is why this mapping looks swapped.
            Self::TimePerDiv => (Key::TimeBaseFaster, Key::TimeBaseSlower),
            Self::HorizontalDelay => (Key::DelayDown, Key::DelayUp),
            Self::TriggerLevel => (Key::TriggerLevelDown, Key::TriggerLevelUp),
            Self::Multipurpose => (Key::MultiLeft, Key::MultiRight),
        };
        match direction {
            Turn::Down => down,
            Turn::Up => up,
        }
    }

    /// The knob's push key, if it has one. Pushing generally zeroes the knob's axis.
    pub fn push_key(self) -> Option<Key> {
        Some(match self {
            Self::Ch1Position => Key::Ch1PositionZero,
            Self::Ch2Position => Key::Ch2PositionZero,
            Self::HorizontalDelay => Key::DelayZero,
            Self::TriggerLevel => Key::TriggerLevelZero,
            Self::Multipurpose => Key::MultiPush,
            Self::Ch1VoltsPerDiv | Self::Ch2VoltsPerDiv | Self::TimePerDiv => return None,
        })
    }

    /// The volts/div knob for channel `ch` (1 or 2), if that channel exists.
    pub fn volts_per_div(ch: u8) -> Option<Self> {
        match ch {
            1 => Some(Self::Ch1VoltsPerDiv),
            2 => Some(Self::Ch2VoltsPerDiv),
            _ => None,
        }
    }

    /// The vertical-position knob for channel `ch` (1 or 2), if that channel exists.
    pub fn position(ch: u8) -> Option<Self> {
        match ch {
            1 => Some(Self::Ch1Position),
            2 => Some(Self::Ch2Position),
            _ => None,
        }
    }
}
