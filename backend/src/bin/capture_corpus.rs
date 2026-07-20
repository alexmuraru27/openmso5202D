//! Record a corpus of **triggered** captures for grading the decoders.
//!
//! Each case is one clean, self-contained transmission:
//!
//! 1. put the ESP32 generator in triggered mode, so the line is idle,
//! 2. configure the scope and **arm** a single sequence,
//! 3. tell the generator to send exactly N bytes,
//! 4. wait for the scope to trigger on the first edge and stop,
//! 5. save each channel to the card and read it back.
//!
//! The point is that the record then holds **exactly** the bytes we asked for, starting at
//! pattern index 0, with idle either side. That makes the expected sequence known, so a
//! decode can be graded byte-for-byte instead of by a heuristic — and it removes the two
//! things that spoil a free-running capture: starting mid-byte, and the record filling with
//! more data than it has resolution for.
//!
//! ```sh
//! cargo run --release -p mso5202d --bin capture_corpus -- --proto uart --freq 115200 --depth 40k
//! cargo run --release -p mso5202d --bin capture_corpus -- --all
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use mso5202d::control::{execute, Context, CsvSource, Op, ProgressEvent, StepState};
use mso5202d::Key;
use mso5202d::settings::{Probe, StoreDepth, TB_TO_NS};
use mso5202d::decoder::common;
use mso5202d::waveform;
use mso5202d::{logging, Device, Result};

/// Where the recorded captures land. Kept apart from the free-running corpus so the two
/// can be compared rather than one replacing the other.
const OUT_DIR: &str = "../scope_dump/decoder_corpus_triggered";

/// The generator control tool.
const ESPGEN: &str = "mso5202d_espgen.py";
const SCRIPTS_DIR: &str = "../scripts";

/// Pattern the generator sends.
///
/// PRBS rather than a ramp: every byte depends on its position, so a decode that is
/// shifted or has dropped a byte is immediately visible, whereas a shifted ramp still
/// looks like a ramp. The varied bit patterns also exercise long same-bit runs and fast
/// toggling that a monotonic ramp never produces.
const PATTERN: &str = "prbs";

/// The byte at `index` of the pattern, mirroring `patternByte()` in the generator
/// firmware exactly, including the wrapping 32-bit arithmetic.
///
/// This lives here rather than in the library because it describes the **test generator**,
/// not the instrument. The sequence it produces is written into the manifest alongside
/// each capture, so grading reads the ground truth from the corpus instead of recomputing
/// it — the corpus is then self-describing.
fn pattern_byte(index: u32) -> u8 {
    let mut x = index.wrapping_mul(2_654_435_761);
    x ^= x >> 15;
    x = x.wrapping_mul(2_246_822_519);
    x ^= x >> 13;
    x as u8
}

/// The `count` bytes of the pattern starting at `seed`, hex encoded for the manifest.
fn expected_hex(seed: u32, count: u32) -> String {
    (0..count)
        .map(|i| format!("{:02x}", pattern_byte(seed.wrapping_add(i))))
        .collect()
}

/// A distinct, reproducible start offset for a case.
///
/// Derived from the case identity so every case covers a different stretch of the pattern
/// — otherwise all captures begin with the same bytes and exercise nothing beyond them —
/// while re-recording the same case reproduces the same data for a clean regression diff.
fn case_seed(case: &Case) -> u32 {
    let key = case.key();
    // FNV-1a over the case key.
    let mut hash = 2_166_136_261u32;
    for byte in key.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    // Keep it well clear of the top of the u32 range so seed+burst never wraps.
    hash % 1_000_000
}

/// Fraction of the record a burst may occupy.
///
/// The scope places the trigger at the **centre** of the record, so only the second half
/// is available after the first edge fires it — the first half is pre-trigger idle.
/// Measured directly: a burst sized to 70 % of the whole record decoded to exactly half
/// its bytes. So the budget is 80 % of the post-trigger half, leaving a little idle tail
/// inside the window.
const RECORD_FILL: f64 = 0.5 * 0.8;

/// Trigger level, in 1/25-division units above centre.
///
/// Placed at mid-swing of the 3.3 V logic signal (≈1.6 V at 1 V/div = ~40 units): high
/// enough that idle-line noise never crosses it, so the armed scope waits quietly and
/// fires only on a real edge of the burst — never a premature false trigger.
const TRIGGER_POSITION: i64 = 40;

/// Channel scale: a 3.3 V logic signal at 1 V/div sits about 3.3 divisions tall.
const VOLTS_PER_DIV_MV: u32 = 1000;

/// One case to record.
#[derive(Debug, Clone, Copy)]
struct Case {
    proto: Proto,
    freq: u64,
    depth: StoreDepth,
    /// Target samples per bit — the resolution the decoder gets to work with.
    spc: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Proto {
    Uart,
    Spi,
    I2c,
}

impl Proto {
    fn name(self) -> &'static str {
        match self {
            Proto::Uart => "uart",
            Proto::Spi => "spi",
            Proto::I2c => "i2c",
        }
    }

    /// Bits on the wire per payload byte, which is what sets how much record a burst needs.
    fn bits_per_byte(self) -> f64 {
        match self {
            Proto::Uart => 10.0, // start + 8 data + stop
            Proto::Spi => 8.0,
            Proto::I2c => 9.0, // 8 data + ACK
        }
    }

    /// Channels the scope must capture.
    fn sources(self) -> &'static [CsvSource] {
        match self {
            Proto::Uart => &[CsvSource::Ch1],
            Proto::Spi | Proto::I2c => &[CsvSource::Ch1, CsvSource::Ch2],
        }
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

/// Samples in a record at each depth, and the factor by which the sample rate exceeds the
/// 200-samples-per-division screen rate.
fn record_shape(depth: StoreDepth) -> (u64, f64) {
    match depth {
        StoreDepth::K4 => (4_064, 1.0),
        StoreDepth::K40 => (40_064, 10.0),
        StoreDepth::K512 => (400_064, 100.0),
        StoreDepth::M1 => (800_064, 200.0),
        StoreDepth::Unknown(_) => (4_064, 1.0),
    }
}

impl Case {
    fn key(&self) -> String {
        format!(
            "{}/{}/{}/{}spc",
            self.proto.name(),
            self.freq,
            depth_name(self.depth),
            self.spc
        )
    }

    /// Timebase index giving the wanted samples per bit.
    ///
    /// The record samples at `200 × multiplier` points per division, so a sample interval
    /// of `1 / (freq × spc)` needs a time/div of `200 × multiplier / (freq × spc)`. The
    /// scope only offers a fixed ladder, so this snaps to the nearest rung — and the burst
    /// size is then derived from what was actually achievable, not what was asked for.
    fn timebase_index(&self) -> usize {
        let (_, multiplier) = record_shape(self.depth);
        let wanted_ns = 1e9 * 200.0 * multiplier / (self.freq as f64 * self.spc as f64);
        TB_TO_NS
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = (**a as f64 / wanted_ns).ln().abs();
                let db = (**b as f64 / wanted_ns).ln().abs();
                da.total_cmp(&db)
            })
            .map(|(i, _)| i)
            .unwrap_or(6)
    }

    /// Samples per bit the chosen timebase actually delivers.
    fn achieved_spc(&self) -> f64 {
        let (_, multiplier) = record_shape(self.depth);
        let tdiv_ns = TB_TO_NS[self.timebase_index()] as f64;
        let dt_ns = tdiv_ns / (200.0 * multiplier);
        1e9 / (self.freq as f64 * dt_ns)
    }

    /// How many bytes to send so the burst fills the intended share of the record.
    fn burst_bytes(&self) -> u32 {
        let (samples, _) = record_shape(self.depth);
        let per_byte = self.achieved_spc() * self.proto.bits_per_byte();
        let fits = (samples as f64 * RECORD_FILL / per_byte).floor();
        (fits.max(1.0) as u32).min(8192)
    }
}

fn main() {
    let _log = logging::init().expect("start logging");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cases = match select_cases(&args) {
        Ok(cases) => cases,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    println!("recording {} case(s) into {OUT_DIR}", cases.len());
    let mut recorded = Vec::new();
    let mut failed = Vec::new();

    // One connection for the whole run; a reset between cases would disturb the card.
    let mut scope = match Device::connect_without_reset() {
        Ok(scope) => scope,
        Err(e) => {
            eprintln!("cannot reach the scope: {e}");
            std::process::exit(1);
        }
    };

    // Drain the USB endpoint before the first transaction. The previous run ends by
    // clearing the card over the 0x43 shell channel, and if it exits with a shell reply
    // still queued in the kernel buffer, our first 0x53 read would pull that stale 0x43
    // frame instead — the "bad leader 0x43" that otherwise loses the first case of every
    // fresh run.
    scope.transport().resync();

    for (index, case) in cases.iter().enumerate() {
        println!(
            "\n[{}/{}] {}  tb={} ns/div  spc={:.1}  burst={} B",
            index + 1,
            cases.len(),
            case.key(),
            TB_TO_NS[case.timebase_index()],
            case.achieved_spc(),
            case.burst_bytes()
        );
        match record_case(&scope, case) {
            Ok(entry) => {
                println!("      ok — {}", entry.summary());
                recorded.push(entry);
                // Written after EVERY case, not once at the end: a capture whose
                // ground truth was never recorded is undecodable, so an interrupted
                // run must still leave everything it managed to record usable.
                write_manifest(&recorded);
            }
            Err(e) => {
                eprintln!("      FAILED: {e}");
                failed.push((case.key(), e.to_string()));
                // A single case failing must not end the run — skip to the next one.
                // First clear any half-stream off the link, then check the scope is
                // still there: a soft failure (a knob that would not converge) needs
                // only a resync, while an actual disconnect — the scope occasionally
                // reboots and returns at a NEW bus address — needs a full reconnect,
                // retried until it re-enumerates.
                scope.transport().resync();
                if scope.read_settings().is_err() && !recover(&mut scope) {
                    eprintln!("      scope did not come back — stopping");
                    break;
                }
            }
        }
    }

    // Every capture has been read back to disk by now, so the card copies are spent.
    // Clearing keeps the next run from filling it — a 512 K export is ~7.7 MB.
    if !recorded.is_empty() {
        println!("\nclearing exported CSVs off the card...");
        let quiet = |_: &ProgressEvent| {};
        let context = Context::new(&scope, &quiet);
        match execute(&context, &[Op::ClearCard]) {
            Ok(()) => println!("card cleared"),
            Err(e) => eprintln!("could not clear the card: {e}"),
        }
    }

    println!("\nrecorded {} case(s), {} failed", recorded.len(), failed.len());
    for (key, error) in &failed {
        println!("  {key}: {error}");
    }
}

/// A recorded case, for the manifest.
struct Recorded {
    case: Case,
    burst: u32,
    files: Vec<String>,
    samples: usize,
    /// Whether the transmission ended inside the capture window.
    fitted: bool,
    /// First channel's CSV, kept so a retry can measure the real bit period from it.
    first_csv: Option<String>,
    /// Pattern start offset used, so the manifest's expected bytes match.
    seed: u32,
}

impl Recorded {
    fn summary(&self) -> String {
        format!("{} sample(s), files: {}", self.samples, self.files.join(", "))
    }
}

/// Re-establish the connection after an error, waiting for the scope to re-enumerate.
///
/// Returns false if it never comes back.
fn recover(scope: &mut Device) -> bool {
    const ATTEMPTS: u32 = 20;
    for attempt in 0..ATTEMPTS {
        sleep(Duration::from_secs(3));
        match scope.reconnect(false) {
            Ok(()) => {
                if attempt > 0 {
                    println!("      reconnected after {}s", (attempt + 1) * 3);
                }
                return true;
            }
            Err(e) if attempt + 1 == ATTEMPTS => eprintln!("      reconnect failed: {e}"),
            Err(_) => {}
        }
    }
    false
}

/// Attempts allowed to find a burst that fits the record.
const FIT_ATTEMPTS: u32 = 4;

/// Shrink factor used only when the capture cannot be measured.
const FIT_SHRINK: f64 = 0.55;

/// Samples per bit measured from a capture's own edges.
///
/// Preferred over the generator's claimed rate, which can be badly wrong: the bit-banged
/// I²C reports 500 kHz while actually running near 160 kHz. Measuring turns a truncated
/// capture into an accurate burst size in one step, instead of halving blindly until it
/// happens to fit.
fn measured_samples_per_bit(csv: &str, proto: Proto) -> Option<f64> {
    let parsed = waveform::parse_csv(csv).ok()?;
    let volts = parsed.volts?;
    let logic = common::threshold_volts(&volts);
    match proto {
        // A clock line toggles twice per bit, so the typical gap is half a bit period.
        Proto::Spi | Proto::I2c => {
            let edges = common::edges(&logic);
            if edges.len() < 8 {
                return None;
            }
            let gaps: Vec<f64> = edges.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
            Some(common::percentile(&gaps, 50.0) * 2.0)
        }
        // On a data line the shortest run is one bit — but the raw minimum is destroyed
        // by a single-sample glitch, which is exactly what produced a nonsensical
        // "1 sample/bit" on a 512 K record. A low percentile of the run lengths is
        // robust: a real stream has many one-bit runs, so it lands on the bit period.
        Proto::Uart => {
            let edges = common::edges(&logic);
            if edges.len() < 8 {
                return None;
            }
            let runs: Vec<f64> = edges.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
            Some(common::percentile(&runs, 10.0))
        }
    }
    .filter(|spb| *spb >= 2.0) // anything shorter is noise, not a bit
}

/// Whether the captured activity ends comfortably inside the record.
///
/// The generator does not always achieve the rate it reports — the bit-banged I²C runs
/// well under its nominal frequency and still claims to have hit it — so a burst sized
/// from the requested rate can overrun the window and be cut off mid-transaction. Rather
/// than trusting the generator, this checks the capture itself: activity running to the
/// very end means the transmission was truncated.
fn fits_in_record(csv: &str) -> bool {
    let Ok(parsed) = waveform::parse_csv(csv) else {
        return true;
    };
    let Some(volts) = parsed.volts else {
        return true;
    };
    if volts.len() < 32 {
        return true;
    }
    let edges = common::edges(&common::threshold_volts(&volts));
    match edges.last() {
        Some(&last) => (last as f64) < 0.92 * volts.len() as f64,
        None => true, // nothing captured at all is a different failure
    }
}

/// Record one case end to end, shrinking the burst if it does not fit the window.
fn record_case(scope: &Device, case: &Case) -> Result<Recorded> {
    let mut burst = case.burst_bytes();
    for attempt in 0..FIT_ATTEMPTS {
        let recorded = record_once(scope, case, burst)?;
        if recorded.fitted || attempt + 1 == FIT_ATTEMPTS {
            if !recorded.fitted {
                println!("      note: burst still reaches the record end at {burst} B");
            }
            return Ok(recorded);
        }
        // Prefer re-deriving the burst from what the capture actually shows; fall back to
        // shrinking only when the capture is too sparse to measure.
        let measured = recorded
            .first_csv
            .as_deref()
            .and_then(|csv| measured_samples_per_bit(csv, case.proto));
        let smaller = match measured {
            Some(spb) => {
                let (samples, _) = record_shape(case.depth);
                let fits = samples as f64 * RECORD_FILL / (spb * case.proto.bits_per_byte());
                println!(
                    "      burst {burst} B overran the window — measured {spb:.0} samples/bit"
                );
                // Guarantee real progress: a measurement that barely moves would retry
                // forever at essentially the same size.
                let capped = (burst as f64 * 0.8).floor().max(1.0) as u32;
                (fits.floor().max(1.0) as u32).min(capped)
            }
            None => ((burst as f64) * FIT_SHRINK).floor().max(1.0) as u32,
        };
        println!("      retrying at {smaller} B");
        burst = smaller;
    }
    unreachable!("the loop returns on its last attempt")
}

/// Wait for the armed scope to be ready, release ONE burst, and wait for it to finish.
///
/// The whole point of the single burst is deep records: a second burst arriving while the
/// scope is mid-acquisition corrupts the record (it came back as 4 K instead of 512 K).
/// One burst is only safe because the trigger sits at mid-swing — the armed scope cannot
/// false-trigger on idle noise, so it waits quietly until the real edge arrives, however
/// long that is.
///
/// The scope arms only after filling its pre-trigger buffer (half the record), which at a
/// slow timebase is seconds; this waits for that, releases the burst, then waits out the
/// full acquisition — both scaled to the record's own timing.
///
/// If the burst is still somehow missed, the whole arm-and-fire is retried from a clean
/// re-arm — never by piling a second burst onto a live acquisition.
fn trigger_and_capture(scope: &Device, case: &Case, burst: u32, seed: u32) -> Result<()> {
    const ATTEMPTS: u32 = 3;

    let tdiv_s = TB_TO_NS[case.timebase_index()] as f64 * 1e-9;
    let record_s = 20.0 * tdiv_s;
    // Pre-trigger is half the record; wait a little longer to be sure the scope has armed.
    let ready_wait = Duration::from_secs_f64((0.5 * record_s + 0.4).min(8.0));
    // A deep record also has to be written out after acquisition, so allow generous margin.
    let capture_wait = Duration::from_secs_f64((record_s * 2.0 + 3.0).min(30.0));

    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            println!("      burst not captured — re-arming (attempt {})", attempt + 1);
            scope.press(Key::Single)?;
        }
        sleep(ready_wait);
        espgen(&["trigger", &burst.to_string(), &seed.to_string()])?;
        if wait_stopped(scope, capture_wait)? {
            return Ok(());
        }
    }
    Err(mso5202d::Error::Unexpected(format!(
        "no trigger after {ATTEMPTS} arm-and-fire attempts — the signal never fired the trigger"
    )))
}

/// Poll the scope until it reports a captured/stopped state, or the timeout elapses.
fn wait_stopped(scope: &Device, timeout: Duration) -> Result<bool> {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if scope.read_settings()?.trig_state().is_stopped() {
            return Ok(true);
        }
        sleep(Duration::from_millis(200));
    }
    Ok(false)
}

/// One capture attempt at a given burst size.
fn record_once(scope: &Device, case: &Case, burst: u32) -> Result<Recorded> {
    // Silence the line first, so arming sees an idle signal and the only edge in the
    // record is the burst we are about to release.
    espgen(&[
        "set",
        case.proto.name(),
        &case.freq.to_string(),
        "triggered",
    ])?;
    espgen(&["pattern", PATTERN])?;

    let report = |event: &ProgressEvent| {
        if let StepState::Failed { error } = &event.state {
            eprintln!("        {} failed: {error}", event.label);
        }
    };
    let context = Context::new(scope, &report);

    // Configure, then arm — and stop, because the burst has to be released while armed.
    let mut setup = vec![
        Op::DefaultSetup,
        Op::SetChannel { channel: 1, on: true },
        Op::SetProbe { channel: 1, probe: Probe::X1 },
        Op::SetVoltsPerDiv { channel: 1, millivolts: VOLTS_PER_DIV_MV },
    ];
    if case.proto.sources().len() > 1 {
        setup.push(Op::SetChannel { channel: 2, on: true });
        setup.push(Op::SetProbe { channel: 2, probe: Probe::X1 });
        setup.push(Op::SetVoltsPerDiv { channel: 2, millivolts: VOLTS_PER_DIV_MV });
    }
    setup.push(Op::SetTimePerDiv {
        nanoseconds: TB_TO_NS[case.timebase_index()],
    });
    setup.push(Op::SetTriggerLevel { position: TRIGGER_POSITION });
    setup.push(Op::SetDepth { depth: case.depth });
    setup.push(Op::ArmSingle);
    execute(&context, &setup)?;

    // Release the burst and wait for the trigger, re-sending if it was missed.
    //
    // At a slow timebase the scope is not armed the instant `ArmSingle` returns: it has to
    // fill its pre-trigger buffer (half the record) before it will accept a trigger, which
    // at 200 ms/div is seconds. A single burst released too early passes while the scope is
    // still filling, the line goes idle, and nothing ever triggers. Because every burst is
    // identical — same seed — re-sending is safe: whichever one lands while the scope is
    // armed fires it, and the captured record always starts at the seed.
    let seed = case_seed(case);
    trigger_and_capture(scope, case, burst, seed)?;

    let mut finish = Vec::new();
    for &source in case.proto.sources() {
        finish.push(Op::SaveCsv { source });
    }
    for &source in case.proto.sources() {
        finish.push(Op::Download { source });
    }
    execute(&context, &finish)?;

    // Write the downloaded CSVs where the corpus expects them.
    let dir = PathBuf::from(OUT_DIR)
        .join(case.proto.name())
        .join("waves")
        .join(depth_name(case.depth));
    std::fs::create_dir_all(&dir).ok();

    let outputs = context.outputs();
    let mut files = Vec::new();
    let mut samples = 0usize;
    let mut fitted = true;
    let mut first_csv = None;
    for file in &outputs.files {
        let Some(data) = &file.data else { continue };
        let text = String::from_utf8_lossy(data).into_owned();
        fitted &= fits_in_record(&text);
        if first_csv.is_none() {
            first_csv = Some(text);
        }
        let name = format!(
            "{}_{}hz_{}spc_{}.csv",
            case.proto.name(),
            case.freq,
            case.spc,
            file.source.name()
        );
        std::fs::write(dir.join(&name), data).ok();
        samples = samples.max(data.iter().filter(|&&b| b == b'\n').count());
        files.push(name);
    }
    Ok(Recorded {
        case: *case,
        burst,
        files,
        samples,
        fitted,
        first_csv,
        seed,
    })
}

/// Run the generator control tool.
fn espgen(args: &[&str]) -> Result<()> {
    let output = Command::new("python3")
        .arg(ESPGEN)
        .args(args)
        .current_dir(SCRIPTS_DIR)
        .output()
        .map_err(|e| {
            mso5202d::Error::Unexpected(format!("cannot run {ESPGEN}: {e}"))
        })?;
    if !output.status.success() {
        return Err(mso5202d::Error::Unexpected(format!(
            "{ESPGEN} {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Write each protocol's manifest, MERGING with what is already recorded.
///
/// A manifest entry is the only record of what the generator sent for a case — its burst
/// size and the exact bytes — so it must never be lost. Merging by case key means a
/// partial or resumed run (after a scope reboot, say) updates just the cases it recorded
/// and leaves every earlier one intact, and re-recording a case cleanly replaces it.
fn write_manifest(recorded: &[Recorded]) {
    use std::collections::BTreeMap;

    // Group the freshly recorded cases by protocol.
    let mut by_proto: BTreeMap<&str, Vec<&Recorded>> = BTreeMap::new();
    for entry in recorded {
        by_proto.entry(entry.case.proto.name()).or_default().push(entry);
    }

    for (proto, entries) in by_proto {
        let dir = PathBuf::from(OUT_DIR).join(proto).join("waves");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("manifest.json");

        // Start from the existing manifest, keyed by case, then overlay this run's cases.
        let mut cases: BTreeMap<String, serde_json::Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|v| v.as_array().cloned())
            .into_iter()
            .flatten()
            .filter_map(|entry| Some((manifest_key(&entry)?, entry)))
            .collect();

        for entry in entries {
            let value = serde_json::json!({
                "freq": entry.case.freq,
                "depth": depth_name(entry.case.depth),
                "spc": entry.case.spc,
                "achieved_spc": (entry.case.achieved_spc() * 10.0).round() / 10.0,
                "tb_ns": TB_TO_NS[entry.case.timebase_index()],
                "pattern": PATTERN,
                "seed": entry.seed,
                "burst": entry.burst,
                "rows": entry.samples,
                "files": entry.files,
                "expected": expected_hex(entry.seed, entry.burst),
            });
            cases.insert(manifest_key(&value).expect("just built"), value);
        }

        // Emit ordered by frequency, then depth, then samples-per-bit.
        let mut ordered: Vec<serde_json::Value> = cases.into_values().collect();
        ordered.sort_by_key(manifest_key);
        let text = serde_json::to_string_pretty(&serde_json::Value::Array(ordered))
            .expect("manifest serialises");
        if let Err(e) = std::fs::write(&path, text + "\n") {
            eprintln!("could not write {proto} manifest: {e}");
        }
    }
}

/// Stable ordering/identity key for a manifest entry.
fn manifest_key(entry: &serde_json::Value) -> Option<String> {
    let freq = entry["freq"].as_i64()?;
    let depth = entry["depth"].as_str()?;
    let spc = entry["spc"].as_i64()?;
    // Zero-pad so lexicographic order is numeric order.
    Some(format!("{freq:012}/{depth}/{spc:04}"))
}

/// Build the case list from the command line.
fn select_cases(args: &[String]) -> std::result::Result<Vec<Case>, String> {
    /// The frequencies the free-running corpus used, so the two are comparable.
    const UART_FREQS: [u64; 3] = [9_600, 115_200, 921_600];
    const SPI_FREQS: [u64; 4] = [10_000, 500_000, 2_000_000, 20_000_000];
    const I2C_FREQS: [u64; 3] = [10_000, 400_000, 1_000_000];
    const DEPTHS: [StoreDepth; 2] = [StoreDepth::K40, StoreDepth::K512];
    const SPCS: [u32; 2] = [12, 100];

    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let all = args.iter().any(|a| a == "--all");
    if !all && flag("--proto").is_none() {
        return Err("usage: capture_corpus --all | --proto <uart|spi|i2c> \
                    [--freq <hz>] [--depth <40k|512k>] [--spc <n>]"
            .into());
    }

    let wanted_proto = flag("--proto");
    let wanted_freq: Option<u64> = flag("--freq").and_then(|f| f.parse().ok());
    let wanted_depth = flag("--depth");
    let wanted_spc: Option<u32> = flag("--spc").and_then(|s| s.parse().ok());

    let mut cases = Vec::new();
    for (proto, freqs) in [
        (Proto::Uart, &UART_FREQS[..]),
        (Proto::Spi, &SPI_FREQS[..]),
        (Proto::I2c, &I2C_FREQS[..]),
    ] {
        if let Some(name) = &wanted_proto {
            if name != proto.name() {
                continue;
            }
        }
        for &freq in freqs {
            if wanted_freq.is_some_and(|f| f != freq) {
                continue;
            }
            for depth in DEPTHS {
                if wanted_depth
                    .as_deref()
                    .is_some_and(|d| d != depth_name(depth))
                {
                    continue;
                }
                for spc in SPCS {
                    if wanted_spc.is_some_and(|s| s != spc) {
                        continue;
                    }
                    // 20 MHz SPI at 100 spc is impossible: the requested sample rate is far
                    // past the ADC ceiling, so it clamps to the same ~10 samples/bit as the
                    // 12 spc case — a redundant point that only ever "fails" on the analog
                    // bandwidth limit. Keep the 12 spc case as the 20 MHz stress test; drop
                    // the 100 spc duplicate.
                    if proto == Proto::Spi && freq == 20_000_000 && spc == 100 {
                        continue;
                    }
                    cases.push(Case { proto, freq, depth, spc });
                }
            }
        }
    }
    if cases.is_empty() {
        return Err("no cases match those filters".into());
    }
    Ok(cases)
}

/// Keep the corpus path visible in `--help`-less usage errors.
#[allow(dead_code)]
fn out_dir() -> &'static Path {
    Path::new(OUT_DIR)
}
