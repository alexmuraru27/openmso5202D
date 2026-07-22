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

use mso5202d::control::{self, Context, CsvSource, Op, ProgressEvent, ProgressSink};
use mso5202d::decoder::{self, i2c, spi, uart, Event, Kind};
use mso5202d::settings::{StoreDepth, TB_TO_NS};
use mso5202d::{waveform, CaptureSpec, Device};

/// Application state held across commands: the open device, the last prepared config, and
/// the traces currently on screen.
#[derive(Default)]
pub struct AppState {
    device: Mutex<Option<Device>>,
    prepared: Mutex<Option<CaptureConfig>>,
    /// The parsed records behind the current plot, kept so the decoder can be re-run against
    /// them when a decode setting changes — no re-capture, and no shipping megabytes of
    /// samples back through the IPC boundary just to annotate them differently.
    traces: Mutex<Option<LoadedTraces>>,
}

/// The records currently plotted, retained for re-decoding.
struct LoadedTraces {
    parsed: Vec<(u8, waveform::WaveformCsv)>,
    sample_interval_s: f64,
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

/// A waveform CSV sitting on the scope's memory card.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardFile {
    pub name: String,
    /// Size in bytes as the card reports it.
    pub size: u64,
}

/// One CSV to plot, and the channel it becomes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsvSlot {
    /// Channel this file is plotted as: 1 or 2.
    pub channel: u8,
    /// `"card"` — a filename on the scope's card, fetched over USB; `"local"` — a path on
    /// this machine, read straight off disk.
    pub source: String,
    /// The card filename or the local path, per `source`.
    pub value: String,
}

/// A card file after it has been pulled onto this machine.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadedFile {
    pub name: String,
    /// Absolute path it was written to on the host.
    pub path: String,
    pub bytes: u64,
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
    /// The raw byte value, for a byte/address event; `None` for a bus marker. Lets the UI
    /// show it in whatever base it likes (hex + decimal) without re-parsing `text`.
    pub value: Option<u8>,
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
    let parsed = parse_outputs(&context.outputs())?;
    retain_and_build(&state, parsed, &config)
}

// --- memory card --------------------------------------------------------------

/// Fallback location when the caller did not pick one (the UI normally supplies a path from
/// a native save dialog).
fn download_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join("openmso5202D")
}

/// The exported waveform CSVs currently on the scope's card.
#[tauri::command]
pub async fn list_card_files(state: State<'_, AppState>) -> Result<Vec<CardFile>, String> {
    let guard = state.device.lock().unwrap();
    let device = guard.as_ref().ok_or("scope not connected")?;
    // Start from a clean frame boundary; a previous transfer can leave frames queued.
    device.clear_link();
    let entries = device
        .list_dir(control::CARD_PATH)
        .map_err(|e| e.to_string())?;
    Ok(control::csv::wavedata_files(&entries)
        .into_iter()
        .map(|file| CardFile {
            name: file.name,
            size: file.size,
        })
        .collect())
}

/// Pull the named CSVs off the card onto this machine.
///
/// `dest` is what the UI's native dialog returned: for a **single** file the full target
/// path (so the user names it where they like), for **several** the directory to fill. With
/// no `dest` it falls back to [`download_dir`].
///
/// Streams `card:progress` so a multi-megabyte deep export (512 K ≈ 7.7 MB, 1 M ≈ 15 MB)
/// shows movement rather than appearing to hang. `async` so the transfer runs off the UI
/// thread.
#[tauri::command]
pub async fn download_card_files(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    names: Vec<String>,
    dest: Option<String>,
) -> Result<Vec<DownloadedFile>, String> {
    let guard = state.device.lock().unwrap();
    let device = guard.as_ref().ok_or("scope not connected")?;

    // One file + a destination means "save exactly here"; anything else treats it as a
    // directory to drop the files into under their own names.
    let chosen = dest.map(std::path::PathBuf::from);
    let single_target = chosen.clone().filter(|_| names.len() == 1);
    let dir = match (&single_target, &chosen) {
        (Some(path), _) => path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(download_dir),
        (None, Some(path)) => path.clone(),
        (None, None) => download_dir(),
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    // Sizes up front so progress can be reported against a real total.
    let listing = device
        .list_dir(control::CARD_PATH)
        .map_err(|e| e.to_string())?;
    let size_of = |name: &str| -> u64 {
        listing
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.size)
            .unwrap_or(0)
    };

    let mut saved = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let expected = size_of(name);
        let remote = format!("{}/{name}", control::CARD_PATH);
        let label = format!("Downloading {name}");
        let data = device
            .download_with(&remote, |done| {
                let _ = app.emit(
                    "card:progress",
                    ProgressPayload {
                        index,
                        total: names.len(),
                        label: label.clone(),
                        state: "advanced".into(),
                        fraction: if expected > 0 {
                            (index as f32 + done as f32 / expected as f32) / names.len() as f32
                        } else {
                            index as f32 / names.len() as f32
                        },
                        detail: Some(format!("{done}/{expected} B")),
                    },
                );
            })
            .map_err(|e| format!("{name}: {e}"))?;

        let path = single_target.clone().unwrap_or_else(|| dir.join(name));
        std::fs::write(&path, &data).map_err(|e| format!("{}: {e}", path.display()))?;
        saved.push(DownloadedFile {
            name: name.clone(),
            path: path.display().to_string(),
            bytes: data.len() as u64,
        });
    }
    Ok(saved)
}

/// Fetch the named CSVs off the card and return them as a plottable result.
///
/// Each file holds **one** channel and nothing in it says which, so they are assigned to
/// channels **explicitly** — nothing in a CSV says which channel it came from, so the caller
/// states it. That is what makes a saved SPI/I²C pair decodable after the fact.
#[tauri::command]
pub async fn load_csvs(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    slots: Vec<CsvSlot>,
    config: CaptureConfig,
) -> Result<CaptureResult, String> {
    if slots.is_empty() {
        return Err("choose a file to load".into());
    }

    // Local files need no instrument at all, so the device lock is only taken when a card
    // file is actually being fetched — saved captures can be reviewed with the scope unplugged.
    let needs_scope = slots.iter().any(|slot| slot.source == "card");
    let guard = needs_scope.then(|| state.device.lock().unwrap());
    let device = match &guard {
        Some(held) => Some(held.as_ref().ok_or("scope not connected")?),
        None => None,
    };

    if let Some(dev) = device {
        dev.clear_link();
    }

    // Sizes up front, so a card transfer can report real progress.
    let listing = match device {
        Some(dev) => dev
            .list_dir(control::CARD_PATH)
            .map_err(|e| e.to_string())?,
        None => Vec::new(),
    };

    let total = slots.len();
    let mut parsed = Vec::new();
    for (index, slot) in slots.iter().enumerate() {
        let text = match slot.source.as_str() {
            "local" => std::fs::read_to_string(&slot.value)
                .map_err(|e| format!("{}: {e}", slot.value))?,
            "card" => {
                let dev = device.ok_or("scope not connected")?;
                let expected = listing
                    .iter()
                    .find(|f| f.name == slot.value)
                    .map(|f| f.size)
                    .unwrap_or(0);
                let label = format!("Loading {}", slot.value);
                let data = dev
                    .download_with(&format!("{}/{}", control::CARD_PATH, slot.value), |done| {
                        let _ = app.emit(
                            "card:progress",
                            ProgressPayload {
                                index,
                                total,
                                label: label.clone(),
                                state: "advanced".into(),
                                fraction: if expected > 0 {
                                    (index as f32 + done as f32 / expected as f32) / total as f32
                                } else {
                                    index as f32 / total as f32
                                },
                                detail: Some(format!("{done}/{expected} B")),
                            },
                        );
                    })
                    .map_err(|e| format!("{}: {e}", slot.value))?;
                String::from_utf8_lossy(&data).into_owned()
            }
            other => return Err(format!("unknown source '{other}'")),
        };

        let csv = waveform::parse_csv(&text).map_err(|e| format!("{}: {e}", slot.value))?;
        parsed.push((slot.channel, csv));
    }
    retain_and_build(&state, parsed, &config)
}

/// Delete **every** exported waveform CSV from the card.
///
/// Goes through the front-panel delete key (never a shell `rm`), and is irreversible — it
/// clears all `WaveData*.csv`, not just files this session made.
#[tauri::command]
pub async fn clear_card_files(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let guard = state.device.lock().unwrap();
    let device = guard.as_ref().ok_or("scope not connected")?;
    device.clear_link();
    let sink = EmitProgress {
        app: &app,
        event_name: "card:progress",
    };
    let context = Context::new(device, &sink);
    control::execute(&context, &[Op::ClearCard]).map_err(|e| e.to_string())
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
        let depth = parse_depth(&self.depth)?;
        // 1M is single-channel only — the deep record needs the whole acquisition memory, so
        // the scope cannot serve two channels at that depth (the F5 ring only reaches 1M with
        // CH2 off).
        if depth == StoreDepth::M1 && self.channels.len() > 1 {
            return Err("1M memory depth is single-channel only — select just one channel".into());
        }
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

/// Parse every downloaded channel of a capture into volts + its sample interval.
fn parse_outputs(
    outputs: &control::Outputs,
) -> Result<Vec<(u8, waveform::WaveformCsv)>, String> {
    let mut parsed: Vec<(u8, waveform::WaveformCsv)> = Vec::new();
    for file in &outputs.files {
        let Some(bytes) = &file.data else { continue };
        let channel = channel_of(file.source);
        let text = String::from_utf8_lossy(bytes);
        let csv = waveform::parse_csv(&text).map_err(|e| e.to_string())?;
        parsed.push((channel, csv));
    }
    Ok(parsed)
}

/// Build the plottable result and **retain the records**, so a later change to a decode
/// setting can re-annotate them ([`redecode`]) without capturing or transferring again.
fn retain_and_build(
    state: &State<'_, AppState>,
    parsed: Vec<(u8, waveform::WaveformCsv)>,
    config: &CaptureConfig,
) -> Result<CaptureResult, String> {
    let result = result_from_parsed(&parsed, config)?;
    *state.traces.lock().unwrap() = Some(LoadedTraces {
        parsed,
        sample_interval_s: result.sample_interval_s,
    });
    Ok(result)
}

/// Re-run the decoder over the traces already on screen.
///
/// This is what makes the protocol and line-assignment controls live: the samples never move,
/// only the annotation, so switching UART→SPI or swapping clock/data re-decodes instantly
/// instead of demanding another capture.
#[tauri::command]
pub async fn redecode(
    state: State<'_, AppState>,
    config: CaptureConfig,
) -> Result<Vec<DecodedItem>, String> {
    let guard = state.traces.lock().unwrap();
    let loaded = guard.as_ref().ok_or("nothing loaded to decode")?;
    Ok(decode(&config, &loaded.parsed, loaded.sample_interval_s))
}

/// Assemble the plottable result from already-parsed per-channel records.
///
/// Shared by a live capture and by viewing CSVs pulled off the card, so both go through the
/// same scaling and decode path.
fn result_from_parsed(
    parsed: &[(u8, waveform::WaveformCsv)],
    config: &CaptureConfig,
) -> Result<CaptureResult, String> {
    if parsed.is_empty() {
        return Err("no data to plot".into());
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

    let decoded = decode(config, parsed, sample_interval_s);
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
            value: e.value,
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
        "1m" => Ok(StoreDepth::M1),
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
