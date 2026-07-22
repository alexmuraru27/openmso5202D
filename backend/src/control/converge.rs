//! Closed-loop control primitives.
//!
//! The scope's settings memory is read-only to us, so every change is made by pressing a
//! key and then **verifying the result**. There is no fire-and-forget: a press can be
//! dropped by the single-slot key mailbox, a knob can sit at an end stop, and the vendor's
//! `+`/`−` key labels are not always truthful.
//!
//! So the shape of essentially every operation is: read, compare, nudge, repeat. These
//! helpers capture that once instead of at every call site.

use std::thread::sleep;
use std::time::Duration;

use crate::device::{Device, Knob, Turn};
use crate::error::{Error, Result};
use crate::settings::Settings;

/// Time to let the scope apply a key press and update its settings block.
pub const SETTLE: Duration = Duration::from_millis(400);

/// Time to let a menu render after the key that opens it.
pub const MENU_SETTLE: Duration = Duration::from_millis(350);

/// Maximum knob nudges before giving up on reaching a target.
///
/// Generous: volts/div spans about a dozen steps end to end and the timebase spans 32.
const MAX_STEPS: u32 = 40;

/// Re-nudges of a knob that appears not to have moved before concluding it is a real end
/// stop. The single-slot key mailbox drops presses, so one dropped nudge looks exactly like
/// an end stop — the Python setters (`_step_key`) just keep pressing, so a transient
/// non-move must not abort the whole plan.
const NONMOVE_RETRIES: u32 = 4;

/// Attempts to open a menu before giving up.
const MENU_ATTEMPTS: u32 = 4;

/// Polls of the settings block per menu attempt.
const MENU_POLLS: u32 = 6;

/// Turn `knob` until the value `read` reports lands within `tolerance` of `target`.
///
/// The direction is decided from the read-back on every iteration rather than assumed, so
/// an inverted or mislabelled key pair cannot send it the wrong way. Returns the value
/// finally reached.
///
/// Fails if the target is unreachable — either the knob stops moving (an end stop) or the
/// step budget runs out, both of which mean the requested value is not one the scope
/// offers in its current mode.
pub fn converge(
    device: &Device,
    knob: Knob,
    target: i64,
    tolerance: i64,
    read: impl Fn(&Settings) -> Option<i64>,
) -> Result<i64> {
    converge_within(device, knob, target, tolerance, MAX_STEPS, read)
}

/// [`converge`] with an explicit step budget.
///
/// The default suits knobs that step through a scale — volts/division spans about a dozen
/// positions, the timebase 32. A knob that moves in fine units needs far more: the trigger
/// level covers ±8 divisions at 25 units each, so crossing the screen is hundreds of presses
/// even though every one of them lands.
pub fn converge_within(
    device: &Device,
    knob: Knob,
    target: i64,
    tolerance: i64,
    max_steps: u32,
    read: impl Fn(&Settings) -> Option<i64>,
) -> Result<i64> {
    // A `None` from `read` means "not readable *right now*", not "never readable" — the
    // trigger level under an alternating trigger shares its field with the other channel and
    // only reports this one's value while the alternation is on it. So look again rather
    // than give up; only a value that stays unreadable is a real failure.
    const READ_ATTEMPTS: u32 = 12;
    let current = |device: &Device| -> Result<i64> {
        for attempt in 0..READ_ATTEMPTS {
            if let Some(value) = read(&device.read_settings()?) {
                return Ok(value);
            }
            if attempt + 1 < READ_ATTEMPTS {
                sleep(Duration::from_millis(150));
            }
        }
        Err(Error::Unexpected(format!(
            "{knob:?} value never became readable from the settings block"
        )))
    };

    let mut value = current(device)?;
    for _ in 0..max_steps {
        if (value - target).abs() <= tolerance {
            return Ok(value);
        }
        let direction = if value < target { Turn::Up } else { Turn::Down };
        device.turn(knob, direction, 1)?;
        sleep(SETTLE);

        // A knob that appears not to have moved is usually a dropped press, not an end
        // stop — the mailbox is single-slot, and the scope drops keys outright while it is
        // busy (straight after a capture, for instance, where it is resuming and
        // re-acquiring). Re-nudge, waiting longer each time, before concluding it really
        // cannot move: verified on hardware that the SEC/DIV keys walk the ladder one rung
        // per press once the scope is idle, so a stall here means "not listening yet",
        // not "end stop".
        let mut moved = current(device)?;
        let mut stalls = 0;
        while moved == value && stalls < NONMOVE_RETRIES {
            sleep(SETTLE * (stalls + 1));
            device.turn(knob, direction, 1)?;
            sleep(SETTLE);
            moved = current(device)?;
            stalls += 1;
        }
        if moved == value {
            // Still put after several nudges: a genuine end stop, or a value the scope
            // will not accept in this mode.
            return Err(Error::Unexpected(format!(
                "{knob:?} stopped at {value} before reaching {target} (end stop or invalid value)"
            )));
        }
        value = moved;
    }
    Err(Error::Unexpected(format!(
        "{knob:?} did not reach {target} within {max_steps} steps (stopped at {value})"
    )))
}

/// Press `key_press` until the scope reports one of `wanted` menu ids.
///
/// Never fires blind: menus are how softkeys acquire meaning, so pressing a softkey
/// without confirming the menu is open would send an unrelated command.
pub fn open_menu(device: &Device, key: crate::device::Key, wanted: &[u8]) -> Result<u8> {
    for _ in 0..MENU_ATTEMPTS {
        device.press(key)?;
        sleep(MENU_SETTLE);
        for _ in 0..MENU_POLLS {
            let menu = device.read_settings()?.menu_id();
            if wanted.contains(&menu) {
                return Ok(menu);
            }
            sleep(Duration::from_millis(200));
        }
    }
    let menu = device.read_settings()?.menu_id();
    Err(Error::Unexpected(format!(
        "menu {wanted:?} did not open (scope is showing menu {menu})"
    )))
}

/// Press `key` until `read` reports `target`, for settings that **cycle** through a ring
/// of values rather than moving up and down (probe attenuation, store depth, trigger type).
///
/// Each press that lands advances one position, so walking the whole ring without finding
/// the target means the value is genuinely unreachable — typically because it is greyed out
/// in the current mode.
///
/// A press that does *not* land is a different matter and must not be counted as a lap. The
/// key mailbox holds a single slot and drops presses when the scope is busy, so a press that
/// moved nothing is far more often a dropped one than an end of the road — and counting it
/// as a lap walks the budget down to zero while the value has not moved at all, failing with
/// "unavailable in this mode" on a value that was perfectly available. Re-press instead,
/// waiting longer each time, and only give up once several in a row have gone nowhere.
pub fn cycle_until(
    device: &Device,
    key: crate::device::Key,
    ring_size: u32,
    read: impl Fn(&Settings) -> Option<i64>,
    target: i64,
) -> Result<i64> {
    let current = |device: &Device| -> Result<Option<i64>> {
        Ok(read(&device.read_settings()?))
    };

    let mut laps = 0;
    let mut stalls = 0;
    loop {
        let before = current(device)?;
        if before == Some(target) {
            return Ok(target);
        }
        device.press(key)?;
        sleep(SETTLE);

        if current(device)? == before {
            stalls += 1;
            if stalls > NONMOVE_RETRIES {
                return Err(Error::Unexpected(format!(
                    "{key:?} moved nothing in {stalls} presses (stuck at {before:?}, wanted \
                     {target}); the menu may not be open"
                )));
            }
            sleep(SETTLE * stalls);
            continue;
        }

        stalls = 0;
        laps += 1;
        if laps > ring_size {
            return Err(Error::Unexpected(format!(
                "cycling {key:?} walked all {ring_size} positions without reaching {target} \
                 (now {:?}); the value may be unavailable in this mode",
                current(device)?
            )));
        }
    }
}
