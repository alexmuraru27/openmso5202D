//! Parsing of the scope's exported waveform CSVs.
//!
//! These are the files the instrument writes to the card on Save/Recall → CSV, and reading
//! one back ([`crate::Device::download`]) is the only route to a record longer than the
//! screen buffer. One source per file.
//!
//! Two layouts share the same header block:
//!
//! ```text
//! ANALOG (CH1/CH2)                LOGIC ANALYZER (LA)
//!   #timebase=<n>(ns)               #timebase=<n>(ns)
//!   ,#voltbase=<n>(mv/100)          ,#threshold=<n>(mv)
//!   #size=<N>                       #size=<N>
//!   <t_s>,<volts>                   <t_s>,<word>
//! ```
//!
//! Both header labels are misleading and the numbers are what count: `#timebase` is the
//! screen time/div in **picoseconds** and `#voltbase` is **µV/div**.

use crate::error::{Error, Result};

/// One parsed export.
#[derive(Debug, Clone)]
pub struct WaveformCsv {
    /// Sample timestamps, in seconds.
    pub time_s: Vec<f64>,
    /// Sample interval in seconds — the median step between timestamps.
    ///
    /// Taken from the data rather than the header: the header's timebase is a screen tag,
    /// not the record's sample rate, and deeper records sample faster.
    pub dt_s: Option<f64>,
    /// Row count declared by the `#size` header.
    pub size: Option<usize>,
    /// Screen time/div in picoseconds, from `#timebase`.
    pub timebase_ps: Option<i64>,
    /// Analog samples in volts — already scope-calibrated, so no counts conversion is
    /// needed. `None` for a logic-analyzer export.
    pub volts: Option<Vec<f64>>,
    /// Volts/division in millivolts, derived from `#voltbase`. Analog exports only.
    pub volts_per_div_mv: Option<f64>,
    /// Logic-analyzer samples, bit `N` being channel D`N`. `None` for an analog export.
    pub words: Option<Vec<u16>>,
    /// Logic threshold in millivolts. Logic-analyzer exports only.
    pub threshold_mv: Option<i64>,
}

impl WaveformCsv {
    /// Whether this export holds logic-analyzer data rather than an analog trace.
    pub fn is_logic(&self) -> bool {
        self.words.is_some()
    }

    /// Number of samples actually parsed.
    pub fn len(&self) -> usize {
        self.time_s.len()
    }

    /// Whether the export carries no samples.
    pub fn is_empty(&self) -> bool {
        self.time_s.is_empty()
    }
}

/// Parse an exported waveform CSV.
///
/// Tolerant of the surrounding formatting — headers are found by pattern anywhere in the
/// preamble, and body rows are read as `time,value` pairs — because the files come off an
/// embedded firmware whose exact spacing is not guaranteed.
pub fn parse_csv(text: &str) -> Result<WaveformCsv> {
    let mut timebase = None;
    let mut voltbase = None;
    let mut threshold = None;
    let mut size = None;
    let mut last_header = None;

    for (index, line) in text.lines().enumerate() {
        if let Some((name, value)) = parse_header(line) {
            match name {
                "timebase" => timebase = Some(value),
                "voltbase" => voltbase = Some(value),
                "threshold" => threshold = Some(value),
                "size" => size = Some(value as usize),
                _ => {}
            }
            last_header = Some(index);
        }
    }

    let body_start = last_header.map(|i| i + 1).unwrap_or(0);
    let mut time_s = Vec::new();
    let mut values = Vec::new();
    for line in text.lines().skip(body_start) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split(',').filter(|f| !f.trim().is_empty());
        let (Some(t), Some(v)) = (fields.next(), fields.next()) else {
            continue;
        };
        let (Ok(t), Ok(v)) = (t.trim().parse::<f64>(), v.trim().parse::<f64>()) else {
            continue;
        };
        time_s.push(t);
        values.push(v);
    }

    if time_s.is_empty() && size.unwrap_or(0) > 0 {
        return Err(Error::Unexpected(
            "waveform CSV declared rows but none could be parsed".into(),
        ));
    }

    let dt_s = median_step(&time_s);
    let is_logic = threshold.is_some();

    Ok(WaveformCsv {
        time_s,
        dt_s,
        size,
        timebase_ps: timebase,
        volts: (!is_logic).then(|| values.clone()),
        volts_per_div_mv: voltbase.map(|v| v as f64 / 1000.0),
        words: is_logic.then(|| {
            values
                .iter()
                .map(|v| v.round().clamp(0.0, u16::MAX as f64) as u16)
                .collect()
        }),
        threshold_mv: threshold,
    })
}

/// Extract a `#name=value` header, ignoring the trailing unit annotation.
fn parse_header(line: &str) -> Option<(&'static str, i64)> {
    const NAMES: [&str; 4] = ["timebase", "voltbase", "threshold", "size"];
    let hash = line.find('#')?;
    let rest = &line[hash + 1..];
    let name = NAMES.iter().find(|n| rest.trim_start().starts_with(**n))?;
    let after = rest.trim_start().get(name.len()..)?;
    let digits = after.trim_start_matches([' ', '=']);
    let end = digits
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && *c == '-')))
        .map(|(i, _)| i)
        .unwrap_or(digits.len());
    digits[..end].parse().ok().map(|v| (*name, v))
}

/// Median gap between consecutive timestamps — the record's true sample interval.
fn median_step(time_s: &[f64]) -> Option<f64> {
    if time_s.len() < 2 {
        return None;
    }
    let mut steps: Vec<f64> = time_s.windows(2).map(|w| w[1] - w[0]).collect();
    steps.sort_by(f64::total_cmp);
    Some(steps[steps.len() / 2])
}
