//! The "hack main" — a scratch binary for driving the driver against real hardware while
//! building out higher layers. Nothing here is load-bearing; edit it freely to try
//! commands, dump frames, and probe the scope.
//!
//! Run it (needs the scope plugged in + udev rule or root):
//!
//! ```sh
//! cargo run -p mso5202d --bin playground
//! ```
//!
//! Every run appends a full trace to `logs/`; set `MSO_LOG=trace` to see it on the console
//! as well.

use mso5202d::control::{execute, Context, CsvSource, Op, ProgressEvent, StepState};
use mso5202d::settings::{Probe, StoreDepth};
use mso5202d::{logging, Device, Result};

fn main() {
    let _log = logging::init().expect("start logging");
    if let Err(e) = run() {
        eprintln!("\n[playground] error: {e}");
        eprintln!("(is the scope plugged in? is the udev rule installed, or are you root?)");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    println!("[playground] connecting to MSO5202D…");
    let scope = Device::connect()?;
    if let Some((bus, address)) = scope.transport().bus_address() {
        println!("[playground] connected — bus {bus} address {address}");
    }

    // --- what the scope currently thinks ------------------------------------
    let settings = scope.read_settings()?;
    println!("\n[settings]");
    println!("  menu        {:?}", settings.menu_name());
    println!("  trig state  {:?}", settings.trig_state());
    println!("  store depth {:?}", settings.store_depth());
    println!("  time/div    {:?} ns", settings.time_per_div_ns());
    for ch in [1u8, 2] {
        println!(
            "  CH{ch}         shown={} {:?} mV/div probe={:?}",
            settings.channel_shown(ch),
            settings.volts_per_div_mv(ch),
            settings.probe(ch),
        );
    }

    // --- run a plan ----------------------------------------------------------
    // A plan is plain data, so its shape — step count and labels — is known before
    // anything runs. That is what a progress bar needs.
    //
    // Add `Op::ClearCard` at the end to wipe every exported CSV off the card — it is
    // irreversible, so it is not here by default.
    let plan = vec![
        Op::DefaultSetup,
        Op::SetChannel { channel: 1, on: true },
        Op::SetProbe { channel: 1, probe: Probe::X1 },
        Op::SetVoltsPerDiv { channel: 1, millivolts: 1000 },
        Op::SetTimePerDiv { nanoseconds: 2000 },
        Op::SetDepth { depth: StoreDepth::K4 },
        Op::ArmSingle,
        Op::WaitCaptured { timeout_s: 20 },
        Op::SaveCsv { source: CsvSource::Ch1 },
        Op::Download { source: CsvSource::Ch1 },
    ];

    println!("\n[plan] {} operations", plan.len());
    for (i, op) in plan.iter().enumerate() {
        println!("  {:>2}. {}", i + 1, op.label());
    }

    println!("\n[running]");
    // A linear bar over index/total — exactly what a UI would draw.
    let report = |event: &ProgressEvent| {
        let percent = (event.index + 1) * 100 / event.total.max(1);
        match &event.state {
            StepState::Started => {
                println!("  [{:>2}/{}] {} …", event.index + 1, event.total, event.label)
            }
            StepState::Completed { elapsed_ms } => {
                println!("  {percent:>3}%    {} ✓ {elapsed_ms} ms", event.label)
            }
            StepState::Failed { error } => println!("         {} ✗ {error}", event.label),
            StepState::Advanced { done, total } => {
                println!("         {} {done}/{total}", event.label)
            }
        }
    };

    let context = Context::new(&scope, &report);
    match execute(&context, &plan) {
        Ok(()) => println!("\n[plan] completed"),
        Err(e) => println!("\n[plan] stopped: {e}"),
    }

    for file in &context.outputs().files {
        println!(
            "\n[output] {} -> {} ({} bytes on card, {} downloaded)",
            file.source.name(),
            file.name,
            file.size,
            file.data.as_ref().map(|d| d.len()).unwrap_or(0)
        );
        if let Some(data) = &file.data {
            let head = String::from_utf8_lossy(&data[..data.len().min(120)]);
            println!("  header: {}", head.lines().take(3).collect::<Vec<_>>().join(" | "));
        }
    }

    let after = scope.read_settings()?;
    println!("\n[after]");
    println!("  CH1 {:?} mV/div probe={:?}", after.volts_per_div_mv(1), after.probe(1));
    println!("  time/div {:?} ns", after.time_per_div_ns());
    println!("  depth {:?}", after.store_depth());
    println!("  trigger position {}", after.trigger_position());

    println!("\n[playground] done — full trace in logs/");
    Ok(())
}
