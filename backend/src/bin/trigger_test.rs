//! Hardware check for trigger configuration.
//!
//! Applies a series of trigger setups and reads each one back out of the settings block, so
//! the softkey map is exercised end to end rather than trusted. Every case is a round trip:
//! ask for a configuration, then confirm the scope reports exactly that.
//!
//! ```sh
//! cargo run -p mso5202d --bin trigger_test
//! ```

use mso5202d::control::trigger::{
    self, AlterChannel, AlterType, Polarity, Qualifier, TriggerCoupling, TriggerMode,
    TriggerSetup, TriggerSource, TriggerType, VideoStandard, VideoSync,
};
use mso5202d::{logging, Device, Result};

fn main() {
    let _log = logging::init().expect("start logging");
    if let Err(e) = run() {
        eprintln!("\n[trigger_test] FAILED: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let scope = Device::connect_without_reset()?;
    scope.transport().resync();
    println!("[trigger_test] connected");

    let cases = [
        (
            "edge / CH1 / rising / auto / DC",
            TriggerSetup::default(),
        ),
        (
            "edge / CH2 / falling / normal / AC",
            TriggerSetup {
                source: TriggerSource::Ch2,
                polarity: Polarity::Negative,
                mode: TriggerMode::Normal,
                coupling: TriggerCoupling::Ac,
                ..Default::default()
            },
        ),
        (
            "edge / AC line — a source only Edge offers",
            TriggerSetup {
                source: TriggerSource::AcLine,
                ..Default::default()
            },
        ),
        (
            "pulse / CH1 / negative / when <",
            TriggerSetup {
                kind: TriggerType::Pulse,
                polarity: Polarity::Negative,
                qualifier: Qualifier::Less,
                coupling: TriggerCoupling::NoiseReject,
                ..Default::default()
            },
        ),
        (
            "slope / CH2 / negative / when =",
            TriggerSetup {
                kind: TriggerType::Slope,
                source: TriggerSource::Ch2,
                polarity: Polarity::Negative,
                qualifier: Qualifier::Equal,
                ..Default::default()
            },
        ),
        (
            "video / PAL / odd field / inverted",
            TriggerSetup {
                kind: TriggerType::Video,
                polarity: Polarity::Negative,
                video_standard: VideoStandard::PalSecam,
                video_sync: VideoSync::OddField,
                ..Default::default()
            },
        ),
        (
            "overtime / CH2 / negative",
            TriggerSetup {
                kind: TriggerType::Overtime,
                source: TriggerSource::Ch2,
                polarity: Polarity::Negative,
                mode: TriggerMode::Normal,
                ..Default::default()
            },
        ),
        (
            "back to edge / CH1",
            TriggerSetup::default(),
        ),
    ];

    let mut failures = 0;
    for (name, wanted) in cases {
        print!("[trigger_test] {name} … ");
        match trigger::apply(&scope, &wanted) {
            Ok(()) => match trigger::read(&scope.read_settings()?) {
                Some(got) => {
                    if got.matches(&wanted) {
                        println!("OK");
                    } else {
                        failures += 1;
                        println!("MISMATCH\n    wanted {wanted:?}\n    got    {got:?}");
                    }
                }
                None => {
                    failures += 1;
                    println!("could not read the trigger back");
                }
            },
            Err(e) => {
                failures += 1;
                println!("apply failed — {e}");
            }
        }
    }

    // The trigger level is meaningless in Slope — the knob is inert there — so a driver
    // must not try to converge on it. Setting a Slope trigger and then asking for a level is
    // the sequence that used to fail with a phantom end stop.
    print!("[trigger_test] slope ignores the trigger level … ");
    let slope = TriggerSetup {
        kind: TriggerType::Slope,
        ..Default::default()
    };
    match trigger::apply(&scope, &slope) {
        Ok(()) => {
            let before = scope.read_settings()?.trigger_position();
            for _ in 0..3 {
                scope.press(mso5202d::Key::TriggerLevelUp)?;
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
            let after = scope.read_settings()?.trigger_position();
            if before == after && !slope.kind.has_level() {
                println!("OK (TRIG-VPOS stayed at {before}, and has_level() says so)");
            } else {
                failures += 1;
                println!("TRIG-VPOS moved {before} → {after}; has_level() may be wrong");
            }
        }
        Err(e) => {
            failures += 1;
            println!("apply failed — {e}");
        }
    }

    // Alter runs a separate trigger per channel, each with its own sub-type and its own
    // page. Both channels are set in one apply and read back from `TRIG-SWAP-CHx-*`.
    for (name, ch1, ch2) in [
        (
            "alter: CH1 edge falling / AC, CH2 edge rising / DC",
            AlterChannel {
                polarity: Polarity::Negative,
                coupling: TriggerCoupling::Ac,
                ..Default::default()
            },
            AlterChannel::default(),
        ),
        (
            "alter: CH1 pulse when < / HF rej, CH2 video PAL odd field",
            AlterChannel {
                kind: AlterType::Pulse,
                qualifier: Qualifier::Less,
                coupling: TriggerCoupling::HighFrequencyReject,
                ..Default::default()
            },
            AlterChannel {
                kind: AlterType::Video,
                polarity: Polarity::Negative,
                video_standard: VideoStandard::PalSecam,
                video_sync: VideoSync::OddField,
                ..Default::default()
            },
        ),
        (
            "alter: CH1 overtime / noise rej, CH2 pulse when ≠",
            AlterChannel {
                kind: AlterType::Overtime,
                coupling: TriggerCoupling::NoiseReject,
                ..Default::default()
            },
            AlterChannel {
                kind: AlterType::Pulse,
                qualifier: Qualifier::NotEqual,
                coupling: TriggerCoupling::LowFrequencyReject,
                ..Default::default()
            },
        ),
    ] {
        print!("[trigger_test] {name} … ");
        let wanted = TriggerSetup {
            kind: TriggerType::Alter,
            alter_ch1: ch1,
            alter_ch2: ch2,
            ..Default::default()
        };
        match trigger::apply(&scope, &wanted) {
            Ok(()) => match trigger::read(&scope.read_settings()?) {
                Some(got) if got.matches(&wanted) => println!("OK"),
                Some(got) => {
                    failures += 1;
                    println!(
                        "MISMATCH\n    CH1 wanted {:?}\n    CH1 got    {:?}\n    CH2 wanted {:?}\n    CH2 got    {:?}",
                        wanted.alter_ch1, got.alter_ch1, wanted.alter_ch2, got.alter_ch2
                    );
                }
                None => {
                    failures += 1;
                    println!("could not read the trigger back");
                }
            },
            Err(e) => {
                failures += 1;
                println!("apply failed — {e}");
            }
        }
    }
    trigger::apply(&scope, &TriggerSetup::default())?;

    // Alter's own knob-only values: a channel on Pulse has a width, one on Overtime has a
    // hold time, each on its own channel page.
    {
        let wanted = TriggerSetup {
            kind: TriggerType::Alter,
            alter_ch1: AlterChannel { kind: AlterType::Pulse, ..Default::default() },
            alter_ch2: AlterChannel { kind: AlterType::Overtime, ..Default::default() },
            ..Default::default()
        };
        trigger::apply(&scope, &wanted)?;
        for what in [
            trigger::Adjustable::AlterCh1PulseWidth,
            trigger::Adjustable::AlterCh2OvertimeTime,
        ] {
            print!("[trigger_test] alter / {} nudges … ", what.label());
            let before = scope.read_settings()?.field_signed(what.field());
            match trigger::nudge(&scope, TriggerType::Alter, what, mso5202d::Turn::Up, 2) {
                Ok(after) if after.is_some() && after != before => {
                    println!("OK ({before:?} → {after:?})")
                }
                Ok(after) => {
                    failures += 1;
                    println!("did not move ({before:?} → {after:?})");
                }
                Err(e) => {
                    failures += 1;
                    println!("FAILED — {e}");
                }
            }
        }
        // A channel on Video has neither — and no coupling box either.
        print!("[trigger_test] alter / video channel offers no knob values … ");
        let video = TriggerSetup {
            kind: TriggerType::Alter,
            alter_ch1: AlterChannel { kind: AlterType::Video, ..Default::default() },
            ..Default::default()
        };
        if video.adjustables().contains(&trigger::Adjustable::AlterCh1PulseWidth) {
            failures += 1;
            println!("it claims one");
        } else {
            println!("OK");
        }
    }
    trigger::apply(&scope, &TriggerSetup::default())?;

    // Video's line number: a parameter only while Sync says LineNumber, and one the knob
    // already owns — pressing the Sync box to "select" it would cycle Sync away from
    // LineNumber and lose it.
    {
        print!("[trigger_test] video / line number nudges … ");
        let wanted = TriggerSetup {
            kind: TriggerType::Video,
            video_sync: VideoSync::LineNumber,
            ..Default::default()
        };
        trigger::apply(&scope, &wanted)?;
        let before = scope.read_settings()?.field_signed("TRIG-VIDEO-LINE");
        match trigger::nudge(
            &scope,
            TriggerType::Video,
            trigger::Adjustable::VideoLine,
            mso5202d::Turn::Up,
            2,
        ) {
            Ok(after) if after.is_some() && after != before => {
                let sync = scope.read_settings()?.field("TRIG-VIDEO-SYN");
                if sync == Some(1) {
                    println!("OK ({before:?} → {after:?}, Sync still LineNum)");
                } else {
                    failures += 1;
                    println!("line moved but Sync was knocked to {sync:?}");
                }
            }
            Ok(after) => {
                failures += 1;
                println!("did not move ({before:?} → {after:?})");
            }
            Err(e) => {
                failures += 1;
                println!("FAILED — {e}");
            }
        }

        print!("[trigger_test] video / no line number unless Sync is LineNum … ");
        let all_lines = TriggerSetup {
            kind: TriggerType::Video,
            video_sync: VideoSync::AllLines,
            ..Default::default()
        };
        if all_lines.adjustables().is_empty() {
            println!("OK");
        } else {
            failures += 1;
            println!("it offers {:?}", all_lines.adjustables());
        }
    }

    // Alter video's line number, on its own channel page.
    {
        print!("[trigger_test] alter / CH1 video line number nudges … ");
        let wanted = TriggerSetup {
            kind: TriggerType::Alter,
            alter_ch1: AlterChannel {
                kind: AlterType::Video,
                video_sync: VideoSync::LineNumber,
                ..Default::default()
            },
            ..Default::default()
        };
        trigger::apply(&scope, &wanted)?;
        let before = scope.read_settings()?.field_signed("TRIG-SWAP-CH1-VIDEO-LINE");
        match trigger::nudge(
            &scope,
            TriggerType::Alter,
            trigger::Adjustable::AlterCh1VideoLine,
            mso5202d::Turn::Up,
            2,
        ) {
            Ok(after) if after.is_some() && after != before => {
                println!("OK ({before:?} → {after:?})")
            }
            Ok(after) => {
                failures += 1;
                println!("did not move ({before:?} → {after:?})");
            }
            Err(e) => {
                failures += 1;
                println!("FAILED — {e}");
            }
        }
    }
    trigger::apply(&scope, &TriggerSetup::default())?;

    // Every continuous parameter, through the slot it actually lives in. These were the
    // ones a diff-only probe declared unreachable — the softkey that owns them changes no
    // field when pressed, it only hands the knob the value.
    for (kind, what) in [
        (TriggerType::Pulse, trigger::Adjustable::PulseWidth),
        (TriggerType::Slope, trigger::Adjustable::SlopeV1),
        (TriggerType::Slope, trigger::Adjustable::SlopeV2),
        (TriggerType::Slope, trigger::Adjustable::SlopeTime),
        (TriggerType::Overtime, trigger::Adjustable::OvertimeTime),
    ] {
        print!("[trigger_test] {} / {} nudges … ", kind.name(), what.label());
        let before = scope.read_settings()?.field_signed(what.field());
        match trigger::nudge(&scope, kind, what, mso5202d::Turn::Up, 2) {
            Ok(after) => {
                if after.is_some() && after != before {
                    println!("OK ({:?} → {:?})", before, after);
                } else {
                    failures += 1;
                    println!("did not move ({:?} → {:?})", before, after);
                }
            }
            Err(e) => {
                failures += 1;
                println!("FAILED — {e}");
            }
        }
    }

    // Overtime's coupling is the only box on its second page.
    print!("[trigger_test] overtime coupling (page 2) … ");
    let ot = TriggerSetup {
        kind: TriggerType::Overtime,
        coupling: TriggerCoupling::HighFrequencyReject,
        ..Default::default()
    };
    match trigger::apply(&scope, &ot) {
        Ok(()) => {
            let got = scope.read_settings()?.field("TRIG-COUP");
            if got == Some(u64::from(TriggerCoupling::HighFrequencyReject.code())) {
                println!("OK");
            } else {
                failures += 1;
                println!("TRIG-COUP is {got:?}, wanted HF reject");
            }
        }
        Err(e) => {
            failures += 1;
            println!("FAILED — {e}");
        }
    }

    // Driving a knob-only value to a target: fire the estimated number of presses, read
    // back, and make up any shortfall. The point is that it costs one walk, not one round
    // trip per step.
    {
        use std::time::Instant;
        let wanted = TriggerSetup { kind: TriggerType::Pulse, ..Default::default() };
        trigger::apply(&scope, &wanted)?;
        let from = scope.read_settings()?.field_signed("TRIG-PULSE-TIME").unwrap_or(0);
        // Two microseconds, a good way from wherever it starts.
        let target = 2_000_000i64;
        print!("[trigger_test] pulse width {from} → {target} ps … ");
        let started = Instant::now();
        match trigger::set_value(&scope, TriggerType::Pulse, trigger::Adjustable::PulseWidth, target)
        {
            Ok(landed) if landed == target => {
                let steps = (target - from).abs() / trigger::Adjustable::PulseWidth.step();
                println!(
                    "OK ({steps} steps in {:.1}s)",
                    started.elapsed().as_secs_f64()
                );
            }
            Ok(landed) => {
                failures += 1;
                println!("landed on {landed}, wanted {target}");
            }
            Err(e) => {
                failures += 1;
                println!("FAILED — {e}");
            }
        }
    }
    trigger::apply(&scope, &TriggerSetup::default())?;

    // Alter has a trigger level too — CH1's. It is shared with CH2 in the settings block,
    // so the read has to be taken while TRIG-SRC says CH1 or it reports the wrong channel.
    {
        print!("[trigger_test] alter level (CH1) … ");
        trigger::apply(
            &scope,
            &TriggerSetup {
                kind: TriggerType::Alter,
                alter_ch1: AlterChannel { kind: AlterType::Pulse, ..Default::default() },
                alter_ch2: AlterChannel { kind: AlterType::Overtime, ..Default::default() },
                ..Default::default()
            },
        )?;
        match trigger::set_level(&scope, 10) {
            Ok(10) => println!("OK (level 10 = 400 mV at 1 V/div)"),
            Ok(landed) => {
                failures += 1;
                println!("landed on {landed}, wanted 10");
            }
            Err(e) => {
                failures += 1;
                println!("FAILED — {e}");
            }
        }
    }
    trigger::apply(&scope, &TriggerSetup::default())?;

    // Applying a configuration the scope already holds must be a no-op, not an error. This
    // is the case that used to fail: with the type already correct no softkey was pressed,
    // the menu stayed on whatever page was open, and the page check rejected it.
    print!("[trigger_test] re-applying an unchanged trigger is a no-op … ");
    // Get back to the configuration first — the Slope case above left the scope elsewhere,
    // so timing an apply straight after it would time a real change.
    trigger::apply(&scope, &TriggerSetup::default())?;
    let started = std::time::Instant::now();
    match trigger::apply(&scope, &TriggerSetup::default()) {
        Ok(()) => {
            let elapsed = started.elapsed();
            // A real apply walks menus and presses keys; a no-op is one settings read.
            if elapsed < std::time::Duration::from_millis(500) {
                println!("OK ({} ms)", elapsed.as_millis());
            } else {
                failures += 1;
                println!("it took {} ms — it did work it should have skipped", elapsed.as_millis());
            }
        }
        Err(e) => {
            failures += 1;
            println!("FAILED — {e}");
        }
    }

    // The source it refuses is as much a part of the contract as the ones it takes.
    print!("[trigger_test] overtime rejects EXT as a source … ");
    let refused = trigger::apply(
        &scope,
        &TriggerSetup {
            kind: TriggerType::Overtime,
            source: TriggerSource::External,
            ..Default::default()
        },
    );
    match refused {
        Err(e) => println!("OK ({e})"),
        Ok(()) => {
            failures += 1;
            println!("UNEXPECTED — it was accepted");
        }
    }

    // Leave the scope on a sane trigger.
    trigger::apply(&scope, &TriggerSetup::default())?;

    if failures > 0 {
        return Err(mso5202d::Error::Unexpected(format!(
            "{failures} trigger case(s) failed"
        )));
    }
    println!("\n[trigger_test] all cases round-tripped");
    Ok(())
}
