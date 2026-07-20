//! Grades the **triggered** capture corpus byte-for-byte against what the generator sent.
//!
//! Unlike the free-running corpus, each of these captures holds exactly one commanded
//! burst, starting at pattern index 0, with idle either side. The expected bytes are
//! therefore known exactly — so this compares against them directly instead of using the
//! ramp heuristic, and anything short of 100 % is a real decoding failure rather than an
//! artefact of where the capture happened to start.
//!
//! ```sh
//! cargo test -p mso5202d --test decoder_triggered --release -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use mso5202d::decoder::uart::UartOptions;
use mso5202d::decoder::{common, i2c, spi, uart, Event, Kind};
use mso5202d::waveform;

const CORPUS: &str = "../scope_dump/decoder_corpus_triggered";
const PROTOCOLS: [&str; 3] = ["uart", "spi", "i2c"];

#[test]
#[ignore = "reads the triggered capture corpus; run with --release"]
fn grade_the_triggered_corpus() {
    let corpus = Path::new(CORPUS);
    if !corpus.is_dir() {
        println!("no triggered corpus at {CORPUS} — record one with the capture_corpus binary");
        return;
    }

    let mut results: BTreeMap<String, (f64, usize, usize)> = BTreeMap::new();
    for proto in PROTOCOLS {
        let waves = corpus.join(proto).join("waves");
        let Ok(text) = fs::read_to_string(waves.join("manifest.json")) else {
            continue;
        };
        let manifest: serde_json::Value = serde_json::from_str(&text).expect("valid manifest");

        for case in manifest.as_array().into_iter().flatten() {
            let freq = case["freq"].as_i64().unwrap_or(0);
            let depth = case["depth"].as_str().unwrap_or("");
            let spc = case["spc"].as_i64().unwrap_or(0);
            // The bytes the generator actually sent, recorded when the capture was made.
            // Reading ground truth from the corpus rather than recomputing it keeps the
            // grader independent of how the pattern is generated.
            let expected = decode_hex(case["expected"].as_str().unwrap_or(""));

            let mut channels = Vec::new();
            for file in case["files"].as_array().into_iter().flatten() {
                let Some(name) = file.as_str() else { continue };
                if let Some(channel) = load(&waves.join(depth).join(name)) {
                    channels.push(channel);
                }
            }
            if channels.is_empty() {
                continue;
            }

            let decoded = data_bytes(&decode(proto, freq, &channels));
            let accuracy = accuracy(&decoded, &expected);
            results.insert(
                format!("{proto}/{freq}/{depth}/{spc}spc"),
                (accuracy, decoded.len(), expected.len()),
            );
        }
    }

    if results.is_empty() {
        println!("triggered corpus is empty");
        return;
    }

    println!("\n{:<28} {:>8} {:>9} {:>9}", "case", "decoded", "expected", "accuracy");
    let mut perfect = 0;
    for (key, (accuracy, got, want)) in &results {
        let mark = if *accuracy >= 0.9999 {
            perfect += 1;
            "OK"
        } else {
            "**"
        };
        println!("{key:<28} {got:>8} {want:>9} {:>8.1}% {mark}", accuracy * 100.0);
    }
    let mean = results.values().map(|(a, _, _)| a).sum::<f64>() / results.len() as f64;
    println!(
        "\nmean accuracy {:.1}%  |  {}/{} cases byte-perfect",
        mean * 100.0,
        perfect,
        results.len()
    );

    write_scoreboard(&results);
    println!("scoreboard written to {SCORES} — `git diff` it to review the change");
}

/// The committed scoreboard, rewritten each run so `git diff` is the regression report.
const SCORES: &str = "tests/decoder_scores_triggered.json";

/// Persist per-case accuracy so a decoder change shows up as a diff.
fn write_scoreboard(results: &BTreeMap<String, (f64, usize, usize)>) {
    let object: serde_json::Map<String, serde_json::Value> = results
        .iter()
        .map(|(key, (accuracy, decoded, expected))| {
            (
                key.clone(),
                serde_json::json!({
                    "accuracy": (accuracy * 1000.0).round() / 1000.0,
                    "decoded": decoded,
                    "expected": expected,
                }),
            )
        })
        .collect();
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(object))
        .expect("scoreboard serialises");
    if let Err(e) = fs::write(SCORES, text + "\n") {
        println!("could not write {SCORES}: {e}");
    }
}

/// Decode the manifest's hex-encoded expected byte sequence.
fn decode_hex(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

/// One capture channel.
struct Channel {
    volts: Vec<f64>,
    logic: Vec<bool>,
    dt_s: Option<f64>,
}

fn load(path: &Path) -> Option<Channel> {
    let parsed = waveform::parse_csv(&fs::read_to_string(path).ok()?).ok()?;
    let volts = parsed.volts?;
    let logic = common::threshold_volts(&volts);
    Some(Channel { volts, logic, dt_s: parsed.dt_s })
}

fn data_bytes(events: &[Event]) -> Vec<u8> {
    events
        .iter()
        .filter(|e| e.kind == Kind::Byte)
        .filter_map(|e| e.value)
        .collect()
}

fn decode(proto: &str, freq: i64, channels: &[Channel]) -> Vec<Event> {
    let first = &channels[0];
    let second = channels.get(1).unwrap_or(first);
    match proto {
        "uart" => uart::decode(
            &first.logic,
            UartOptions {
                sample_interval_ns: first.dt_s.map(|dt| dt * 1e9),
                baud: Some(freq as f64),
                ..Default::default()
            },
        ),
        "spi" => {
            let options = spi::SpiOptions::default();
            let a = spi::decode(&first.logic, &second.logic, None, Some(&first.volts), options);
            let b = spi::decode(&second.logic, &first.logic, None, Some(&second.volts), options);
            if data_bytes(&b).len() > data_bytes(&a).len() { b } else { a }
        }
        "i2c" => {
            let a = i2c::decode(&first.logic, &second.logic, i2c::Anchor::Auto);
            let b = i2c::decode(&second.logic, &first.logic, i2c::Anchor::Auto);
            if data_bytes(&b).len() > data_bytes(&a).len() { b } else { a }
        }
        _ => Vec::new(),
    }
}

/// Fraction of the expected bytes that were decoded correctly, at the best alignment.
///
/// The alignment search covers a capture that clipped a leading byte or picked up a
/// spurious one; without it a single extra byte at the front would score zero despite the
/// rest being perfect.
fn accuracy(decoded: &[u8], expected: &[u8]) -> f64 {
    if expected.is_empty() {
        return 0.0;
    }
    if decoded.is_empty() {
        return 0.0;
    }
    let search = decoded.len().min(expected.len()).min(16) as isize;
    let mut best = 0usize;
    for offset in -search..=search {
        let mut matched = 0usize;
        for (i, want) in expected.iter().enumerate() {
            let j = i as isize + offset;
            if j >= 0 && (j as usize) < decoded.len() && decoded[j as usize] == *want {
                matched += 1;
            }
        }
        best = best.max(matched);
    }
    best as f64 / expected.len() as f64
}
