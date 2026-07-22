//! Hardware smoke test for the prepare + capture workflow.
//!
//! Exercises the cohesive `control::capture` workflow against the real scope and checks the
//! three things the port has to get right: the store depth **cycles** to the requested value
//! (read back from the settings block after prepare), each requested channel's CSV **Source**
//! is selected and exported, and every export reads back and parses.
//!
//! ```sh
//! cargo run -p mso5202d --bin capture_test -- --depth 40k --channels 1,2 --tb-ns 2000
//! cargo run -p mso5202d --bin capture_test -- --depth 4k  --channels 1
//! ```

use std::time::Instant;

use mso5202d::control::{self, capture::CaptureSpec, Context, ProgressEvent, StepState};
use mso5202d::settings::StoreDepth;
use mso5202d::{logging, waveform, Device, Result};

fn main() {
    let _log = logging::init().expect("start logging");
    if let Err(e) = run() {
        eprintln!("\n[capture_test] FAILED: {e}");
        eprintln!("(is the scope plugged in? udev rule installed or running as root? SD card inserted?)");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    println!(
        "[capture_test] depth={} channels={:?} tb_ns={:?}",
        depth_name(args.depth),
        args.channels,
        args.timebase_ns
    );

    // Card-safe connect: a USB reset would disturb the scope's own USB host controller, and
    // the SD card hangs off it.
    let scope = Device::connect_without_reset()?;
    scope.transport().resync();
    if let Some((bus, address)) = scope.transport().bus_address() {
        println!("[capture_test] connected — bus {bus} address {address}");
    }

    // Prepare owns the trigger now: it begins with a Default Setup, so anything applied
    // beforehand would be wiped. Exercise a non-default one so the ordering is really tested.
    let spec = CaptureSpec {
        channels: args.channels.clone(),
        depth: args.depth,
        timebase_ns: args.timebase_ns,
        delete_after: args.clear,
        trigger: Some(mso5202d::control::trigger::TriggerSetup {
            source: mso5202d::control::trigger::TriggerSource::Ch2,
            coupling: mso5202d::control::trigger::TriggerCoupling::Ac,
            ..Default::default()
        }),
        ..Default::default()
    };

    // --- prepare -------------------------------------------------------------
    println!("\n[prepare]");
    let context = Context::new(&scope, &report);
    let t0 = Instant::now();
    control::capture::prepare(&context, &spec)?;
    println!("[prepare] done in {:.1}s", t0.elapsed().as_secs_f64());

    // The trigger must have survived the Default Setup that opens the plan.
    {
        let settings = scope.read_settings()?;
        match mso5202d::control::trigger::read(&settings) {
            Some(got) if got.source == mso5202d::control::trigger::TriggerSource::Ch2
                && got.coupling == mso5202d::control::trigger::TriggerCoupling::Ac =>
            {
                println!("[prepare] trigger applied after the reset: CH2 / AC");
            }
            other => {
                return Err(mso5202d::Error::Unexpected(format!(
                    "prepare did not leave the requested trigger in place: {other:?}"
                )))
            }
        }
    }

    // Verify the depth actually cycled to what we asked for — the headline check.
    let after = scope.read_settings()?;
    let got = after.store_depth();
    let depth_ok = got == args.depth;
    println!(
        "  depth read-back: {:?} {}",
        got,
        if depth_ok { "✓ matches request" } else { "✗ WRONG" }
    );
    for ch in [1u8, 2] {
        println!(
            "  CH{ch} shown={} {:?} mV/div probe={:?}",
            after.channel_shown(ch),
            after.volts_per_div_mv(ch),
            after.probe(ch),
        );
    }
    println!("  time/div {:?} ns   trig pos {}", after.time_per_div_ns(), after.trigger_position());
    if !depth_ok {
        return Err(mso5202d::Error::Unexpected(format!(
            "depth did not cycle: wanted {:?}, got {got:?}",
            args.depth
        )));
    }

    // --- capture -------------------------------------------------------------
    println!("\n[capture]");
    let t1 = Instant::now();
    control::capture::capture(&context, &spec)?;
    println!("[capture] done in {:.1}s", t1.elapsed().as_secs_f64());

    // --- results -------------------------------------------------------------
    println!("\n[results]");
    let outputs = context.outputs();
    if outputs.files.is_empty() {
        return Err(mso5202d::Error::Unexpected("no files were exported".into()));
    }
    let mut ok = true;
    for &ch in &args.channels {
        let source = match ch {
            1 => control::CsvSource::Ch1,
            2 => control::CsvSource::Ch2,
            _ => continue,
        };
        match outputs.file(source) {
            Some(file) => {
                let data_len = file.data.as_ref().map(|d| d.len()).unwrap_or(0);
                print!(
                    "  {} -> {} ({} B on card, {} B downloaded)",
                    file.source.name(),
                    file.name,
                    file.size,
                    data_len
                );
                match file.data.as_ref() {
                    Some(bytes) => match waveform::parse_csv(&String::from_utf8_lossy(bytes)) {
                        Ok(csv) => println!(
                            "  parsed {} samples, dt={}",
                            csv.len(),
                            csv.dt_s.map(|d| format!("{:.1} ns", d * 1e9)).unwrap_or("?".into())
                        ),
                        Err(e) => {
                            println!("  PARSE FAILED: {e}");
                            ok = false;
                        }
                    },
                    None => {
                        println!("  NOT downloaded");
                        ok = false;
                    }
                }
            }
            None => {
                println!("  CH{ch}: NO exported file — source cycling missed it");
                ok = false;
            }
        }
    }

    if !ok {
        return Err(mso5202d::Error::Unexpected(
            "one or more channels did not come back correctly".into(),
        ));
    }
    println!("\n[capture_test] PASS — depth cycled, sources exported, records read back");
    Ok(())
}

/// Print a progress event as one line.
fn report(event: &ProgressEvent) {
    match &event.state {
        StepState::Started => println!("  [{:>2}/{}] {}", event.index + 1, event.total, event.label),
        StepState::Completed { elapsed_ms } => println!("          ✓ {} ({elapsed_ms} ms)", event.label),
        StepState::Failed { error } => println!("          ✗ {} — {error}", event.label),
        StepState::Advanced { done, total } => println!("          … {} {done}/{total}", event.label),
    }
}

/// Command-line arguments.
struct Args {
    depth: StoreDepth,
    channels: Vec<u8>,
    timebase_ns: Option<u64>,
    clear: bool,
}

impl Args {
    fn parse() -> Self {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let flag = |name: &str| -> Option<String> {
            argv.iter().position(|a| a == name).and_then(|i| argv.get(i + 1)).cloned()
        };
        let depth = match flag("--depth").as_deref().unwrap_or("4k").to_ascii_lowercase().as_str() {
            "4k" => StoreDepth::K4,
            "40k" => StoreDepth::K40,
            "512k" => StoreDepth::K512,
            "1m" => StoreDepth::M1,
            other => {
                eprintln!("unknown --depth '{other}' (use 4k|40k|512k|1m)");
                std::process::exit(2);
            }
        };
        let channels = flag("--channels")
            .unwrap_or_else(|| "1".into())
            .split(',')
            .filter_map(|s| s.trim().parse::<u8>().ok())
            .collect::<Vec<_>>();
        let timebase_ns = flag("--tb-ns").and_then(|s| s.parse::<u64>().ok());
        let clear = argv.iter().any(|a| a == "--clear");
        Self { depth, channels, timebase_ns, clear }
    }
}

fn depth_name(depth: StoreDepth) -> &'static str {
    match depth {
        StoreDepth::K4 => "4k",
        StoreDepth::K40 => "40k",
        StoreDepth::K512 => "512k",
        StoreDepth::M1 => "1m",
        StoreDepth::Unknown(_) => "unknown",
    }
}
