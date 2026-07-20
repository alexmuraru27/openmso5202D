//! The backend-API layer: the seam between the webview and the `mso5202d` driver.
//!
//! The UI never touches USB. It sends a [`CaptureConfig`], and this layer turns it into a
//! control-layer plan, runs it, reads the record back, decodes it, and returns plain data
//! the frontend can plot. Progress is streamed to the webview as events so a bar can move
//! while a multi-second capture runs.
//!
//! Two phases mirror the workflow the Python plotter uses:
//!
//! - [`prepare`] — the slow, one-time setup (reset, channels, scale, timebase, trigger,
//!   depth). Leaves the scope configured but not armed.
//! - [`capture`] — arm a single sequence, wait for the signal to trigger it, export each
//!   channel to the card, read it back, and decode.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};

use mso5202d::control::{self, Context, CsvSource, ProgressEvent, ProgressSink};
use mso5202d::decoder::{self, i2c, spi, uart, Event, Kind};
use mso5202d::settings::{StoreDepth, TB_TO_NS};
use mso5202d::{waveform, CaptureSpec, Device};

/// Application state held across commands: the open device and the last prepared config.
#[derive(Default)]
pub struct AppState {
    device: Mutex<Option<Device>>,
    prepared: Mutex<Option<CaptureConfig>>,
}

// --- data the UI sends and receives -----------------------------------------

/// Everything the UI chooses for a capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureConfig {
    /// Channels to acquire: `[1]`, `[2]`, or `[1, 2]`.
    pub channels: Vec<u8>,
    /// The fastest signal frequency to resolve, in hertz.
    pub max_freq_hz: f64,
    /// Target samples per signal period — the decoder's resolution.
    pub samples_per_cycle: f64,
    /// Memory depth: `"4k"`, `"40k"`, or `"512k"`.
    pub depth: String,
    /// Decoder to run: `"none"`, `"uart"`, `"spi"`, or `"i2c"`.
    pub protocol: String,
    /// Channel carrying the clock (SPI SCLK, I²C SCL). `None` for UART.
    pub clock_channel: Option<u8>,
    /// Channel carrying the data (UART line, SPI MOSI, I²C SDA).
    pub data_channel: Option<u8>,
}

/// Whether the scope is reachable, and where.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeStatus {
    pub connected: bool,
    pub location: Option<String>,
}

/// One captured channel, ready to plot.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelData {
    pub channel: u8,
    pub label: String,
    /// Sample values in volts.
    pub volts: Vec<f32>,
}

/// One decoded element, positioned in time.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodedItem {
    pub start_s: f64,
    pub end_s: f64,
    pub text: String,
    pub kind: String,
    /// Channel the badge should be drawn over (the data line).
    pub channel: u8,
}

/// The result of a capture: the traces plus the decode.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureResult {
    pub sample_interval_s: f64,
    pub channels: Vec<ChannelData>,
    pub decoded: Vec<DecodedItem>,
}

// --- progress streaming ------------------------------------------------------

/// A [`ProgressSink`] that forwards each event to the webview under `event_name`.
struct EmitProgress<'a> {
    app: &'a tauri::AppHandle,
    event_name: &'static str,
}

/// The progress payload the UI receives.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload {
    index: usize,
    total: usize,
    label: String,
    /// One of `started` / `advanced` / `completed` / `failed`.
    state: String,
    fraction: f32,
    detail: Option<String>,
}

impl ProgressSink for EmitProgress<'_> {
    fn report(&self, event: &ProgressEvent) {
        use mso5202d::control::StepState;
        let (state, detail, within) = match &event.state {
            StepState::Started => ("started", None, 0.0),
            StepState::Advanced { done, total } => (
                "advanced",
                Some(format!("{done}/{total}")),
                if *total > 0 { *done as f32 / *total as f32 } else { 0.0 },
            ),
            StepState::Completed { elapsed_ms } => ("completed", Some(format!("{elapsed_ms} ms")), 1.0),
            StepState::Failed { error } => ("failed", Some(error.clone()), 1.0),
        };
        // Linear over steps, nudged by any in-step sub-progress.
        let fraction = if event.total == 0 {
            0.0
        } else {
            (event.index as f32 + within) / event.total as f32
        };
        let _ = self.app.emit(
            self.event_name,
            ProgressPayload {
                index: event.index,
                total: event.total,
                label: event.label.clone(),
                state: state.into(),
                fraction,
                detail,
            },
        );
    }
}

// --- commands ----------------------------------------------------------------

/// Current connection status (does not attempt to connect).
#[tauri::command]
pub fn scope_status(state: State<'_, AppState>) -> ScopeStatus {
    let guard = state.device.lock().unwrap();
    ScopeStatus {
        connected: guard.is_some(),
        location: guard.as_ref().and_then(location_of),
    }
}

/// Connect to the scope (without a USB reset, so the SD card stays available).
///
/// `async` so Tauri runs it on a worker thread rather than the UI thread — the blocking
/// USB work would otherwise freeze the event loop, and the Wayland compositor kills an app
/// that stops answering its pings ("Lost connection to Wayland compositor").
#[tauri::command]
pub async fn connect_scope(state: State<'_, AppState>) -> Result<ScopeStatus, String> {
    let device = Device::connect_without_reset().map_err(|e| e.to_string())?;
    // Clear any stale reply a previous session left on the endpoint.
    device.transport().resync();
    let location = location_of(&device);
    *state.device.lock().unwrap() = Some(device);
    Ok(ScopeStatus {
        connected: true,
        location,
    })
}

/// Configure the scope for `config`. Slow; streams `prepare:progress` events.
///
/// `async` so the multi-second plan runs off the UI thread (see [`connect_scope`]).
#[tauri::command]
pub async fn prepare(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    config: CaptureConfig,
) -> Result<(), String> {
    config.validate()?;
    let spec = config.to_spec()?;
    let guard = state.device.lock().unwrap();
    let device = guard.as_ref().ok_or("scope not connected")?;

    let sink = EmitProgress {
        app: &app,
        event_name: "prepare:progress",
    };
    let context = Context::new(device, &sink);
    // The prepare half of the shared capture workflow: reset, channels, scale, timebase,
    // trigger level, depth — all key-only, closed-loop.
    control::capture::prepare(&context, &spec).map_err(|e| e.to_string())?;

    *state.prepared.lock().unwrap() = Some(config);
    Ok(())
}

/// Arm, wait for the signal to trigger, read the record back, and decode it.
///
/// Streams `capture:progress` events. Requires a prior [`prepare`]. `async` so the capture
/// (which can take tens of seconds) runs off the UI thread (see [`connect_scope`]).
#[tauri::command]
pub async fn capture(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<CaptureResult, String> {
    let config = state
        .prepared
        .lock()
        .unwrap()
        .clone()
        .ok_or("prepare the scope before capturing")?;
    let spec = config.to_spec()?;
    let guard = state.device.lock().unwrap();
    let device = guard.as_ref().ok_or("scope not connected")?;

    let sink = EmitProgress {
        app: &app,
        event_name: "capture:progress",
    };
    let context = Context::new(device, &sink);
    // The capture half of the shared workflow: arm a single sequence, wait (force after a
    // grace), export each channel to the card, read it back, and leave the scope live.
    control::capture::capture(&context, &spec).map_err(|e| e.to_string())?;

    // Bind so the `Ref` from `outputs()` drops before the local borrows it depends on.
    let result = build_result(&context.outputs(), &config);
    result
}

// --- plan construction -------------------------------------------------------

impl CaptureConfig {
    /// Reject an incoherent configuration early, with a message the UI can show.
    fn validate(&self) -> Result<(), String> {
        if self.channels.is_empty() {
            return Err("select at least one channel".into());
        }
        if self.max_freq_hz <= 0.0 || self.samples_per_cycle <= 0.0 {
            return Err("frequency and samples-per-cycle must be positive".into());
        }
        parse_depth(&self.depth)?;
        Ok(())
    }

    /// Turn the UI config into the driver's [`CaptureSpec`] — the single source of truth the
    /// backend `control::capture` workflow drives. The trace scale (1 V/div for 3.3 V logic),
    /// trigger level, and reset policy are the workflow's defaults; only the channels, depth,
    /// and timebase come from the UI.
    fn to_spec(&self) -> Result<CaptureSpec, String> {
        Ok(CaptureSpec {
            channels: self.channels.clone(),
            depth: parse_depth(&self.depth)?,
            timebase_ns: Some(self.timebase_ns()),
            ..CaptureSpec::default()
        })
    }

    /// The timebase to set, snapped to the scope's ladder.
    ///
    /// The driver's [`control::capture::deep_tdiv_for_bit`] gives the ideal time/div for the
    /// requested samples-per-cycle at `max_freq_hz`; this rounds toward the **faster** rung so
    /// the result meets or exceeds the requested resolution rather than falling short.
    fn timebase_ns(&self) -> u64 {
        let depth = parse_depth(&self.depth).unwrap_or(StoreDepth::K40);
        let bit_ns = 1e9 / self.max_freq_hz;
        let ideal = control::capture::deep_tdiv_for_bit(bit_ns, depth, self.samples_per_cycle);
        // Largest rung not exceeding the ideal → at least the requested resolution.
        let index = TB_TO_NS
            .iter()
            .rposition(|&tb| tb as f64 <= ideal)
            .unwrap_or(0);
        TB_TO_NS[index]
    }
}

// --- decoding ----------------------------------------------------------------

/// Turn the downloaded records into plottable traces plus a decode.
fn build_result(
    outputs: &control::Outputs,
    config: &CaptureConfig,
) -> Result<CaptureResult, String> {
    // Parse every downloaded channel into volts + its sample interval.
    let mut parsed: Vec<(u8, waveform::WaveformCsv)> = Vec::new();
    for file in &outputs.files {
        let Some(bytes) = &file.data else { continue };
        let channel = channel_of(file.source);
        let text = String::from_utf8_lossy(bytes);
        let csv = waveform::parse_csv(&text).map_err(|e| e.to_string())?;
        parsed.push((channel, csv));
    }
    if parsed.is_empty() {
        return Err("capture produced no data".into());
    }

    let sample_interval_s = parsed
        .iter()
        .find_map(|(_, csv)| csv.dt_s)
        .unwrap_or(0.0);

    let channels: Vec<ChannelData> = parsed
        .iter()
        .map(|(channel, csv)| ChannelData {
            channel: *channel,
            label: format!("CH{channel}"),
            volts: csv
                .volts
                .as_ref()
                .map(|v| v.iter().map(|&x| x as f32).collect())
                .unwrap_or_default(),
        })
        .collect();

    let decoded = decode(config, &parsed, sample_interval_s);
    Ok(CaptureResult {
        sample_interval_s,
        channels,
        decoded,
    })
}

/// Run the selected decoder over the parsed channels.
fn decode(
    config: &CaptureConfig,
    parsed: &[(u8, waveform::WaveformCsv)],
    dt_s: f64,
) -> Vec<DecodedItem> {
    let logic = |channel: u8| -> Option<Vec<bool>> {
        let volts = parsed
            .iter()
            .find(|(c, _)| *c == channel)?
            .1
            .volts
            .as_ref()?;
        Some(decoder::common::threshold_volts(volts))
    };
    let raw = |channel: u8| -> Option<Vec<f64>> {
        parsed.iter().find(|(c, _)| *c == channel)?.1.volts.clone()
    };

    let (events, data_channel): (Vec<Event>, u8) = match config.protocol.as_str() {
        "uart" => {
            let line = config.data_channel.unwrap_or(1);
            let Some(trace) = logic(line) else { return Vec::new() };
            (
                uart::decode(
                    &trace,
                    uart::UartOptions {
                        sample_interval_ns: Some(dt_s * 1e9),
                        baud: Some(config.max_freq_hz),
                        ..Default::default()
                    },
                ),
                line,
            )
        }
        "spi" => {
            let (clk, data) = (config.clock_channel.unwrap_or(1), config.data_channel.unwrap_or(2));
            let (Some(clk_t), Some(data_t)) = (logic(clk), logic(data)) else {
                return Vec::new();
            };
            let analog = raw(clk);
            (
                spi::decode(&clk_t, &data_t, None, analog.as_deref(), spi::SpiOptions::default()),
                data,
            )
        }
        "i2c" => {
            let (scl, sda) = (config.clock_channel.unwrap_or(1), config.data_channel.unwrap_or(2));
            let (Some(scl_t), Some(sda_t)) = (logic(scl), logic(sda)) else {
                return Vec::new();
            };
            (i2c::decode(&scl_t, &sda_t, i2c::Anchor::Auto), sda)
        }
        _ => return Vec::new(),
    };

    events
        .iter()
        .map(|e| DecodedItem {
            start_s: e.start as f64 * dt_s,
            end_s: e.end as f64 * dt_s,
            text: e.text(),
            kind: kind_name(e.kind).to_string(),
            channel: data_channel,
        })
        .collect()
}

// --- small helpers -----------------------------------------------------------

fn location_of(device: &Device) -> Option<String> {
    device
        .transport()
        .bus_address()
        .map(|(bus, addr)| format!("bus {bus} address {addr}"))
}

fn parse_depth(depth: &str) -> Result<StoreDepth, String> {
    match depth.to_ascii_lowercase().as_str() {
        "4k" => Ok(StoreDepth::K4),
        "40k" => Ok(StoreDepth::K40),
        "512k" => Ok(StoreDepth::K512),
        other => Err(format!("unknown depth '{other}'")),
    }
}

fn channel_of(source: CsvSource) -> u8 {
    match source {
        CsvSource::Ch1 => 1,
        CsvSource::Ch2 => 2,
        CsvSource::La => 0,
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Byte => "byte",
        Kind::Address => "address",
        Kind::Start => "start",
        Kind::RepeatedStart => "repeated-start",
        Kind::Stop => "stop",
    }
}
