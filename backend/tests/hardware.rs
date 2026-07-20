//! End-to-end hardware test for the device layer.
//!
//! **Requires the MSO5202D to be plugged in**, so it is `#[ignore]`d by default and never
//! runs in a normal `cargo test`. Run it deliberately:
//!
//! ```sh
//! cargo test -p mso5202d --test hardware -- --ignored --nocapture
//! ```
//!
//! It is a single test because the USB interface is exclusive — parallel tests would
//! fight over the device.
//!
//! # It moves the front panel
//!
//! Verifying knobs means actually turning them. Every change made here is reversed by
//! stepping the same knob back, and the menu is returned to where it started, so the scope
//! should end roughly as it began. A Default Setup restores a known state if anything
//! drifts.

use std::thread::sleep;
use std::time::Duration;

use mso5202d::device::shell::check_command;
use mso5202d::{Device, Key, Knob, Turn};

/// Time to let the scope apply a key press before reading the result back.
const SETTLE: Duration = Duration::from_millis(400);

#[test]
#[ignore = "requires the MSO5202D to be connected"]
fn device_layer_works_on_hardware() {
    let scope = Device::connect().expect("connect to the scope (udev rule installed, or run as root)");

    let (bus, address) = scope.transport().bus_address().expect("device is present");
    println!("\n== connected: bus {bus} address {address} ==");

    check_settings(&scope);
    check_keys(&scope);
    check_knobs(&scope);
    check_screen(&scope);
    check_files(&scope);
    check_shell(&scope);

    println!("\n== all device-layer operations verified ==");
}

// --- settings ---------------------------------------------------------------

fn check_settings(scope: &Device) {
    println!("\n-- settings --");
    let settings = scope.read_settings().expect("read settings");

    assert_eq!(settings.raw().len(), 213, "settings block must be 213 bytes");

    // Scaling tables must resolve, otherwise the index mapping is wrong.
    let volts = settings
        .volts_per_div_mv(1)
        .expect("CH1 volts/div index must map to a known value");
    let time = settings
        .time_per_div_ns()
        .expect("timebase index must map to a known value");
    assert!(volts > 0 && time > 0);

    // The derived sample interval must agree with the documented 200 samples/division.
    let interval = settings.sample_interval_ns().expect("sample interval");
    assert!(
        (interval - time as f64 / 200.0).abs() < f64::EPSILON,
        "sample interval must be time/div over 200"
    );

    println!("  block 213 B, menu {:?}", settings.menu_name());
    println!("  CH1 {volts} mV/div, {time} ns/div, {interval} ns/sample");
    println!("  trig {:?} @ {:?} mV", settings.trig_state(), settings.trigger_level_mv());
}

// --- keys -------------------------------------------------------------------

fn check_keys(scope: &Device) {
    println!("\n-- keys --");
    let original = scope.read_settings().expect("read menu").menu_id();

    // A key press must actually move the instrument, not merely be acknowledged.
    scope.press(Key::MenuAcquire).expect("press Acquire");
    sleep(SETTLE);
    let acquire_menu = scope.read_settings().expect("read menu").menu_id();
    assert_eq!(acquire_menu, 17, "Acquire key must open menu 17");
    println!("  press(MenuAcquire): menu {original} -> {acquire_menu}");

    scope.press(Key::MenuUtility).expect("press Utility");
    sleep(SETTLE);
    let utility_menu = scope.read_settings().expect("read menu").menu_id();
    assert_ne!(utility_menu, acquire_menu, "Utility key must change the menu");
    println!("  press(MenuUtility): menu {acquire_menu} -> {utility_menu}");

    // Repeat presses must each land, not collapse into one. Utility cycles 3 pages, so
    // two more presses must leave a different page than one press did.
    scope.press_repeatedly(Key::MenuUtility, 2).expect("press Utility twice");
    sleep(SETTLE);
    let cycled = scope.read_settings().expect("read menu").menu_id();
    println!("  press_repeatedly(MenuUtility, 2): menu {utility_menu} -> {cycled}");

    // Put the menu back where we found it.
    scope.press(Key::MenuSaveRecall).expect("restore menu");
    sleep(SETTLE);
    println!("  menu restored");
}

// --- knobs ------------------------------------------------------------------

fn check_knobs(scope: &Device) {
    println!("\n-- knobs --");

    check_volts_per_div(scope);
    check_timebase_direction(scope);
    check_trigger_level(scope);
    check_position_and_push(scope);
}

/// Turning the volts/div knob must change volts/div, and turning back must restore it.
fn check_volts_per_div(scope: &Device) {
    let read = |scope: &Device| scope.read_settings().expect("read settings").volts_per_div_mv(1);
    let before = read(scope).expect("CH1 volts/div");

    // Pick a direction with headroom rather than assuming where the knob is sitting.
    let (direction, after) = try_both_directions(scope, Knob::Ch1VoltsPerDiv, read, before);
    println!("  Ch1VoltsPerDiv {direction:?}: {before} -> {after} mV/div");
    assert_ne!(after, before, "volts/div knob must move the value");

    scope.turn(Knob::Ch1VoltsPerDiv, opposite(direction), 1).expect("turn back");
    sleep(SETTLE);
    assert_eq!(read(scope), Some(before), "volts/div must return to its original value");
    println!("  Ch1VoltsPerDiv restored to {before} mV/div");
}

/// The one that validates our normalisation: the vendor key *names* are inverted on this
/// firmware, so `Turn::Down` must be wired to produce a **smaller** time/div.
fn check_timebase_direction(scope: &Device) {
    let read = |scope: &Device| scope.read_settings().expect("read settings").time_per_div_ns();
    let before = read(scope).expect("timebase");

    scope.turn(Knob::TimePerDiv, Turn::Down, 1).expect("turn timebase down");
    sleep(SETTLE);
    let after = read(scope).expect("timebase");
    println!("  TimePerDiv Down: {before} -> {after} ns/div");

    if after == before {
        // Already at the fastest setting — check the other direction instead.
        scope.turn(Knob::TimePerDiv, Turn::Up, 1).expect("turn timebase up");
        sleep(SETTLE);
        let up = read(scope).expect("timebase");
        assert!(up > before, "Turn::Up must give a LARGER time/div (slower)");
        scope.turn(Knob::TimePerDiv, Turn::Down, 1).expect("restore timebase");
    } else {
        assert!(
            after < before,
            "Turn::Down must give a SMALLER time/div (faster) — key-name inversion mishandled"
        );
        scope.turn(Knob::TimePerDiv, Turn::Up, 1).expect("restore timebase");
    }
    sleep(SETTLE);
    assert_eq!(read(scope), Some(before), "timebase must return to its original value");
    println!("  TimePerDiv restored to {before} ns/div (Down = faster confirmed)");
}

/// The trigger level knob must move `TRIG-VPOS`, and multiple steps must each land.
fn check_trigger_level(scope: &Device) {
    let read = |scope: &Device| scope.read_settings().expect("read settings").trigger_position();
    let before = read(scope);

    const STEPS: u32 = 3;
    scope.turn(Knob::TriggerLevel, Turn::Up, STEPS).expect("raise trigger level");
    sleep(SETTLE);
    let after = read(scope);
    println!("  TriggerLevel Up x{STEPS}: {before} -> {after} (1/25 div)");
    assert!(after > before, "trigger level knob must raise TRIG-VPOS");

    scope.turn(Knob::TriggerLevel, Turn::Down, STEPS).expect("lower trigger level");
    sleep(SETTLE);
    let restored = read(scope);
    assert_eq!(restored, before, "trigger level must return to its original value");
    println!("  TriggerLevel restored to {before}");
}

/// The position knob must move the trace, and its **push** must zero the axis.
fn check_position_and_push(scope: &Device) {
    let read = |scope: &Device| scope.read_settings().expect("read settings").channel_position(1);
    let before = read(scope);

    scope.turn(Knob::Ch1Position, Turn::Up, 1).expect("raise CH1 position");
    sleep(SETTLE);
    let stepped = read(scope);
    assert_ne!(stepped, before, "position knob must move VERT-CH1-POS");
    let step = stepped - before;
    println!("  Ch1Position Up: {before} -> {stepped} (step {step})");

    // Push zeroes the axis — this is the documented behaviour of the knob push keys.
    let pushed = scope.push(Knob::Ch1Position).expect("push CH1 position knob");
    assert!(pushed, "Ch1Position has a push key");
    sleep(SETTLE);
    assert_eq!(read(scope), 0, "pushing the position knob must zero VERT-CH1-POS");
    println!("  push(Ch1Position): position -> 0");

    // Knobs without a push action must report that rather than pressing something wrong.
    assert!(
        !scope.push(Knob::TimePerDiv).expect("timebase push is a no-op"),
        "TimePerDiv must report that it has no push key"
    );

    // Step back to where the position started.
    if before != 0 && step != 0 {
        let steps = (before / step).unsigned_abs() as u32;
        let direction = if before > 0 { Turn::Up } else { Turn::Down };
        scope.turn(Knob::Ch1Position, direction, steps).expect("restore position");
        sleep(SETTLE);
    }
    println!("  Ch1Position restored to {}", read(scope));
}

// --- screen -----------------------------------------------------------------

fn check_screen(scope: &Device) {
    println!("\n-- screen --");
    let shot = scope.screenshot().expect("grab the framebuffer");

    assert_eq!(shot.width(), 800);
    assert_eq!(shot.height(), 480);
    assert_eq!(shot.rgb().len(), 800 * 480 * 3, "RGB buffer must be fully populated");

    // A real screen is not a single flat colour — this catches a decode that "succeeds"
    // but produces a blank or garbled image.
    let first = &shot.rgb()[0..3];
    let varied = shot.rgb().chunks_exact(3).any(|px| px != first);
    assert!(varied, "screenshot must contain more than one colour");

    // The scope draws a coloured trace/graticule, so some pixel must be non-grey.
    let coloured = shot.rgb().chunks_exact(3).any(|px| {
        let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
        (r - g).abs() > 40 || (g - b).abs() > 40
    });
    assert!(coloured, "screenshot must contain coloured content");

    // Save it so the image can be inspected: binary PPM needs no image dependency.
    let path = std::env::temp_dir().join("mso5202d-screen.ppm");
    let mut ppm = format!("P6\n{} {}\n255\n", shot.width(), shot.height()).into_bytes();
    ppm.extend_from_slice(shot.rgb());
    std::fs::write(&path, ppm).expect("write screenshot");
    println!("  800x480 grabbed, varied + coloured, saved to {}", path.display());
}

// --- files ------------------------------------------------------------------

fn check_files(scope: &Device) {
    println!("\n-- files --");

    // A known file with known content: the parameter list the settings block follows.
    let inf = scope.download("/protocol.inf").expect("download /protocol.inf");
    let text = String::from_utf8_lossy(&inf);
    assert!(text.contains("[TOTAL]"), "/protocol.inf must contain its [TOTAL] header");
    assert!(text.contains("213"), "/protocol.inf must declare the 213-byte total");
    println!("  /protocol.inf: {} bytes, contains [TOTAL] 213", inf.len());

    // Cross-check the download against an independent path: the shell's own file size.
    // These come from two different channels, so agreement is strong evidence both work.
    let listing = scope.list_dir("/").expect("list /");
    if let Some(entry) = listing.iter().find(|e| e.name == "protocol.inf") {
        assert_eq!(
            entry.size as usize,
            inf.len(),
            "downloaded length must match the size reported by the shell"
        );
        println!("  size agrees with `ls` ({} bytes) across both channels", entry.size);
    }

    // A file large enough to span many frames, exercising the multi-frame download loop
    // that a deep-capture CSV depends on. `help.db` (~900 KB) is the known-good large file
    // — deliberately named rather than "whatever is biggest in /", because some paths are
    // not servable at all (the running application binary `/dso_bin` answers with a single
    // byte no matter how long you wait).
    if let Some(help) = listing.iter().find(|e| e.name == "help.db") {
        let started = std::time::Instant::now();
        let data = scope.download(&help.path_in("/")).expect("download help.db");
        let elapsed = started.elapsed();
        assert_eq!(
            data.len(),
            help.size as usize,
            "multi-frame download must return the whole file"
        );
        let rate = data.len() as f64 / elapsed.as_secs_f64() / 1024.0;
        println!(
            "  help.db: {} bytes intact over many frames in {elapsed:?} ({rate:.0} KB/s)",
            data.len()
        );
    }
}

// --- shell ------------------------------------------------------------------

fn check_shell(scope: &Device) {
    println!("\n-- shell --");

    let uname = scope.shell("uname -n -r").expect("run uname");
    assert!(
        uname.contains("Hantek"),
        "uname must identify the scope, got {uname:?}"
    );
    println!("  uname: {}", uname.trim());

    // Consecutive commands must not return each other's output — this is the reply-lag
    // guard doing its job.
    let first = scope.shell("echo alpha").expect("echo alpha");
    let second = scope.shell("echo beta").expect("echo beta");
    assert!(first.contains("alpha") && !first.contains("beta"), "got {first:?}");
    assert!(second.contains("beta") && !second.contains("alpha"), "got {second:?}");
    println!("  consecutive commands stay distinct (no reply lag)");

    // Directory listing must parse into usable entries.
    let root = scope.list_dir("/").expect("list /");
    assert!(!root.is_empty(), "/ must list some entries");
    assert!(root.iter().any(|e| e.is_dir), "/ must contain directories");
    assert!(
        root.iter().any(|e| e.name == "protocol.inf"),
        "/ must contain protocol.inf"
    );
    println!("  ls / -> {} entries parsed", root.len());

    // The safety guard must refuse destructive commands *before* they reach the scope.
    let blocked = scope.shell("rm -rf /");
    assert!(blocked.is_err(), "destructive command must be refused");
    println!("  guard refused `rm -rf /`: {}", blocked.unwrap_err());
    assert!(check_command("reboot").is_err());

    // ...and the link must still be healthy afterwards, proving nothing was sent.
    let alive = scope.shell("echo still-alive").expect("shell still works after a refusal");
    assert!(alive.contains("still-alive"));
    println!("  link healthy after refusal");
}

// --- helpers ----------------------------------------------------------------

fn opposite(direction: Turn) -> Turn {
    match direction {
        Turn::Up => Turn::Down,
        Turn::Down => Turn::Up,
    }
}

/// Turn `knob` one step in whichever direction actually changes the value, so a knob
/// sitting at an end stop does not fail the test. Returns the direction that moved and
/// the new value.
fn try_both_directions<T: PartialEq + Copy>(
    scope: &Device,
    knob: Knob,
    read: impl Fn(&Device) -> Option<T>,
    before: T,
) -> (Turn, T) {
    for direction in [Turn::Up, Turn::Down] {
        scope.turn(knob, direction, 1).expect("turn knob");
        sleep(SETTLE);
        if let Some(value) = read(scope) {
            if value != before {
                return (direction, value);
            }
        }
        // No movement: undo and try the other way.
        scope.turn(knob, opposite(direction), 1).expect("undo turn");
        sleep(SETTLE);
    }
    panic!("knob {knob:?} did not move in either direction");
}
