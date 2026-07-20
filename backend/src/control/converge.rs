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
const NONMOVE_RETRIES: u32 = 3;

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
    let current = |device: &Device| -> Result<i64> {
        let settings = device.read_settings()?;
        read(&settings).ok_or_else(|| {
            Error::Unexpected(format!("{knob:?} value is not readable from the settings block"))
        })
    };

    let mut value = current(device)?;
    for _ in 0..MAX_STEPS {
        if (value - target).abs() <= tolerance {
            return Ok(value);
        }
        let direction = if value < target { Turn::Up } else { Turn::Down };
        device.turn(knob, direction, 1)?;
        sleep(SETTLE);

        // A knob that appears not to have moved is usually a dropped press, not an end
        // stop — the mailbox is single-slot. Re-nudge a few times before concluding it
        // really cannot move, so one lost press does not abort the whole prepare/capture.
        let mut moved = current(device)?;
        let mut stalls = 0;
        while moved == value && stalls < NONMOVE_RETRIES {
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
        "{knob:?} did not reach {target} within {MAX_STEPS} steps (stopped at {value})"
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
/// of values rather than moving up and down (probe attenuation, store depth).
///
/// Each press advances one position, so this walks the ring at most `ring_size` times
/// before concluding the value is unreachable — typically because it is greyed out in the
/// current mode.
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

    for _ in 0..=ring_size {
        if current(device)? == Some(target) {
            return Ok(target);
        }
        device.press(key)?;
        sleep(SETTLE);
    }
    Err(Error::Unexpected(format!(
        "cycling {key:?} did not reach {target} in {ring_size} steps \
         (now {:?}); the value may be unavailable in this mode",
        current(device)?
    )))
}
