//! Reverse-engineer the trigger menu's softkey map on real hardware.
//!
//! The bezel softkeys have no fixed meaning — what `Fn2` does depends on which menu is
//! open — and the trigger menus' map is not documented anywhere. Rather than guess it, this
//! discovers it: put the scope on a known trigger page, press one softkey, and diff the
//! settings block before and after. Whatever field moved is what that key controls, and the
//! sequence of values it moved through shows whether it cycles a ring or toggles a pair.
//!
//! Navigation is driven by **trigger type**, not by pressing the trigger key and hoping:
//! the trigger key reopens whichever page matches the current type, so a probe that has
//! just cycled the type away can never get back that way — it lands on Edge and silently
//! reports the Edge map for every page.
//!
//! Pressing softkeys inside the trigger menus only ever changes trigger configuration, so
//! this is safe to run repeatedly. It starts from Default Setup so the map is recorded from
//! a known state.
//!
//! ```sh
//! cargo run -p mso5202d --bin trigger_probe             # every trigger type
//! cargo run -p mso5202d --bin trigger_probe -- --type 2 # just Pulse
//! ```

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::Duration;

use mso5202d::control::converge::{MENU_SETTLE, SETTLE};
use mso5202d::device::{Key, Turn};
use mso5202d::settings::{Settings, SETTINGS_PARAMS};
use mso5202d::{logging, Device, Result};

/// The bezel softkeys this probe may press.
///
/// **`Fn7` is excluded and must never be pressed.** On this instrument it opens the
/// dual-window / logic-analyzer view rather than acting as a trigger softkey: it toggles
/// `LA-SWI` and jumps to menu 61, which strands any navigation in progress. Earlier runs
/// aborted after the first type for exactly that reason.
const SOFTKEYS: [Key; 7] = [
    Key::Fn0,
    Key::Fn1,
    Key::Fn2,
    Key::Fn3,
    Key::Fn4,
    Key::Fn5,
    Key::Fn6,
];

/// How many times to press one softkey, to see a ring's whole cycle.
const PRESSES: usize = 6;

/// The softkey that cycles `TRIG-TYPE`, established by the first probe run.
const TYPE_KEY: Key = Key::Fn1;

/// The softkey that advances to a multi-page menu's second page, likewise established.
const PAGE_KEY: Key = Key::Fn6;

/// Trigger types, by `TRIG-TYPE` code.
const TYPES: [(u8, &str); 6] = [
    (0, "Edge"),
    (1, "Video"),
    (2, "Pulse"),
    (3, "Slope"),
    (4, "Overtime"),
    (5, "Alter"),
];

fn main() {
    let _log = logging::init().expect("start logging");
    if let Err(e) = run() {
        eprintln!("\n[trigger_probe] FAILED: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let only: Option<u8> = std::env::args()
        .skip_while(|a| a != "--type")
        .nth(1)
        .and_then(|v| v.parse().ok());
    // Probe the second page of each multi-page type instead of the first.
    let page2 = std::env::args().any(|a| a == "--page2");
    // Softkeys that only *select* a parameter leave the blob untouched; the multipurpose
    // knob is what then changes it. In this mode each softkey is pressed once and the knob
    // turned, so those parameters reveal themselves.
    let knob = std::env::args().any(|a| a == "--knob");
    // Walk the trigger key from a closed menu, logging which page each press lands on, then
    // probe whatever page is showing. This is how a page that only appears on a particular
    // route — menu 11, the trigger base — gets mapped at all.
    let walk = std::env::args().any(|a| a == "--walk");
    // Map a second page properly: a softkey there either navigates away or hands the
    // multipurpose knob a parameter. The earlier probe conflated the two — it pressed on,
    // and reported knob movement that had happened back on page 1.
    let page2map = std::env::args().any(|a| a == "--page2map");
    // Does the trigger level knob mean anything in this trigger type?
    let levelmap = std::env::args().any(|a| a == "--levelmap");
    // Chase the parameters still missing from page 2: is there a third page behind the
    // page key, and does pushing the multipurpose knob hand it a different parameter?
    let deep = std::env::args().any(|a| a == "--deep");
    // Press each page-2 softkey a *number* of times before turning the knob. A key that
    // toggles which parameter the knob owns (V1 → V2) is indistinguishable from one that
    // merely selects the first, if it is only ever pressed once.
    let repeat = std::env::args().any(|a| a == "--repeat");
    // Grab the scope's own screen at each stop. Softkey *labels* answer in one look what a
    // blob diff can only infer — and they name the keys that move no field at all, which is
    // precisely where a diff-only probe goes blind.
    let shots = std::env::args().any(|a| a == "--shots");
    // Overtime's time sits on page *1* (the screen shows an "Overtime 500ns" box there),
    // so the page-2 sweep could never find it.
    let otime = std::env::args().any(|a| a == "--otime");
    // Map the Alter (alternating) tree: menu 24 plus its per-channel sub-pages 26-33.
    let alter = std::env::args().any(|a| a == "--alter");
    // Check specific claims one operation at a time, screenshotting after each so the
    // scope's own display can be read rather than inferred from the settings blob.
    let verify = std::env::args().any(|a| a == "--verify");
    // What the level's volts conversion actually has to work with.
    let scale = std::env::args().any(|a| a == "--scale");
    // How big is one step of the trigger level knob, and does it depend on volts/division?
    let levelstep = std::env::args().any(|a| a == "--levelstep");
    // How does the multipurpose knob step a continuous value — fixed, or scaled to it?
    let knobstep = std::env::args().any(|a| a == "--knobstep");
    // What every knob-only value reads after a factory Default Setup — the state Prepare
    // always starts from, and therefore the value a target is walked from.
    let defaults = std::env::args().any(|a| a == "--defaults");
    // Does an Alter channel have its own trigger level, and does the level knob reach it?
    let alterlevel = std::env::args().any(|a| a == "--alterlevel");

    // Card-safe connect, as everywhere else: a USB reset disturbs the scope's own host
    // controller.
    let scope = Device::connect_without_reset()?;
    scope.transport().resync();
    println!("[trigger_probe] connected");

    scope.press(Key::DefaultSetup)?;
    sleep(Duration::from_millis(1500));
    scope.clear_link();

    if walk {
        return walk_trigger_key(&scope);
    }
    if page2map {
        return map_second_pages(&scope);
    }
    if levelmap {
        return map_level_knob(&scope);
    }
    if deep {
        return probe_deeper(&scope);
    }
    if repeat {
        return probe_repeated_presses(&scope);
    }
    if shots {
        return shoot_pages(&scope);
    }
    if alter {
        return map_alter(&scope);
    }
    if verify {
        return verify_claims(&scope);
    }
    if std::env::args().any(|a| a == "--altertrack") {
        use mso5202d::control::trigger::{
            self as t, AlterChannel, AlterType, TriggerSetup, TriggerType,
        };
        t::apply(
            &scope,
            &TriggerSetup {
                kind: TriggerType::Alter,
                alter_ch1: AlterChannel { kind: AlterType::Pulse, ..Default::default() },
                alter_ch2: AlterChannel { kind: AlterType::Overtime, ..Default::default() },
                ..Default::default()
            },
        )?;
        // Put a distinct level on whichever channel is current, then watch both fields as
        // the alternation runs: if TRIG-VPOS is per-channel it will flip with TRIG-SRC.
        for _ in 0..5 {
            scope.press(Key::TriggerLevelUp)?;
            sleep(SETTLE);
        }
        println!("polling TRIG-SRC / TRIG-VPOS while alternating:");
        for _ in 0..25 {
            let st = scope.read_settings()?;
            println!(
                "  src {:?}  vpos {:>4}  level_mv {:?}",
                st.field("TRIG-SRC"),
                st.trigger_position(),
                st.trigger_level_mv()
            );
            sleep(std::time::Duration::from_millis(250));
        }
        return Ok(());
    }
    // Walk the Alter knob-value path one step at a time — Alter → channel → softkey → V0 —
    // capturing the screen and the fields after each step, so every step can be checked
    // rather than inferred from the end state.
    if std::env::args().any(|a| a == "--altersteps") {
        use mso5202d::control::trigger::{
            self as t, AlterChannel, AlterType, TriggerSetup, TriggerType,
        };
        use mso5202d::device::Knob;
        let dir = std::env::args()
            .skip_while(|a| a != "--altersteps")
            .nth(1)
            .filter(|a| !a.starts_with("--"))
            .unwrap_or_else(|| ".".into());

        for (sub_type, key, field, tag) in [
            (AlterType::Pulse, Key::Fn4, "TRIG-SWAP-CH1-PULSE-TIME", "pulse"),
            (AlterType::Overtime, Key::Fn3, "TRIG-SWAP-CH1-OVERTIME-TIME", "ot"),
        ] {
            println!("\n=== Alter → CH1 {} → {key:?} → V0 ===", sub_type.name());
            let report = |step: &str| -> Result<()> {
                let st = scope.read_settings()?;
                println!(
                    "  {step:<22} menu {:>2}  CH1-TYPE {:?}  {field} = {:?}  VPOS {}",
                    st.menu_id(),
                    st.field("TRIG-SWAP-CH1-TYPE"),
                    st.field_signed(field),
                    st.trigger_position()
                );
                let shot = scope.screenshot()?;
                std::fs::write(format!("{dir}/steps-{tag}-{step}.rgb"), shot.rgb())
                    .map_err(|e| mso5202d::Error::Unexpected(e.to_string()))?;
                Ok(())
            };

            // 1. Alter, with CH1 on the sub-type that owns the value.
            t::apply(
                &scope,
                &TriggerSetup {
                    kind: TriggerType::Alter,
                    alter_ch1: AlterChannel { kind: sub_type, ..Default::default() },
                    ..Default::default()
                },
            )?;
            report("1-alter")?;

            // 2. Open CH1's own page.
            for _ in 0..4 {
                if (26..=29).contains(&scope.read_settings()?.menu_id()) {
                    break;
                }
                scope.press(Key::Fn2)?;
                sleep(MENU_SETTLE);
            }
            report("2-ch1page")?;

            // 3. The softkey that hands V0 this value.
            scope.press(key)?;
            sleep(SETTLE);
            report("3-softkey")?;

            // 4. V0 itself.
            for _ in 0..3 {
                scope.turn(Knob::Multipurpose, Turn::Up, 1)?;
                sleep(SETTLE);
            }
            report("4-after-v0")?;
        }
        return Ok(());
    }
    // On each Alter channel page, turn the *trigger level* knob and read the two per-channel
    // readouts off the screen — the blob has only one TRIG-VPOS, so the screen is the only
    // place the other level is visible.
    if std::env::args().any(|a| a == "--alterlevels") {
        use mso5202d::control::trigger::{
            self as t, AlterChannel, AlterType, TriggerSetup, TriggerType,
        };
        let dir = std::env::args()
            .skip_while(|a| a != "--alterlevels")
            .nth(1)
            .filter(|a| !a.starts_with("--"))
            .unwrap_or_else(|| ".".into());
        t::apply(
            &scope,
            &TriggerSetup {
                kind: TriggerType::Alter,
                alter_ch1: AlterChannel { kind: AlterType::Pulse, ..Default::default() },
                alter_ch2: AlterChannel { kind: AlterType::Overtime, ..Default::default() },
                ..Default::default()
            },
        )?;
        for (channel, open_key) in [(1u8, Key::Fn2), (2u8, Key::Fn3)] {
            for _ in 0..5 {
                let menu = scope.read_settings()?.menu_id();
                if (26..=33).contains(&menu) {
                    break;
                }
                scope.press(open_key)?;
                sleep(MENU_SETTLE);
            }
            let shot = scope.screenshot()?;
            std::fs::write(format!("{dir}/lv-ch{channel}-before.rgb"), shot.rgb())
                .map_err(|e| mso5202d::Error::Unexpected(e.to_string()))?;
            let before = scope.read_settings()?;
            for _ in 0..6 {
                scope.press(Key::TriggerLevelUp)?;
                sleep(SETTLE);
            }
            let after = scope.read_settings()?;
            let shot = scope.screenshot()?;
            std::fs::write(format!("{dir}/lv-ch{channel}-after.rgb"), shot.rgb())
                .map_err(|e| mso5202d::Error::Unexpected(e.to_string()))?;
            println!(
                "CH{channel} page (menu {}): TRIG-SRC {:?}→{:?}  TRIG-VPOS {}→{}",
                after.menu_id(),
                before.field("TRIG-SRC"),
                after.field("TRIG-SRC"),
                before.trigger_position(),
                after.trigger_position()
            );
            for _ in 0..4 {
                if scope.read_settings()?.menu_id() == 24 {
                    break;
                }
                scope.press(Key::Fn6)?;
                sleep(MENU_SETTLE);
            }
        }
        return Ok(());
    }
    if alterlevel {
        use mso5202d::control::trigger::{
            self as t, AlterChannel, AlterType, TriggerSetup, TriggerType,
        };
        let dir = std::env::args()
            .skip_while(|a| a != "--alterlevel")
            .nth(1)
            .filter(|a| !a.starts_with("--"))
            .unwrap_or_else(|| ".".into());
        t::apply(
            &scope,
            &TriggerSetup {
                kind: TriggerType::Alter,
                alter_ch1: AlterChannel { kind: AlterType::Pulse, ..Default::default() },
                alter_ch2: AlterChannel { kind: AlterType::Overtime, ..Default::default() },
                ..Default::default()
            },
        )?;
        for (channel, key) in [(1u8, Key::Fn2), (2u8, Key::Fn3)] {
            for _ in 0..4 {
                let menu = scope.read_settings()?.menu_id();
                if (26..=33).contains(&menu) {
                    break;
                }
                scope.press(key)?;
                sleep(MENU_SETTLE);
            }
            let menu = scope.read_settings()?.menu_id();
            // Stop the acquisition first: while it runs, the alternation keeps moving
            // TRIG-SRC, and the level knob follows whichever channel is being serviced.
            if std::env::args().any(|a| a == "--stopped") {
                for _ in 0..4 {
                    let state = scope.read_settings()?.trig_state();
                    if matches!(
                        state,
                        mso5202d::TrigState::Stop | mso5202d::TrigState::SingleCaptured
                    ) {
                        break;
                    }
                    scope.press(Key::RunStop)?;
                    sleep(SETTLE);
                }
            }
            let before = scope.read_settings()?;
            println!(
                "  CH{channel} before: TRIG-SRC {:?} state {:?}",
                before.field("TRIG-SRC"),
                before.trig_state()
            );
            for _ in 0..3 {
                scope.press(Key::TriggerLevelUp)?;
                sleep(SETTLE);
            }
            let after = scope.read_settings()?;
            println!(
                "CH{channel} (menu {menu}): TRIG-VPOS {} → {}   level_mv {:?} → {:?}   other: {:?}",
                before.trigger_position(),
                after.trigger_position(),
                before.trigger_level_mv(),
                after.trigger_level_mv(),
                diff(&before, &after)
                    .iter()
                    .filter(|(f, _)| f != "TRIG-VPOS")
                    .map(|(f, v)| format!("{f} {v:?}"))
                    .collect::<Vec<_>>()
            );
            let shot = scope.screenshot()?;
            std::fs::write(format!("{dir}/alterlevel-ch{channel}.rgb"), shot.rgb())
                .map_err(|e| mso5202d::Error::Unexpected(e.to_string()))?;
            for _ in 0..4 {
                if scope.read_settings()?.menu_id() == 24 {
                    break;
                }
                scope.press(Key::Fn6)?;
                sleep(MENU_SETTLE);
            }
        }
        return Ok(());
    }
    if defaults {
        scope.press(Key::DefaultSetup)?;
        sleep(std::time::Duration::from_millis(2000));
        scope.clear_link();
        let st = scope.read_settings()?;
        for field in [
            "TRIG-PULSE-TIME",
            "TRIG-SLOPE-V1",
            "TRIG-SLOPE-V2",
            "TRIG-SLOPE-TIME",
            "TRIG-OVERTIME-TIME",
            "TRIG-VIDEO-LINE",
            "TRIG-SWAP-CH1-PULSE-TIME",
            "TRIG-SWAP-CH1-OVERTIME-TIME",
            "TRIG-SWAP-CH1-VIDEO-LINE",
            "TRIG-SWAP-CH2-PULSE-TIME",
            "TRIG-SWAP-CH2-OVERTIME-TIME",
            "TRIG-SWAP-CH2-VIDEO-LINE",
        ] {
            println!("{field:<30} {:?}", st.field_signed(field));
        }
        return Ok(());
    }
    if knobstep {
        use mso5202d::control::trigger::{self as t, Adjustable, TriggerSetup, TriggerType};
        use mso5202d::device::Knob;
        t::apply(&scope, &TriggerSetup { kind: TriggerType::Pulse, ..Default::default() })?;
        // Land the knob on the pulse width, then walk it a long way in each direction.
        t::nudge(&scope, TriggerType::Pulse, Adjustable::PulseWidth, Turn::Up, 0)?;
        goto_type(&scope, 2)?;
        scope.press(PAGE_KEY)?;
        sleep(SETTLE);
        scope.press(Key::Fn5)?;
        sleep(SETTLE);
        // How fast can presses be fired and still all land? The step is a fixed 10 ns, so a
        // run of N presses must move the value by exactly N × 10 ns — anything less is a
        // press the single-slot key mailbox dropped.
        const STEP_PS: i64 = 10_000;
        for delay_ms in [200u64, 120, 80, 50, 30, 20] {
            let before = scope.read_settings()?.field("TRIG-PULSE-TIME").unwrap_or(0) as i64;
            let presses = 40i64;
            for _ in 0..presses {
                scope.turn(Knob::Multipurpose, Turn::Up, 1)?;
                sleep(std::time::Duration::from_millis(delay_ms));
            }
            sleep(SETTLE);
            let after = scope.read_settings()?.field("TRIG-PULSE-TIME").unwrap_or(0) as i64;
            let landed = (after - before) / STEP_PS;
            println!(
                "  {delay_ms:>3} ms between presses: {landed}/{presses} landed{}",
                if landed == presses { "  ok" } else { "  DROPPED" }
            );
        }
        return Ok(());
    }
    if levelstep {
        use mso5202d::device::Knob;
        for volts_index in [3u32, 5, 7] {
            // Walk CH1's volts/division to a few different settings and measure there.
            let mut settings = scope.read_settings()?;
            let mut guard = 0;
            while settings.field("VERT-CH1-VB") != Some(u64::from(volts_index)) && guard < 20 {
                let direction = if settings.field("VERT-CH1-VB").unwrap_or(0) < u64::from(volts_index) {
                    mso5202d::Turn::Up
                } else {
                    mso5202d::Turn::Down
                };
                scope.turn(Knob::Ch1VoltsPerDiv, direction, 1)?;
                sleep(SETTLE);
                settings = scope.read_settings()?;
                guard += 1;
            }
            let mv = settings.volts_per_div_mv(1);
            let mut steps = Vec::new();
            let mut previous = settings.trigger_position();
            for _ in 0..5 {
                scope.press(Key::TriggerLevelUp)?;
                sleep(SETTLE);
                let now = scope.read_settings()?.trigger_position();
                steps.push(now - previous);
                previous = now;
            }
            println!("volts/div {mv:?} mV: level steps {steps:?}");
        }
        return Ok(());
    }
    if scale {
        let st = scope.read_settings()?;
        for ch in [1u8, 2] {
            println!(
                "CH{ch}: VERT-CH{ch}-VB = {:?}  volts_per_div_mv = {:?}  position = {}",
                st.field(&format!("VERT-CH{ch}-VB")),
                st.volts_per_div_mv(ch),
                st.channel_position(ch)
            );
        }
        println!(
            "TRIG-SRC = {:?}  TRIG-VPOS = {}  trigger_level_mv = {:?}",
            st.field("TRIG-SRC"),
            st.trigger_position(),
            st.trigger_level_mv()
        );
        return Ok(());
    }
    if otime {
        goto_type(&scope, 4)?;
        for key in [Key::Fn2, Key::Fn3, Key::Fn4, Key::Fn5] {
            goto_type(&scope, 4)?;
            scope.press(key)?;
            sleep(SETTLE);
            let before = scope.read_settings()?;
            let mut moves: BTreeMap<String, Vec<i64>> = BTreeMap::new();
            let mut previous = before;
            for _ in 0..2 {
                scope.press(Key::MultiRight)?;
                sleep(SETTLE);
                let after = scope.read_settings()?;
                for (field, values) in diff(&previous, &after) {
                    moves.entry(field).or_default().push(values.1);
                }
                previous = after;
            }
            println!(
                "  overtime page 1 {key:?} + knob: {}",
                if moves.is_empty() {
                    "nothing".to_string()
                } else {
                    moves
                        .iter()
                        .map(|(f, v)| format!("{f} {v:?}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                }
            );
        }
        return Ok(());
    }

    for (code, name) in TYPES {
        if only.is_some_and(|t| t != code) {
            continue;
        }
        let menu = match reach(&scope, code, page2) {
            Ok(menu) => menu,
            Err(e) => {
                println!("\n=== {name} (type {code}) — could not reach: {e}");
                continue;
            }
        };
        let which = if page2 { " page 2" } else { "" };
        println!("\n=== {name} (type {code}){which} — menu {menu} ===");
        for key in SOFTKEYS {
            // The type key is already known, and cycling it would leave the page.
            if key == TYPE_KEY {
                println!("  {key:?}: (type selector — skipped)");
                continue;
            }
            if page2 && key == PAGE_KEY {
                println!("  {key:?}: (page key — skipped)");
                continue;
            }
            let outcome = if knob {
                probe_knob(&scope, code, key, page2)
            } else {
                probe_key(&scope, code, menu, key, page2)
            };
            match outcome {
                Ok(()) => {}
                Err(e) => println!("  {key:?}: probe failed — {e}"),
            }
        }
    }
    Ok(())
}

/// Verify, one operation at a time, which knob owns what.
fn verify_claims(scope: &Device) -> Result<()> {
    let dir = std::env::args()
        .skip_while(|a| a != "--verify")
        .nth(1)
        .filter(|a| !a.starts_with("--"))
        .unwrap_or_else(|| ".".into());
    let shoot = |tag: &str| -> Result<()> {
        let shot = scope.screenshot()?;
        std::fs::write(format!("{dir}/verify-{tag}.rgb"), shot.rgb())
            .map_err(|e| mso5202d::Error::Unexpected(e.to_string()))?;
        Ok(())
    };
    // Turn a knob a few times and report every field that moved.
    let turn = |label: &str, key: Key| -> Result<()> {
        let before = scope.read_settings()?;
        for _ in 0..3 {
            scope.press(key)?;
            sleep(SETTLE);
        }
        let after = scope.read_settings()?;
        let moved = diff(&before, &after);
        println!(
            "    {label}: {}",
            if moved.is_empty() {
                "nothing moved".to_string()
            } else {
                moved
                    .iter()
                    .map(|(f, (a, b))| format!("{f} {a}→{b}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        Ok(())
    };

    // --- 1. Video with Sync = LineNum: which knob sets the line number? -------
    println!("\n[1] Video / Sync = LineNum");
    goto_type(scope, 1)?;
    // Fn5 is the Sync box; cycle it to LineNum (code 1).
    for _ in 0..6 {
        if scope.read_settings()?.field("TRIG-VIDEO-SYN") == Some(1) {
            break;
        }
        scope.press(Key::Fn5)?;
        sleep(SETTLE);
    }
    println!("    TRIG-VIDEO-SYN = {:?}", scope.read_settings()?.field("TRIG-VIDEO-SYN"));
    shoot("video-linenum")?;
    turn("trigger level knob", Key::TriggerLevelUp)?;
    turn("multipurpose knob", Key::MultiRight)?;
    shoot("video-linenum-after")?;

    // --- 2. Overtime: is there a trigger level? ------------------------------
    println!("\n[2] Overtime");
    goto_type(scope, 4)?;
    shoot("overtime")?;
    turn("trigger level knob", Key::TriggerLevelUp)?;
    shoot("overtime-after-level")?;

    // --- 2b. Drive the overtime time through the driver, and look at the screen ---
    println!("\n[2b] Overtime — nudging the time through the driver");
    {
        use mso5202d::control::trigger::{self as t, Adjustable, TriggerSetup, TriggerType};
        t::apply(scope, &TriggerSetup { kind: TriggerType::Overtime, ..Default::default() })?;
        shoot("ot-before")?;
        t::nudge(scope, TriggerType::Overtime, Adjustable::OvertimeTime, Turn::Up, 4)?;
        shoot("ot-after")?;
        println!(
            "    TRIG-OVERTIME-TIME = {:?}",
            scope.read_settings()?.field("TRIG-OVERTIME-TIME")
        );
    }

    // --- 2c. Drive the video line number through the driver ---------------------
    println!("\n[2c] Video / LineNum — nudging the line through the driver");
    {
        use mso5202d::control::trigger::{
            self as t, Adjustable, TriggerSetup, TriggerType, VideoSync,
        };
        t::apply(
            scope,
            &TriggerSetup {
                kind: TriggerType::Video,
                video_sync: VideoSync::LineNumber,
                ..Default::default()
            },
        )?;
        shoot("videoline-before")?;
        t::nudge(scope, TriggerType::Video, Adjustable::VideoLine, Turn::Up, 5)?;
        shoot("videoline-after")?;
        let st = scope.read_settings()?;
        println!(
            "    TRIG-VIDEO-LINE = {:?}  TRIG-VIDEO-SYN = {:?}",
            st.field("TRIG-VIDEO-LINE"),
            st.field("TRIG-VIDEO-SYN")
        );
    }

    // --- 3. Alter / Video / LineNum: which knob sets the line number? --------
    println!("\n[3] Alter / CH1 Video / Sync = LineNum");
    goto_type(scope, 5)?;
    scope.press(Key::Fn2)?; // CH1
    sleep(MENU_SETTLE);
    for _ in 0..6 {
        if scope.read_settings()?.field("TRIG-SWAP-CH1-TYPE") == Some(1) {
            break;
        }
        scope.press(Key::Fn1)?;
        sleep(SETTLE);
    }
    // Fn4/Fn5 are the Sync scroll on the Alter video page.
    for _ in 0..6 {
        if scope.read_settings()?.field("TRIG-SWAP-CH1-VIDEO-SYN") == Some(1) {
            break;
        }
        scope.press(Key::Fn4)?;
        sleep(SETTLE);
    }
    println!(
        "    TRIG-SWAP-CH1-VIDEO-SYN = {:?}  menu {}",
        scope.read_settings()?.field("TRIG-SWAP-CH1-VIDEO-SYN"),
        scope.read_settings()?.menu_id()
    );
    shoot("alter-video-linenum")?;
    turn("multipurpose knob", Key::MultiRight)?;
    turn("trigger level knob", Key::TriggerLevelUp)?;
    shoot("alter-video-linenum-after")?;

    Ok(())
}

/// Map the Alter tree: the base page and every sub-page it opens.
///
/// Alter runs a separate trigger per channel, so its settings live in `TRIG-SWAP-CH1-*` and
/// `TRIG-SWAP-CH2-*` rather than the main `TRIG-*` fields. Each channel gets its own type,
/// and each (channel, type) pair its own menu id.
fn map_alter(scope: &Device) -> Result<()> {
    let dir = std::env::args()
        .skip_while(|a| a != "--alter")
        .nth(1)
        .filter(|a| !a.starts_with("--"))
        .unwrap_or_else(|| ".".into());

    let shoot = |tag: &str| -> Result<()> {
        let shot = scope.screenshot()?;
        let path = format!("{dir}/alter-{tag}.rgb");
        std::fs::write(&path, shot.rgb())
            .map_err(|e| mso5202d::Error::Unexpected(format!("could not write {path}: {e}")))?;
        println!("    screen → {path}");
        Ok(())
    };

    goto_type(scope, 5)?;
    let base = scope.read_settings()?.menu_id();
    println!("=== Alter base — menu {base} ===");
    shoot("base")?;

    // Which softkeys open sub-pages, and which change something in place.
    for key in [Key::Fn2, Key::Fn3, Key::Fn4, Key::Fn5] {
        goto_type(scope, 5)?;
        let entry = scope.read_settings()?;
        scope.press(key)?;
        sleep(SETTLE);
        let after = scope.read_settings()?;
        let menu = after.menu_id();
        let moved = diff(&entry, &after);
        println!(
            "  {key:?}: menu {base} → {menu}   {}",
            if moved.is_empty() {
                "no field moved".to_string()
            } else {
                moved
                    .iter()
                    .map(|(f, (a, b))| format!("{f} {a}→{b}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        if menu != base {
            shoot(&format!("{key:?}").to_lowercase())?;

            // Is the bottom-slot "Back" button `Fn6`, where other pages put the page turn?
            let before_back = scope.read_settings()?.menu_id();
            scope.press(Key::Fn6)?;
            sleep(SETTLE);
            println!("      Fn6 (Back?): menu {before_back} → {}", scope.read_settings()?.menu_id());
            scope.press(key)?;
            sleep(SETTLE);

            // Each sub-type gets its own page; capture the layout of every one.
            for step in 0..4 {
                let here = scope.read_settings()?.menu_id();
                shoot(&format!("{}-sub{here}", format!("{key:?}").to_lowercase()))?;
                if step < 3 {
                    scope.press(Key::Fn1)?;
                    sleep(SETTLE);
                }
            }
            scope.press(Key::Fn1)?;
            sleep(SETTLE);

            // Inside the sub-page: press each key repeatedly, since a selector that steps
            // through a box looks like a plain selection when pressed once.
            for sub in [Key::Fn1, Key::Fn2, Key::Fn3, Key::Fn4, Key::Fn5] {
                let before = scope.read_settings()?;
                let mut seen: BTreeMap<String, Vec<i64>> = BTreeMap::new();
                let mut previous = before;
                let mut menus = Vec::new();
                for _ in 0..4 {
                    scope.press(sub)?;
                    sleep(SETTLE);
                    let now = scope.read_settings()?;
                    for (f, v) in diff(&previous, &now) {
                        seen.entry(f).or_default().push(v.1);
                    }
                    if !menus.contains(&now.menu_id()) {
                        menus.push(now.menu_id());
                    }
                    previous = now;
                }
                println!(
                    "      {sub:?}: {}  [menus {menus:?}]",
                    if seen.is_empty() {
                        "nothing".to_string()
                    } else {
                        seen.iter()
                            .map(|(f, v)| format!("{f} {v:?}"))
                            .collect::<Vec<_>>()
                            .join("; ")
                    }
                );
            }
        }
    }
    Ok(())
}

/// Save the scope's screen for each trigger page, as raw RGB next to a `.dims` file.
///
/// Written raw rather than encoded: the driver has no image encoder, and anything that can
/// read the pixels can read a width, a height and three bytes per pixel.
fn shoot_pages(scope: &Device) -> Result<()> {
    let dir = std::env::args()
        .skip_while(|a| a != "--shots")
        .nth(1)
        .unwrap_or_else(|| ".".into());

    for (code, name) in TYPES {
        for (page2, suffix) in [(false, "p1"), (true, "p2")] {
            if reach(scope, code, page2).is_err() {
                continue;
            }
            let shot = scope.screenshot()?;
            let path = format!("{dir}/trigger-{}-{suffix}.rgb", name.to_lowercase());
            std::fs::write(&path, shot.rgb()).map_err(|e| {
                mso5202d::Error::Unexpected(format!("could not write {path}: {e}"))
            })?;
            println!("{path}  {}x{}", shot.width(), shot.height());
        }
    }
    Ok(())
}

/// Press each page-2 softkey `n` times, then turn the knob, for n = 1..4.
///
/// This is what a single press cannot show: a selector that steps through the parameters a
/// page owns looks exactly like one that selects the first of them.
fn probe_repeated_presses(scope: &Device) -> Result<()> {
    for (code, name) in [(2u8, "Pulse"), (3, "Slope"), (4, "Overtime")] {
        println!("\n=== {name} page 2 — repeated presses ===");
        for key in [Key::Fn2, Key::Fn3, Key::Fn4, Key::Fn5] {
            for presses in 1..=4u32 {
                if reach(scope, code, true).is_err() {
                    println!("  {key:?} ×{presses}: could not reach page 2");
                    continue;
                }
                for _ in 0..presses {
                    scope.press(key)?;
                    sleep(SETTLE);
                }
                let menu = scope.read_settings()?.menu_id();
                let before = scope.read_settings()?;
                let mut moves: BTreeMap<String, Vec<i64>> = BTreeMap::new();
                let mut previous = before;
                for _ in 0..2 {
                    scope.press(Key::MultiRight)?;
                    sleep(SETTLE);
                    let after = scope.read_settings()?;
                    for (field, values) in diff(&previous, &after) {
                        moves.entry(field).or_default().push(values.1);
                    }
                    previous = after;
                }
                let summary = if moves.is_empty() {
                    "knob: nothing".to_string()
                } else {
                    moves
                        .iter()
                        .map(|(field, values)| format!("knob: {field} {values:?}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                };
                println!("  {key:?} ×{presses} [menu {menu}]: {summary}");
            }
        }
    }
    Ok(())
}

/// Look for the parameters page 2's softkeys do not reach: `TRIG-SLOPE-V2`, `-WIN`, `-TIME`
/// and `TRIG-OVERTIME-TIME`.
///
/// Two candidates: a third page behind the page key, and the multipurpose knob's **push**,
/// which on many instruments steps the knob between the parameters a page owns.
fn probe_deeper(scope: &Device) -> Result<()> {
    for (code, name) in [(2u8, "Pulse"), (3, "Slope"), (4, "Overtime")] {
        let page = match reach(scope, code, true) {
            Ok(page) => page,
            Err(e) => {
                println!("\n=== {name} — page 2 unreachable: {e}");
                continue;
            }
        };
        println!("\n=== {name} — from page 2 (menu {page}) ===");

        // A third page?
        scope.press(PAGE_KEY)?;
        sleep(SETTLE);
        let next = scope.read_settings()?.menu_id();
        println!("  page key again → menu {next}");

        // The trigger level knob, while page 2 is open. In Slope it drives no `TRIG-VPOS`
        // at all (measured), so it may instead own the thresholds — which would be where
        // V2 and the V1/V2/Both selector live.
        reach(scope, code, true)?;
        for (label, key) in [
            ("level +", Key::TriggerLevelUp),
            ("level −", Key::TriggerLevelDown),
            ("level push", Key::TriggerLevelZero),
        ] {
            let before = scope.read_settings()?;
            for _ in 0..2 {
                scope.press(key)?;
                sleep(SETTLE);
            }
            let after = scope.read_settings()?;
            let moved = diff(&before, &after);
            println!(
                "  {label}: {}  [menu {}]",
                if moved.is_empty() {
                    "nothing".to_string()
                } else {
                    moved
                        .iter()
                        .map(|(f, (a, b))| format!("{f} {a}→{b}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
                after.menu_id()
            );
        }

        // The knob push, repeatedly: after each push, turn and see what moved.
        reach(scope, code, true)?;
        for push in 0..4 {
            scope.press(Key::MultiPush)?;
            sleep(SETTLE);
            let before = scope.read_settings()?;
            let mut moves: BTreeMap<String, Vec<i64>> = BTreeMap::new();
            let mut previous = before;
            for _ in 0..2 {
                scope.press(Key::MultiRight)?;
                sleep(SETTLE);
                let after = scope.read_settings()?;
                for (field, values) in diff(&previous, &after) {
                    moves.entry(field).or_default().push(values.1);
                }
                previous = after;
            }
            let summary = if moves.is_empty() {
                "knob moves nothing".to_string()
            } else {
                moves
                    .iter()
                    .map(|(field, values)| format!("{field} {values:?}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            println!("  push {push} → {summary}  [menu {}]", scope.read_settings()?.menu_id());
        }
    }
    Ok(())
}

/// Map each second page, separating navigation keys from knob-assignment keys.
fn map_second_pages(scope: &Device) -> Result<()> {
    for (code, name) in TYPES {
        let page = match reach(scope, code, true) {
            Ok(page) => page,
            Err(e) => {
                println!("\n=== {name} page 2 — unreachable: {e}");
                continue;
            }
        };
        println!("\n=== {name} page 2 — menu {page} ===");
        for key in SOFTKEYS {
            if key == TYPE_KEY || key == PAGE_KEY {
                continue;
            }
            if let Err(e) = reach(scope, code, true) {
                println!("  {key:?}: could not get back to the page — {e}");
                continue;
            }
            let entry = scope.read_settings()?;
            scope.press(key)?;
            sleep(SETTLE);

            let landed = scope.read_settings()?.menu_id();
            if landed != page {
                println!("  {key:?}: navigates to menu {landed}");
                continue;
            }
            // Report what the press itself did, separately from what the knob then does.
            // A key can be both: on these pages a selector cycles which threshold is live
            // *and* points the knob at it, and conflating the two hides the selector.
            let pressed = diff(&entry, &scope.read_settings()?);
            let before = scope.read_settings()?;
            let mut moves: BTreeMap<String, Vec<i64>> = BTreeMap::new();
            let mut previous = before;
            for _ in 0..3 {
                scope.press(Key::MultiRight)?;
                sleep(SETTLE);
                let after = scope.read_settings()?;
                for (field, values) in diff(&previous, &after) {
                    moves.entry(field).or_default().push(values.1);
                }
                previous = after;
            }
            let press_note = if pressed.is_empty() {
                "press: nothing".to_string()
            } else {
                format!(
                    "press: {}",
                    pressed
                        .iter()
                        .map(|(field, (a, b))| format!("{field} {a}→{b}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let knob_note = if moves.is_empty() {
                "knob: nothing".to_string()
            } else {
                moves
                    .iter()
                    .map(|(field, values)| format!("knob: {field} {values:?}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            println!("  {key:?}: {press_note}  |  {knob_note}");
        }
    }
    Ok(())
}

/// Ask whether the trigger *level* knob does anything in each trigger type.
///
/// Edge triggers on one level, but Slope compares against two thresholds and Pulse and
/// Overtime qualify on time — so the panel's level knob need not drive `TRIG-VPOS` at all
/// outside Edge, and a driver that converges on it would spin against an unmoving value.
fn map_level_knob(scope: &Device) -> Result<()> {
    for (code, name) in TYPES {
        if goto_type(scope, code).is_err() {
            println!("{name}: could not reach");
            continue;
        }
        let before = scope.read_settings()?;
        for _ in 0..3 {
            scope.press(Key::TriggerLevelUp)?;
            sleep(SETTLE);
        }
        let after = scope.read_settings()?;
        let moved = diff(&before, &after);
        println!(
            "{name:<9} TRIG-VPOS {} → {}   other fields moved: {:?}",
            before.trigger_position(),
            after.trigger_position(),
            moved
                .iter()
                .filter(|(f, _)| f != "TRIG-VPOS")
                .map(|(f, v)| format!("{f} {v:?}"))
                .collect::<Vec<_>>()
        );
    }
    Ok(())
}

/// Press the trigger key repeatedly from a closed menu, logging where each press lands, then
/// probe the softkeys of the page that is showing.
fn walk_trigger_key(scope: &Device) -> Result<()> {
    // Reaching menu 11 needs a different route than the trigger key: turning the trigger
    // *level* knob pops up the trigger base page. That is the route the capture prepare
    // takes, which is why the app meets menu 11 and this probe otherwise never does.
    if std::env::args().any(|a| a == "--level") {
        for turn in 0..3 {
            scope.press(Key::TriggerLevelUp)?;
            sleep(SETTLE);
            let settings = scope.read_settings()?;
            println!(
                "[level] turn {turn}: menu {} bar {}  TRIG-VPOS {}",
                settings.menu_id(),
                settings.field("CONTROL-DISP-MENU").unwrap_or(0),
                settings.trigger_position()
            );
        }
    }
    for press in 0..(if std::env::args().any(|a| a == "--level") { 0 } else { 4 }) {
        let settings = scope.read_settings()?;
        println!(
            "[walk] press {press}: menu {} bar {}  TRIG-TYPE {:?}",
            settings.menu_id(),
            settings.field("CONTROL-DISP-MENU").unwrap_or(0),
            settings.field("TRIG-TYPE")
        );
        scope.press(Key::MenuTrigger)?;
        sleep(MENU_SETTLE);
    }
    let page = scope.read_settings()?.menu_id();
    println!("\n=== probing whatever is showing — menu {page} ===");
    for key in SOFTKEYS {
        if key == TYPE_KEY {
            println!("  {key:?}: (type selector — skipped)");
            continue;
        }
        // Deliberately no navigation: the point is to characterise *this* page.
        let mut before = scope.read_settings()?;
        let mut moves: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        for _ in 0..PRESSES {
            scope.press(key)?;
            sleep(SETTLE);
            let after = scope.read_settings()?;
            for (name, values) in diff(&before, &after) {
                moves.entry(name).or_default().push(values.1);
            }
            before = after;
        }
        let summary = if moves.is_empty() {
            "no field moved".to_string()
        } else {
            moves
                .iter()
                .map(|(name, values)| format!("{name} → {values:?}"))
                .collect::<Vec<_>>()
                .join("; ")
        };
        println!("  {key:?}: {summary}  [menu now {}]", scope.read_settings()?.menu_id());
    }
    Ok(())
}

/// Cycle the type softkey until `TRIG-TYPE` reads `code`, and report the menu it lands on.
fn goto_type(scope: &Device, code: u8) -> Result<u8> {
    open_trigger(scope)?;
    for _ in 0..=TYPES.len() * 2 {
        let settings = scope.read_settings()?;
        if settings.field("TRIG-TYPE") == Some(code as u64) {
            return Ok(settings.menu_id());
        }
        scope.press(TYPE_KEY)?;
        sleep(SETTLE);
    }
    Err(mso5202d::Error::Unexpected(format!(
        "TRIG-TYPE never reached {code}"
    )))
}

/// Every menu id belonging to the trigger tree (protocol.md §9.1).
///
/// Checking against this set rather than merely "some menu is open" matters: straight after
/// a Default Setup the scope is showing menu 25, which would otherwise be mistaken for a
/// trigger page and every softkey probed against the wrong menu.
/// The trigger pages whose `Fn1` is the type selector. A second page must never be accepted
/// here: `Fn1` means something else there, so cycling the type would press forever.
const FIRST_PAGES: [u8; 7] = [5, 6, 8, 11, 22, 24, 38];

/// Press the trigger key until one of the trigger pages is showing.
fn open_trigger(scope: &Device) -> Result<u8> {
    for _ in 0..6 {
        let menu = scope.read_settings()?.menu_id();
        if FIRST_PAGES.contains(&menu) {
            return Ok(menu);
        }
        scope.press(Key::MenuTrigger)?;
        sleep(MENU_SETTLE);
    }
    Err(mso5202d::Error::Unexpected(format!(
        "no trigger menu opened (showing {})",
        scope.read_settings()?.menu_id()
    )))
}

/// Put the scope on the page to be probed: the type's first page, or its second.
fn reach(scope: &Device, code: u8, page2: bool) -> Result<u8> {
    let menu = goto_type(scope, code)?;
    if !page2 {
        return Ok(menu);
    }
    // The page key can be dropped like any other, so a single press that did not land is
    // not evidence that the type lacks a second page.
    for _ in 0..4 {
        scope.press(PAGE_KEY)?;
        sleep(SETTLE);
        let next = scope.read_settings()?.menu_id();
        if next != menu {
            return Ok(next);
        }
    }
    Err(mso5202d::Error::Unexpected(format!(
        "type {code} has no second page (still on menu {menu})"
    )))
}

/// Press one softkey repeatedly from a page, reporting every field it moves.
fn probe_key(scope: &Device, type_code: u8, page: u8, key: Key, page2: bool) -> Result<()> {
    reach(scope, type_code, page2)?;

    let mut before = scope.read_settings()?;
    let mut moves: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    let mut pages = Vec::new();

    for _ in 0..PRESSES {
        scope.press(key)?;
        sleep(SETTLE);
        let after = scope.read_settings()?;

        for (name, values) in diff(&before, &after) {
            moves.entry(name).or_default().push(values.1);
        }
        let menu = after.menu_id();
        if menu != page && !pages.contains(&menu) {
            pages.push(menu);
        }
        before = after;
    }

    let summary = if moves.is_empty() {
        "no field moved".to_string()
    } else {
        moves
            .iter()
            .map(|(name, values)| format!("{name} → {values:?}"))
            .collect::<Vec<_>>()
            .join("; ")
    };
    let nav = if pages.is_empty() {
        String::new()
    } else {
        format!("  [menu → {pages:?}]")
    };
    println!("  {key:?}: {summary}{nav}");
    Ok(())
}

/// Press a softkey once, then turn the multipurpose knob, reporting what the knob moved.
///
/// This is how a scope exposes a continuous parameter: the softkey selects which one the
/// knob owns, and the knob sets it. Such a softkey moves nothing by itself, so the
/// press-only probe reports it as dead.
fn probe_knob(scope: &Device, type_code: u8, key: Key, page2: bool) -> Result<()> {
    const TURNS: usize = 3;

    reach(scope, type_code, page2)?;
    scope.press(key)?;
    sleep(SETTLE);

    let mut before = scope.read_settings()?;
    let mut moves: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for _ in 0..TURNS {
        scope.press(Key::MultiRight)?;
        sleep(SETTLE);
        let after = scope.read_settings()?;
        for (name, values) in diff(&before, &after) {
            moves.entry(name).or_default().push(values.1);
        }
        before = after;
    }

    let summary = if moves.is_empty() {
        "knob moved nothing".to_string()
    } else {
        moves
            .iter()
            .map(|(name, values)| format!("{name} → {values:?}"))
            .collect::<Vec<_>>()
            .join("; ")
    };
    println!("  {key:?} + knob: {summary}");
    Ok(())
}

/// Named fields whose value differs, as (name, (before, after)).
fn diff(before: &Settings, after: &Settings) -> Vec<(String, (i64, i64))> {
    SETTINGS_PARAMS
        .iter()
        .filter_map(|(name, _)| {
            let a = before.field(name)? as i64;
            let b = after.field(name)? as i64;
            // These move on their own as the scope acquires, independently of any key.
            let noisy = matches!(*name, "TRIG-FREQUENCY" | "TRIG-STATE" | "CONTROL-MENUID");
            (a != b && !noisy).then(|| ((*name).to_string(), (a, b)))
        })
        .collect()
}
