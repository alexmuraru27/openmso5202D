//! Scores every saved capture in the corpus against the generator's 0x00..0xFF ramp and
//! records the result, so any decoder change can be diffed for improvement or regression.
//!
//! Hardware-free — it runs entirely on the CSV corpus under `scope_dump/decoder_corpus`,
//! which the Python decoder scores too, so the two implementations are measured against
//! exactly the same captures.
//!
//! # Workflow
//!
//! Running the test rewrites `tests/decoder_scores.json`. That file is committed, so
//! **`git diff` after a run is the improvement/regression report** — per case and in the
//! summary totals. The run also prints the same comparison inline:
//!
//! ```sh
//! pnpm decoder:score                      # or the cargo line below
//! cargo test -p mso5202d --test decoder_corpus --release -- --ignored --nocapture
//! git diff backend/tests/decoder_scores.json
//! ```
//!
//! It is `#[ignore]`d, and **must be run with `--release`**: it reads 276 MB of captures
//! and decodes ~2 million samples per case, which takes about 20 seconds optimised and
//! over ten minutes unoptimised. Keeping it out of the default suite also means a routine
//! `cargo test` never rewrites a committed file.
//!
//! The `ramp` figure is what matters: the fraction of adjacent decoded bytes that step by
//! +1. A byte count alone says nothing, because a desynced decoder happily emits thousands
//! of wrong bytes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use mso5202d::decoder::uart::UartOptions;
use mso5202d::decoder::{common, i2c, spi, uart, values, Event, Kind};
use mso5202d::waveform;

/// Captures shared with the Python decoder's corpus scorer.
const CORPUS: &str = "../scope_dump/decoder_corpus";

/// The committed scoreboard; rewritten on every run.
const SCORES: &str = "tests/decoder_scores.json";

/// Protocols, in the order they are reported.
const PROTOCOLS: [&str; 3] = ["spi", "uart", "i2c"];

/// A protocol whose mean falls below this is broken, not merely worse.
const BROKEN_BELOW: f64 = 0.30;

/// Change in ramp ratio counted as a real move rather than noise.
const NOTABLE: f64 = 0.02;

/// One scored case.
#[derive(Debug, Clone, PartialEq)]
struct Score {
    proto: String,
    freq: i64,
    depth: String,
    spc: i64,
    ramp: f64,
    bytes: usize,
}

#[test]
#[ignore = "reads the 276 MB capture corpus; run with --release, see the module docs"]
fn score_the_decoder_corpus() {
    let corpus = Path::new(CORPUS);
    if !corpus.is_dir() {
        // The corpus is large; a checkout without it should not fail the suite.
        println!("corpus not found at {CORPUS} — skipping");
        return;
    }

    let previous = load_scores(Path::new(SCORES));
    let mut current: BTreeMap<String, Score> = BTreeMap::new();
    for proto in PROTOCOLS {
        current.extend(score_protocol(corpus, proto));
    }
    assert!(!current.is_empty(), "corpus present but no cases were scored");

    report(&current, &previous);
    write_scores(Path::new(SCORES), &current);

    for proto in PROTOCOLS {
        let cases: Vec<&Score> = current.values().filter(|s| s.proto == proto).collect();
        if cases.is_empty() {
            continue;
        }
        let mean = cases.iter().map(|s| s.ramp).sum::<f64>() / cases.len() as f64;
        assert!(
            mean >= BROKEN_BELOW,
            "{proto} mean ramp {mean:.3} is below {BROKEN_BELOW} — the decoder is broken, \
             not merely regressed"
        );
    }
}

/// Score every case of one protocol.
fn score_protocol(corpus: &Path, proto: &str) -> BTreeMap<String, Score> {
    let waves = corpus.join(proto).join("waves");
    let manifest_path = waves.join("manifest.json");
    let Ok(text) = fs::read_to_string(&manifest_path) else {
        println!("no manifest for {proto} — skipping");
        return BTreeMap::new();
    };
    let manifest: serde_json::Value =
        serde_json::from_str(&text).expect("manifest.json should be valid JSON");

    let mut out = BTreeMap::new();
    for case in manifest.as_array().into_iter().flatten() {
        let freq = case["freq"].as_i64().unwrap_or(0);
        let depth = case["depth"].as_str().unwrap_or("").to_string();
        let spc = case["spc"].as_i64().unwrap_or(0);

        // Channels come back in file order: CH1 then CH2.
        let mut channels: Vec<Channel> = Vec::new();
        for file in case["files"].as_array().into_iter().flatten() {
            let Some(name) = file.as_str() else { continue };
            let path = waves.join(&depth).join(name);
            if let Some(channel) = load_channel(&path) {
                channels.push(channel);
            }
        }
        if channels.is_empty() {
            continue;
        }

        let events = decode(proto, freq, &channels);
        // Score DATA bytes only. An I²C address is a decoded byte but it is not part of
        // the generator's ramp, so counting it would break one adjacency per transaction
        // and understate a decoder that is in fact perfect.
        let bytes: Vec<u8> = events
            .iter()
            .filter(|e| e.kind == Kind::Byte)
            .filter_map(|e| e.value)
            .collect();
        out.insert(
            format!("{proto}/{freq}/{depth}/{spc}spc"),
            Score {
                proto: proto.to_string(),
                freq,
                depth,
                spc,
                ramp: (common::ramp_ratio(&bytes) * 1000.0).round() / 1000.0,
                bytes: bytes.len(),
            },
        );
    }
    out
}

/// One capture channel: raw volts plus its thresholded logic trace.
struct Channel {
    volts: Vec<f64>,
    logic: Vec<bool>,
    dt_s: Option<f64>,
}

fn load_channel(path: &Path) -> Option<Channel> {
    let text = fs::read_to_string(path).ok()?;
    let parsed = waveform::parse_csv(&text).ok()?;
    let volts = parsed.volts?;
    let logic = common::threshold_volts(&volts);
    Some(Channel {
        volts,
        logic,
        dt_s: parsed.dt_s,
    })
}

/// Run the decoder for `proto` over the loaded channels.
///
/// For the two-wire protocols the channel labels can be imperfect — a capture is tagged by
/// the order its sources were saved — so both assignments are tried and whichever yields
/// more bytes wins. That stops a swapped clock and data line from silently scoring zero.
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
            let a = spi::decode(
                &first.logic,
                &second.logic,
                None,
                Some(&first.volts),
                options,
            );
            let b = spi::decode(
                &second.logic,
                &first.logic,
                None,
                Some(&second.volts),
                options,
            );
            better(a, b)
        }
        "i2c" => {
            let a = i2c::decode(&first.logic, &second.logic, i2c::Anchor::Auto);
            let b = i2c::decode(&second.logic, &first.logic, i2c::Anchor::Auto);
            better(a, b)
        }
        _ => Vec::new(),
    }
}

/// Keep whichever decode produced more byte-bearing events.
fn better(a: Vec<Event>, b: Vec<Event>) -> Vec<Event> {
    if values(&b).len() > values(&a).len() {
        b
    } else {
        a
    }
}

// --- reporting --------------------------------------------------------------

fn report(current: &BTreeMap<String, Score>, previous: &BTreeMap<String, Score>) {
    for proto in PROTOCOLS {
        let mut cases: Vec<(&String, &Score)> = current
            .iter()
            .filter(|(_, s)| s.proto == proto)
            .collect();
        if cases.is_empty() {
            continue;
        }
        cases.sort_by(|a, b| {
            (a.1.freq, &a.1.depth, -a.1.spc).cmp(&(b.1.freq, &b.1.depth, -b.1.spc))
        });

        println!("\n=== {} ===", proto.to_uppercase());
        println!("{:>9} {:>5} {:>4} {:>7} {:>6}   vs baseline", "rate", "depth", "spc", "bytes", "ramp");
        for (key, score) in &cases {
            let was = previous.get(*key).map(|p| p.ramp);
            println!(
                "{:>9} {:>5} {:>4} {:>7} {:>6.3}{}",
                score.freq,
                score.depth,
                score.spc,
                score.bytes,
                score.ramp,
                delta(score.ramp, was)
            );
        }
        summarise(&cases, previous);
    }

    let all: Vec<(&String, &Score)> = current.iter().collect();
    let mean = mean_ramp(&all);
    let shared: Vec<(&String, &Score)> = all
        .iter()
        .filter(|(k, _)| previous.contains_key(*k))
        .copied()
        .collect();
    println!("\n=== OVERALL ===");
    print!("mean decode {:.1}% over {} cases", mean * 100.0, all.len());
    if !shared.is_empty() {
        let base = shared
            .iter()
            .map(|(k, _)| previous[*k].ramp)
            .sum::<f64>()
            / shared.len() as f64;
        print!(
            "  |  baseline {:.1}%  ->  delta {:+.1} pts",
            base * 100.0,
            (mean_ramp(&shared) - base) * 100.0
        );
    }
    println!();

    let moved = |improving: bool| -> Vec<&str> {
        shared
            .iter()
            .filter(|(k, s)| {
                let d = s.ramp - previous[*k].ramp;
                if improving { d > NOTABLE } else { d < -NOTABLE }
            })
            .map(|(k, _)| k.as_str())
            .collect()
    };
    let improved = moved(true);
    let regressed = moved(false);
    if !improved.is_empty() {
        println!("improved ({}): {}", improved.len(), improved.join(", "));
    }
    if !regressed.is_empty() {
        println!("REGRESSED ({}): {}", regressed.len(), regressed.join(", "));
    }
    println!("\nscoreboard written to {SCORES} — `git diff` it to review the change");
}

fn summarise(cases: &[(&String, &Score)], previous: &BTreeMap<String, Score>) {
    let mean = mean_ramp(cases);
    let solid = cases.iter().filter(|(_, s)| s.ramp >= 0.99).count();
    let shared: Vec<(&String, &Score)> = cases
        .iter()
        .filter(|(k, _)| previous.contains_key(*k))
        .copied()
        .collect();
    print!(
        "  -> mean ramp {:.1}% | {}/{} at >=0.99",
        mean * 100.0,
        solid,
        cases.len()
    );
    if !shared.is_empty() {
        let base = shared.iter().map(|(k, _)| previous[*k].ramp).sum::<f64>() / shared.len() as f64;
        print!(
            "   (baseline {:.1}%, delta {:+.1} pts)",
            base * 100.0,
            (mean_ramp(&shared) - base) * 100.0
        );
    }
    println!();
}

fn mean_ramp(cases: &[(&String, &Score)]) -> f64 {
    if cases.is_empty() {
        return 0.0;
    }
    cases.iter().map(|(_, s)| s.ramp).sum::<f64>() / cases.len() as f64
}

fn delta(current: f64, previous: Option<f64>) -> String {
    match previous {
        None => "    (new)".into(),
        Some(was) if (current - was).abs() < 0.005 => format!("  ={was:.3}"),
        Some(was) => {
            let d = current - was;
            format!("  {}{d:+.3} (was {was:.3})", if d > 0.0 { "^" } else { "v" })
        }
    }
}

// --- persistence ------------------------------------------------------------

fn load_scores(path: &Path) -> BTreeMap<String, Score> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return BTreeMap::new();
    };
    json.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, v)| {
            Some((
                key.clone(),
                Score {
                    proto: v["proto"].as_str()?.to_string(),
                    freq: v["freq"].as_i64()?,
                    depth: v["depth"].as_str()?.to_string(),
                    spc: v["spc"].as_i64()?,
                    ramp: v["ramp"].as_f64()?,
                    bytes: v["bytes"].as_u64()? as usize,
                },
            ))
        })
        .collect()
}

fn write_scores(path: &Path, scores: &BTreeMap<String, Score>) {
    let object: serde_json::Map<String, serde_json::Value> = scores
        .iter()
        .map(|(key, s)| {
            (
                key.clone(),
                serde_json::json!({
                    "proto": s.proto,
                    "freq": s.freq,
                    "depth": s.depth,
                    "spc": s.spc,
                    "ramp": s.ramp,
                    "bytes": s.bytes,
                }),
            )
        })
        .collect();
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(object))
        .expect("scores serialise");
    if let Err(e) = fs::write(PathBuf::from(path), text + "\n") {
        println!("could not write {}: {e}", path.display());
    }
}
